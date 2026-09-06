# File upload as a native fastuse feature

Date: 2026-09-07
Status: approved design, not yet implemented

## Problem

An agent driving Windows through fastuse can see a "Choose file" button and
click it, but it has no reliable way to finish the job. Today the only route is
to screenshot the resulting dialog, guess where the filename field is, and type
a path one character at a time, which fails on localized dialogs, on multi-file
selection, and on any app whose drop zone has no file input at all.

Three distinct Windows mechanisms move a file into an application, and they need
different code:

- the native common dialog (`#32770`) that opens behind `<input type=file>` in
  every browser and behind every native app's Open/Save command;
- a clipboard paste of `CF_HDROP`, which Discord, Slack, Explorer, most Electron
  apps and many web drop zones accept;
- a real OLE drag-and-drop onto a drop zone that has no file input.

All three are in scope. They ship as three explicit tools rather than one tool
with an auto-detect strategy, because their inputs genuinely differ — a dialog
needs a target process and a submit, a paste needs a focused window, a drag
needs a destination coordinate — and because "which mechanism applies here" is
exactly the judgment the vision-first agent is already making from the
screenshot. An auto-detector would be a second, blinder guesser competing with
the one that can see.

## Verified facts this design rests on

Each of these was checked in this session rather than recalled.

The daemon runs at **High integrity**. Running `whoami /groups | findstr S-1-16-`
through the daemon's own `shell_exec`, so that it reports the daemon's token,
returns `Mandatory Label\High Mandatory Level  Label  S-1-16-12288`. This is the
single most consequential fact in the document; see the drag section.

In the installed `windows 0.62.2`, `FORMATETC` and `STGMEDIUM` are in
`Win32_System_Com`, which `fastuse-win` already enables. `DoDragDrop` and
`IDropSource_Impl` are in `Win32_System_Ole`, which is the one new Cargo feature
this work needs. `windows_core::implement` is exported unconditionally and needs
no feature flag.

`WindowInfo` in `fastuse-proto/src/coords.rs` already carries `class`, so dialog
discovery needs no new Win32 code. `clipboard/mod.rs` already owns a short-lived
STA helper thread and the `GlobalAlloc`/`GlobalLock` pattern. `wait_for_window`
already polls `list_windows` on a 50ms cadence. `wait_for_idle` already drains a
target's input queue.

## Tool surface

    file_dialog_set { paths, hwnd?, wait_for_dialog_ms = 5000,
                      wait_for_close_ms = 5000, submit = true }
    clipboard_set_files { paths, paste = false, hwnd? }
    drag_files { paths, x, y, start_x?, start_y? }

Each is a new `Request` variant in `fastuse-proto/src/wire.rs`, an arm in
`fastuse-daemon/src/dispatch.rs`, a module under `fastuse-win`, and an MCP tool
beside the existing ones. `clipboard_set_files` is the exception: it becomes a
`Files` variant on the existing `ClipboardSet` enum, so the existing permission
check applies to it without new wiring.

## Shared front half

All three share one `resolve_paths` helper, living beside the new modules in
`fastuse-win`, which canonicalizes every entry, stats it, and
fails the whole call if any path is missing or is a directory where a file is
required. This is not defensive padding. A nonexistent path makes a file dialog
silently refuse to close, which at the screenshot level is indistinguishable
from the app rejecting the file; it makes `CF_HDROP` produce a drop the target
quietly discards. Validating once, up front, collapses all three of those into a
single honest error.

File paths carry usernames, so the wire field is `paths: Redact<Vec<String>>`
and tracing spans record only a count and the resolved extensions. This is a
deliberate choice rather than a lint requirement: `xtask/src/check_redact.rs`
carries the suspect-name list `["payload", "text", "clipboard", "image_bytes",
"secret", "password"]`, and `paths` is not on it, so nothing would currently
force the wrapper. Add `paths` to that list as part of this work, so the
decision is enforced rather than remembered. Errors returned to the agent do include the
offending path: an agent that cannot see which path was wrong cannot fix it.

