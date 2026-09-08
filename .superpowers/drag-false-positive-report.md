# `drag_files` reports a successful drop but no file is copied

Status: **root cause confirmed by observation, fixed, verified live.**

Summary in one line: the drops were real and the report was truthful — the files
were delivered to a full-screen **Chrome** window that sat above the drop point,
not to the Explorer window the caller aimed at, and `DragResult` gave the caller
no way to see that.

## 1. Reproduction

The bug reproduced on the pristine `v1.0` binary once, at 07:22:51, into a
5-second-old Explorer window:

```
drag-files ...fu_probe_src.txt --x 730 --y 420
{"dropped":true,"effect":1,"ok":true}
ls "$TEMP/fu_probe/"    # empty
```

Daemon log for that run and the parent's four:

| time | drop point | dropped | effect | total_ms | file arrived |
|---|---|---|---|---|---|
| 07:18:41 | 704,341 | true | 1 | 227 | no |
| 07:19:55 | 875,373 | true | 1 | 231 | no |
| 07:20:18 | 875,373 | true | 1 | 223 | no |
| 07:20:22 | 875,373 | true | 1 | 192 | no |
| 07:22:51 | 730,420 | true | 1 | 225 | no |

Every later run — 40+ of them, across four daemon restarts, both instrumented
and pristine builds — succeeded, with `total_ms` between 449 and 1260. The
failing runs all fall in one contiguous window under a single daemon instance
and are ~200ms faster than any success. That gap turned out to be the tell:
Explorer's folder view services a real copy synchronously inside
`IDropTarget::Drop` and takes 250-700ms doing it; Chrome accepts a file drop,
opens it in a tab and returns almost immediately.

## 2. Hypotheses and how each was tested

Instrumentation used: an in-memory tracer in `drag_helper.rs` (flushed once, so
it does not perturb the timing it measures) logging `EnumFormatEtc`,
`QueryGetData`, `GetData`, every `QueryContinueDrag`, every change of the
`GiveFeedback` effect, `DoDragDrop` entry/return with the in and out effect,
`HdropData::drop`, and each teardown step; plus a daemon-side `WindowFromPoint`
at the drop coordinate. All of it was removed before the commit.

**H1 - data-object lifetime; the helper dies before Explorer reads the payload.**
Refuted. In every successful trace the entire target interaction happens
*inside* `DoDragDrop`: the post-drop `GetData(CF_HDROP)` burst ends ~16ms after
the button release, `DoDragDrop` returns several hundred ms later, and
`HdropData::drop` (COM refcount zero) fires immediately after. No `GetData` and
no release ever lands after `DoDragDrop` returns. Explorer's copy is
synchronous; there is no post-return window to lose.

**H2 - the button-down lands on the taskbar, not the 1x1 source window.**
Refuted directly: `pump: WM_LBUTTONDOWN received on the source window` appears
in every trace, and the helper only calls `DoDragDrop` from that message.

**H3 - the drop is serviced by a different target than the Explorer window we
aimed at.** **Confirmed.** See section 3.

**H4 - the ~200ms cursor walk is too fast for Explorer to register DragOver.**
Refuted: traces show 20 `QueryContinueDrag` calls and `GiveFeedback` moving
`0 -> 1 -> 0x80000001`, i.e. the target accepted with copy semantics well before
the release.

Two further mechanisms were tested and eliminated:

- **"No drop target under the cursor leaves `pdwEffect` at its in-value."** The
  helper passes `let mut effect = DROPEFFECT_COPY;` as the in/out `pdwEffect`,
  so a path where OLE never writes it would report `effect: 1` out of thin air.
  Tested by dropping on the taskbar (`Shell_TrayWnd`) and on a Calculator
  window: both returned `out-effect=0` and were correctly reported as
  `dropped: false, effect: 0`. OLE writes the value on every path exercised.
- **A starved helper reaching `DoDragDrop` after the button is already up.**
  Forced with a temporary 600ms sleep before `do_drag`. It does not produce a
  fast false drop - `DoDragDrop` blocks with no further mouse input and the call
  ends as `the drag helper reported no outcome within 11810ms`. Ruled out.

## 3. Confirmed root cause

The first `list-windows` of the session showed a Chrome window titled
**`fu_probe_src.txt - Google Chrome`**, bounds `(-8, -8, 1936x1048)` - full
screen - i.e. a Chrome tab displaying the very file being dragged. Chrome's
history database dates the visits:

```
2026-09-08 07:19:55.744385  file:///C:/Users/.../Temp/fu_probe_src.txt
2026-09-08 07:20:19.111175  file:///C:/Users/.../Temp/fu_probe_src.txt
2026-09-08 07:20:22.473363  file:///C:/Users/.../Temp/fu_probe_src.txt
2026-09-08 07:22:52.102404  file:///C:/Users/.../Temp/fu_probe_src.txt
```

Those four timestamps match four of the five failing `drag_files` outcomes to
the millisecond (`07:22:52.093` outcome / `07:22:52.102` visit, and so on). The
07:18:41 run has no matching visit; plausibly a collapsed duplicate visit, but
that is unverified and I am not claiming it.

So the files were dropped. They went to Chrome, which is a legitimate OLE drop
target: it accepts `CF_HDROP`, answers `DROPEFFECT_COPY`, opens the file in a
tab and copies nothing. `dropped: true, effect: 1` was **true**. The first
failing drop activated Chrome, which is why the next four failed the same way -
the failure was self-sustaining, which is exactly why it looked like three
identical reproductions.