`clipboard_set_files` clobbers exactly the resource `clipboard_set_text` and
`clipboard_set_image` already clobber, so it joins the default gated list in
`docs/permissions.md`. `drag_files` presses the real mouse button and walks the
real cursor across the desktop, so it is gated too. `file_dialog_set` stays
ungated: it types into a dialog the user's own agent just caused to open, which
is no more privileged than the `computer` typing actions that are always
allowed.

## `file_dialog_set`

**Discovery.** Poll `list_windows` for `class == "#32770"` on the existing 50ms
cadence. Scope defaults to the foreground window's process at call time; passing
`hwnd` scopes to that window's process instead. A Chrome upload dialog and a
leftover dialog from an unrelated app are both `#32770`, and grabbing the wrong
one types a path into something the user cares about.

**Filling the field.** Not a `FindWindowEx` class walk. The Vista+ `IFileDialog`
filename field is an `Edit` nested inside a `ComboBoxEx32`, while the legacy
`GetOpenFileName` path is a bare `Edit`, and the walk differs between them. The
field is found through the existing selector engine as the dialog's `Edit`
descendant supporting `ValuePattern`, preferring the one labelled by the "File
name" static, and the value goes in through `ValuePattern::SetValue`, which is
atomic and does not depend on focus. Where UIA exposes no tree, fall back to
`WM_SETTEXT` on the deepest `Edit` in the class walk; where that also fails,
return a named error rather than typing into the void.

**Multiple files.** More than one path is joined in the quoted form,
`"a.png" "b.png"`. A single-select dialog treats that entire string as one
literal filename and refuses to close, so when the close wait expires with more
than one path the error says the dialog was single-select rather than reporting
a generic timeout.

**Submitting.** Enter into the field, not a click on the Open button: Enter is
invariant to the button's position and to the dialog's language, and it reuses
the input path everything else in fastuse uses. Fallback is a UIA `Invoke` on
the `IDOK` button. Passing `submit: false` fills the field and stops there,
leaving the dialog open for an agent that wants to change the file-type filter
or inspect the resolved selection before committing; it implies no close wait.

**Confirming.** The close wait watches the specific dialog HWND that was filled,
via `IsWindow` — not "any `#32770` is gone". Save dialogs stack a second
`#32770` for overwrite-confirm, and a design watching the class would report
success the instant focus moved to that confirm box. When the wait expires, the
result carries the dialog's own state plus any `#32770` that appeared after it,
so the agent can recognize an overwrite prompt and answer it. Auto-answering
overwrite prompts is out of scope: it is a destructive choice and it belongs to
the agent that can read the screen.

Both waits are individually switchable via `wait_for_dialog_ms` and
`wait_for_close_ms`, either set to zero to skip, for the Save dialog that
legitimately stays open or the app that stacks a second dialog deliberately.

**Runnable check.** Open Notepad, Ctrl+S, call the tool, assert the file exists
on disk.

## `clipboard_set_files`

A `DROPFILES` header — `pFiles` set to the header size, point zeroed, `fWide`
true — followed by the resolved paths as double-null-terminated UTF-16,
published as `CF_HDROP` on the existing STA helper thread.

Alongside it, publish the registered format `CFSTR_PREFERREDDROPEFFECT` holding
`DROPEFFECT_COPY`. This is not optional. Without it some targets, Explorer most
visibly, treat a pasted file list as a **move**, and the user's source file
disappears from its original location. A file-upload feature that silently
relocates the user's files is a data-loss bug; one `RegisterClipboardFormatW`
call and four bytes prevent it.

`paste: true` focuses the target window when `hwnd` is given, runs the existing
`wait_for_idle` drain, then sends Ctrl+V through the normal chord path. The
drain is what keeps Electron targets from swallowing the paste.

**Runnable check.** Read the clipboard back in-process with `DragQueryFileW` and
assert both the path round-trip and that the preferred-effect blob says copy.

## `drag_files`

### The integrity-level constraint

OLE drag-drop reverses the direction of the data flow: the drop target calls
back into the source's `IDataObject`. The daemon is High IL and Chrome and
Electron are medium, and a medium-IL target calling COM into a high-IL source is
the blocked direction — the same reason a file cannot be dragged from an
elevated Explorer into a normal application. A `DoDragDrop` written into the
daemon process would compile, run, and never drop a file into the browsers that
are the entire point of the feature.