Why Chrome was above a focused Explorer window is not fully pinned down (the
Chrome window is not `WS_EX_TOPMOST`; checked, ex-style `0x200100`). Once the
first drop raised Chrome it stayed in front, and the parent's `foreground-window`
check is not evidence against this: foreground and z-order at a given pixel are
different questions.

**The defect is therefore not in the OLE plumbing. It is that `DragResult` was
unfalsifiable.** A drag lands on whichever window is above the coordinate - the
same thing that happens to a human - and `dropped: true, effect: 1` cannot
distinguish "copied into the folder you were looking at" from "opened in a
browser tab that happened to be on top". OLE hands the source nothing beyond the
effect, so the source genuinely cannot know whether the target committed a copy.

## 4. The fix

`drag_files` now reports **which window received the drop**, so the caller can
check it against the window it aimed at.

- `fastuse-proto` - `DragResult` gains `drop_target: Option<WindowInfo>`, with
  `#[serde(default)]` so a version-skewed daemon/client pair still decodes (the
  `Eq` derive is dropped because `WindowInfo` is only `PartialEq`).
- `fastuse-win/src/files/drag.rs` - a new `window_at(x, y)`
  (`WindowFromPoint` + `GetAncestor(GA_ROOT)`) sampled **inside** the
  held-button scope, right after the cursor walk and before `ReleaseGuard`
  fires. Only the HWND is taken there; `build_window_info` (which costs a
  cross-process `WM_GETTEXT` with a 100ms timeout) runs after the release, so
  nothing new can stall with the user's mouse button down.
- `fastuse-cli`, `fastuse-mcp` - the field is surfaced, and the MCP tool
  description now tells the agent that `dropped: true` means *some* target
  accepted the files and that it must compare `drop_target` against the window
  it aimed at.

Why this and not something else: sampling *before* the release is load-bearing.
The drop activates the window that receives it, so a post-hoc probe names
whichever window the drop brought forward - the same answer whether or not it
was the intended one, which is no answer at all. And the reporting layer is the
right layer: `drag_files` takes a coordinate, not a window, so refusing to drop
when the coordinate is covered would break legitimate uses. Nothing in the
existing reporting was weakened - `dropped` and `effect` are unchanged and still
mean exactly what they meant.

Honest limit, unchanged by this fix: **the source still cannot know whether the
target committed the copy.** `dropped: true` means a drop target accepted the
payload with the reported effect. `drop_target` makes that claim checkable; it
does not make it a completion signal. Callers that need certainty must verify
the destination.

Two runnable checks were added in `drag.rs`:
`the_window_that_received_the_drop_is_reported_alongside_the_effect` (the field
survives into the result, and a bare point reports `None` rather than inventing
a window) and `window_at_reports_a_top_level_window` (GA_ROOT is applied and
idempotent).

## 5. Evidence

Five runs, each into a **freshly opened** Explorer window (new folder, new
window, 5s settle), new build:

| run | aimed hwnd | dropped | effect | file arrived | drop_target |
|---|---|---|---|---|---|
| 1 | 663938 | true | 1 | **yes** | hwnd 663938, explorer.exe, `fu_v1 - File Explorer` |
| 2 | 5183152 | true | 1 | **yes** | hwnd 5183152, explorer.exe, `fu_v2 - File Explorer` |
| 3 | 2625050 | true | 1 | **yes** | hwnd 2625050, explorer.exe, `fu_v3 - File Explorer` |
| 4 | 271218 | true | 1 | **yes** | hwnd 271218, explorer.exe, `fu_v4 - File Explorer` |
| 5 | 336472 | true | 1 | **yes** | hwnd 336472, explorer.exe, `fu_v5 - File Explorer` |

5/5 arrived, and `drop_target.hwnd` equals the aimed hwnd in every run.

Run 6 deliberately recreates the original failure - the full-screen Chrome
window raised over the drop point, then a drag aimed at the Explorer window
behind it:

```
aiming at Explorer hwnd=336772 point=(756,473)
foreground before drag: 6624724 chrome.exe
run=6 aimed_hwnd=336772 dropped=True effect=1
       drop_target: hwnd=6624724 process=chrome.exe title='fu_probe_src.txt - Google Chrome'
files in destination: 0
```

That is the bug as originally reported - `dropped: true, effect: 1`, nothing
copied - now with the receiving window named, and `drop_target.hwnd` visibly
different from the hwnd that was aimed at. This is the run that shows the fix
does something; the five clean runs only show nothing was broken.

An earlier attempt at run 6 was hijacked by a ShareX region-capture overlay that
grabbed the screen; the drop went to `ShareX.exe`, which refused it
(`dropped=False effect=0`, `drop_target: ShareX.exe`). Unplanned, but the same
point from the other direction.

Tests: `fastuse-win` 126 lib tests pass (one flake,
`clipboard::tests::text_round_trip`, fails only under parallel clipboard
contention and passes when run alone - unrelated to this change).
`fastuse-proto` 67 pass, `fastuse-mcp` 7 pass. `fastuse-daemon` has the four
documented pre-existing failures (`clipboard_get_image_requires_allow`,
`kill_authy_blocked_even_with_wildcard`, `launch_authy_blocked`,
`shell_exec_without_allow_returns_required`) and no new ones.

## 6. Loose ends

- The Chrome window titled `fu_probe_src.txt - Google Chrome` is still open. It
  was created by the original failing drags, not by me. I left it alone rather
  than close a browser window on the user's behalf; the tab now points at a
  deleted temp file.
- Scratch state cleaned: all `fu_*` temp folders and files removed, every
  Explorer window opened during this investigation closed.