A and B are unaffected, and for the opposite reason each time: `WM_SETTEXT` and
UIA into a dialog run high to low, which is allowed and is why the daemon
elevates at all, and the clipboard is not integrity-partitioned the way COM and
window messages are.

### Resolution: a de-elevated child of the same binary

`fastuse-daemon.exe --drag-helper`, re-executed at medium integrity, one
short-lived process per drag. It receives the resolved paths and the target
coordinate over the existing pipe protocol and exits when `DoDragDrop` returns.
The daemon still injects the mouse movement, which is the one part that needs
the elevated token.

This introduces a second hop the tool surface does not otherwise have: daemon to
helper, with its own framing and its own failure mode. `QueryContinueDrag`'s
deadline protects against a drag wedging *inside* the helper; it does nothing if
the helper is killed while holding the button down. So the daemon owns the
recovery, not the helper: it injects the button-down, and it injects a
button-up unconditionally when the helper's process handle signals or its own
deadline expires, whichever comes first. A drag that fails must never leave the
user's mouse button stuck.

De-elevation duplicates the daemon's own token, lowers its integrity label to
medium with `SetTokenInformation`, and spawns with `CreateProcessAsUser`. If
that misbehaves, the fallback is borrowing `explorer.exe`'s token via
`CreateProcessWithTokenW`. Which of the two actually works on this machine
deserves a ten-line spike before the implementation plan freezes.

Two alternatives were considered and rejected. Requiring a non-elevated daemon
for dragging is nearly free, but it forces the user to choose per session
between dragging files and driving elevated windows, and that choice will be
made wrong at the worst moment. Dropping C entirely and routing its use case
through the clipboard paste covers a large share of real drop zones at zero
cost, and remains the honest fallback if the helper turns out to be more
expensive than it looks — but it does not cover drop zones that ignore paste.

### Mechanics inside the helper

The helper creates a 1x1 layered tool window at the start point, styled
`WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` with alpha 1 rather than
0, because a fully transparent layered window can fall out of hit-testing. The
daemon injects a left-button-down at that point, so the click lands on our own
throwaway window and disturbs nothing on the user's desktop. `DoDragDrop` is
called from inside the `WM_LBUTTONDOWN` handler that receives it, not blindly
after the injection: that is the sequence the API is built around, and it makes
the mouse-capture handoff correct by construction.

`DoDragDrop` then blocks in its own modal loop while tracking the real cursor,
so the daemon walks the cursor to the destination in steps and releases.
`QueryContinueDrag` returns `DRAGDROP_S_DROP` on release and
`DRAGDROP_S_CANCEL` on Escape or on a deadline, so a drag can never wedge
holding the user's mouse button down. The data object serves `CF_HDROP` plus the
same preferred-effect blob as the clipboard path.

`start_x` and `start_y` default to a point on the same monitor as the
destination, away from the destination itself.

### Two honesty notes

`WM_DROPFILES` is the cheap trick people reach for here, and this design
deliberately does not build a fallback tier on it. Cross-process `HGLOBAL`
through a posted message is Win16 shared-memory legacy that works on some
targets and not others, and a fallback that silently does nothing is worse than
no fallback. If a cheap tier is wanted later, it needs its own probe first.

Unlike A and B, C has no meaningful in-process check. Its only real verification
is dropping a file into Chrome and watching the upload begin.

## Sequencing

A, then B, then C, each verified before the next begins. A and B each have a
real runnable check; C has only live verification, which is exactly why it must
not be stacked behind two unverified features. The integrity-level spike for the
de-elevation call happens before C's implementation is planned in detail.

## Out of scope

Auto-answering overwrite-confirmation dialogs. CDP or any browser-specific
upload path, which would contradict the vision-first substrate. A `WM_DROPFILES`
fallback tier. Uploading bytes supplied by the agent rather than paths on disk:
every mechanism here is path-based, and materializing agent-supplied bytes to a
temp file is a separate decision with its own lifetime and cleanup questions.
