# File Upload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give fastuse three native tools that put a file into a Windows application — `file_dialog_set`, `clipboard_set_files`, and `drag_files`.

**Architecture:** Each tool is a `Request` variant in `fastuse-proto`, an arm in the daemon's `dispatch`, a module under `fastuse-win`, and an MCP tool plus a CLI subcommand. All three share one `resolve_paths` front half. `drag_files` alone cannot run in the daemon — the daemon is High integrity and OLE drop targets call back into the drag source — so it runs in a de-elevated child process re-executed from the same binary, with the daemon owning the mouse injection and the button-up recovery.

**Tech Stack:** Rust 1.83+, `windows 0.62.2` (UIA, Shell, Ole, Com), `uiautomation 0.24.4`, `rmcp 1.6`, `clap 4.6`, `tokio 1.49`.

**Spec:** `docs/superpowers/specs/2026-09-07-fastuse-file-upload-design.md`

## Global Constraints

- `fastuse-win` is the ONLY crate allowed to link `windows` / `uiautomation`. Everything else goes through it.
- `fastuse-win` has `#![deny(missing_docs)]`. Every new `pub` item needs a doc comment, or the crate will not build.
- UIA-12, enforced by `cargo xtask check-cacherequest`: no `Current[A-Z]*` accessor calls. Every UIA read sources from a `IUIAutomationCacheRequest` + `BuildUpdatedCache` pass. Reuse `uia::walk` / `uia::find` rather than reading properties directly.
- D-26: no tokio worker thread ever calls Windows API surface. UIA work goes through `uia_pool.run(...)`, input through `input.run(...)`, clipboard through the STA helper in `clipboard/mod.rs`.
- Payloads that can carry user data are wrapped in `Redact<T>`; `cargo xtask check-redact` enforces this for the field names listed in `xtask/src/check_redact.rs`.
- Windows x64 only. `crates/fastuse-win/src/lib.rs` has a `compile_error!` guarding this.
- Test command throughout: `cargo test -p <crate>`. E2E tests that need a live desktop are `#[ignore]`-gated and run with `-- --ignored`.

## Agent and Model Assignment

One implementer subagent per task, dispatched with the model named here. The rule behind the assignment: a task whose plan text already contains the code to write is transcription plus testing and takes the cheapest tier, while a task specified in prose — because writing invented Win32 or COM code into a plan would have been fabrication — needs a model that can read the installed crate source and decide. Reviews are separate dispatches and are listed alongside.

| Task | What it is | Implementer | Task reviewer |
|---|---|---|---|
| 1. Foundations | Complete code given, 4 small files | `haiku` | `sonnet` |
| 2. `file_dialog_set` core | Multi-file, three helpers specified in prose, UIA judgment | `opus` | `sonnet` |
| 3. `file_dialog_set` surface | Transcription into 3 files | `haiku` | `sonnet` |
| 4. `CF_HDROP` builder | Complete code given, 1 file, pure logic | `haiku` | `sonnet` |
| 5. `clipboard_set_files` | Real Win32 clipboard work, publish path in prose | `opus` | `sonnet` |
| 6. `clipboard_set_files` surface | Transcription into 3 files | `haiku` | `sonnet` |
| 7. De-elevation spike | Token APIs, two approaches, a judgment call | `opus` | none — the deliverable is a recorded answer, not code |
| 8. Drag helper | COM interface implementation, the hardest task here | `opus` | `opus` |
| 9. `drag_files` orchestration | Process lifecycle, the `Drop`-guard recovery | `opus` | `opus` |
| 10. `drag_files` surface | Transcription plus a coordinate-space check and live verification | `sonnet` | `sonnet` |

Fix rounds 4 and 5 escalate one tier above the implementer that got stuck, per the SDD skill. The final whole-branch review runs on `opus` regardless of what the individual tasks used.

Tasks 8 and 9 get `opus` on both seats because they are where a subtle mistake is invisible in a diff and expensive live: a reference-counting error in the `IDataObject` implementation, or a recovery path that lets an early return skip the button-up.

---

## File Structure

**New files**

- `crates/fastuse-win/src/files/mod.rs` — module root; owns `resolve_paths` and its Save-mode sibling `resolve_paths_allowing_new`, the shared front half for all three tools.
- `crates/fastuse-win/src/files/dialog.rs` — `file_dialog_set`: discovery, fill, submit, close-confirm.
- `crates/fastuse-win/src/files/hdrop.rs` — pure `CF_HDROP` byte-buffer construction. No Win32 calls, fully unit-testable.
- `crates/fastuse-win/src/files/drag.rs` — daemon-side drag orchestration: spawn helper, walk cursor, guarantee button-up.
- `crates/fastuse-win/src/files/deelevate.rs` — duplicate the daemon token, lower its integrity label, spawn a child.
- `crates/fastuse-daemon/src/drag_helper.rs` — the `--drag-helper` child mode: 1×1 window, `IDataObject`, `IDropSource`, `DoDragDrop`.
- `crates/fastuse-win/tests/hdrop_roundtrip.rs` — sets `CF_HDROP` and reads it back with `DragQueryFileW`.
- `crates/fastuse-daemon/tests/file_dialog_e2e.rs` — Notepad Save-dialog end-to-end.

**Modified files**

- `crates/fastuse-proto/src/error.rs` — five new `ErrorCode` variants.
- `crates/fastuse-proto/src/wire.rs` — three new `Request` variants, two new `Response` variants, a `Files` variant on `ClipboardSet`.
- `crates/fastuse-core/src/perm.rs` — two new `ToolPerm` rows.
- `crates/fastuse-win/src/lib.rs` — `pub mod files;`.
- `crates/fastuse-win/Cargo.toml` — three new `windows` features.
- `crates/fastuse-win/src/clipboard/mod.rs` — handle `ClipboardSet::Files`.
- `crates/fastuse-daemon/src/dispatch.rs` — three new arms.
- `crates/fastuse-daemon/src/main.rs` — `--drag-helper` branch.
- `crates/fastuse-mcp/src/handler.rs` — three new `#[tool]` methods plus their `Args` structs.
- `crates/fastuse-cli/src/main.rs` + `crates/fastuse-cli/src/cmd_phase4.rs` — three new subcommands.
- `xtask/src/check_redact.rs` — add `paths` to the suspect-name list.
- `docs/permissions.md` — document the two newly gated tools.

---

## Task 1: Shared foundations — error codes, `resolve_paths`, redact lint

**Files:**
- Modify: `crates/fastuse-proto/src/error.rs` (add to `ErrorCode` enum and `as_str`)
- Modify: `xtask/src/check_redact.rs:14`
- Create: `crates/fastuse-win/src/files/mod.rs`
- Modify: `crates/fastuse-win/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `fastuse_win::files::resolve_paths(raw: &[String]) -> Result<Vec<std::path::PathBuf>, fastuse_proto::Error>`, returning absolute paths with any `\\?\` prefix stripped. New codes `ErrorCode::{FileNotFound, DialogNotFound, DialogStillOpen, DragFailed, HelperSpawnFailed}`.

- [ ] **Step 1: Write the failing test**

Append to the bottom of the new `crates/fastuse-win/src/files/mod.rs` (create the file with just this for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_paths_rejects_empty() {
        let err = resolve_paths(&[]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
    }

    #[test]
    fn resolve_paths_rejects_missing_file() {
        let err = resolve_paths(&["Z:\\definitely\\not\\here.txt".to_string()]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
        // The offending path must be visible: an agent that cannot see which
        // path was wrong cannot fix it.
        assert!(err.message.contains("not\\here.txt"), "message was {}", err.message);
    }

    #[test]
    fn resolve_paths_rejects_directory() {
        let dir = std::env::temp_dir();
        let err = resolve_paths(&[dir.to_string_lossy().into_owned()]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
        assert!(err.message.contains("directory"), "message was {}", err.message);
    }

    #[test]
    fn resolve_paths_canonicalizes_and_strips_unc_prefix() {
        let p = std::env::temp_dir().join("fastuse_resolve_paths_test.txt");
        std::fs::write(&p, b"x").unwrap();
        let out = resolve_paths(&[p.to_string_lossy().into_owned()]).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(out.len(), 1);
        let s = out[0].to_string_lossy().into_owned();
        assert!(!s.starts_with(r"\\?\"), "UNC prefix leaked: {s}");
        assert!(s.ends_with("fastuse_resolve_paths_test.txt"), "got {s}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fastuse-win files::tests`
Expected: FAIL — the crate does not compile, because `resolve_paths` and `ErrorCode::FileNotFound` do not exist.

- [ ] **Step 3: Add the five error codes**

In `crates/fastuse-proto/src/error.rs`, add these variants at the end of the `ErrorCode` enum (before the closing brace, after `AmbiguousMatch`):

```rust
    /// A path passed to a file-upload tool is missing, unreadable, or is a
    /// directory where a file is required (v2.5.0).
    FileNotFound,
    /// `file_dialog_set` found no `#32770` common dialog within its wait
    /// budget (v2.5.0).
    DialogNotFound,
    /// `file_dialog_set` submitted the dialog but it was still open when the
    /// close wait expired — usually a wrong path, or a single-select dialog
    /// given several paths (v2.5.0).
    DialogStillOpen,
    /// `drag_files` completed without the target accepting a drop (v2.5.0).
    DragFailed,
    /// The de-elevated drag helper process could not be started (v2.5.0).
    HelperSpawnFailed,
```

And the matching arms in `ErrorCode::as_str`, after `Self::AmbiguousMatch => "AMBIGUOUS_MATCH",`:

```rust
            Self::FileNotFound => "FILE_NOT_FOUND",
            Self::DialogNotFound => "DIALOG_NOT_FOUND",
            Self::DialogStillOpen => "DIALOG_STILL_OPEN",
            Self::DragFailed => "DRAG_FAILED",
            Self::HelperSpawnFailed => "HELPER_SPAWN_FAILED",
```

- [ ] **Step 4: Write `resolve_paths`**

Put this at the top of `crates/fastuse-win/src/files/mod.rs`, above the `mod tests` block from Step 1:

```rust
//! File-upload primitives: the common dialog, `CF_HDROP` on the clipboard,
//! and OLE drag-drop.
//!
//! All three entry points share [`resolve_paths`], which is the only place
//! that decides whether a caller-supplied path is usable. Validating once, up
//! front, is not defensive padding: a nonexistent path makes a file dialog
//! silently refuse to close (indistinguishable at the screenshot level from
//! the app rejecting the file) and makes `CF_HDROP` produce a drop the target
//! quietly discards.

use std::path::PathBuf;

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// Canonicalize and validate every caller-supplied path.
///
/// Returns absolute paths with the `\\?\` verbatim prefix stripped.
/// `std::fs::canonicalize` on Windows always produces that prefix, and both
/// the common file dialog and `CF_HDROP` consumers mishandle it — the dialog
/// treats it as a literal filename and shell targets reject the drop.
///
/// Fails the whole call if any entry is missing, unreadable, or a directory.
pub fn resolve_paths(raw: &[String]) -> Result<Vec<PathBuf>, ProtoError> {
    if raw.is_empty() {
        return Err(ProtoError::new(
            ErrorCode::FileNotFound,
            "paths: at least one path is required".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        let canon = std::fs::canonicalize(p).map_err(|e| {
            ProtoError::new(ErrorCode::FileNotFound, format!("path {p}: {e}"))
        })?;
        let meta = std::fs::metadata(&canon).map_err(|e| {
            ProtoError::new(ErrorCode::FileNotFound, format!("path {p}: {e}"))
        })?;
        if meta.is_dir() {
            return Err(ProtoError::new(
                ErrorCode::FileNotFound,
                format!("path {p}: is a directory, expected a file"),
            ));
        }
        out.push(strip_verbatim_prefix(canon));
    }
    Ok(out)
}

/// Strip the `\\?\` verbatim prefix that `canonicalize` adds on Windows.
/// Leaves `\\?\UNC\server\share` paths alone — rewriting those to `\\server`
/// is a separate normalization and no caller needs it yet.
fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}
```

Then register the module in `crates/fastuse-win/src/lib.rs`, in the `pub mod` block, keeping alphabetical order (after `pub mod dpi;`):

```rust
pub mod files;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p fastuse-win files::tests`
Expected: PASS, 4 tests.

- [ ] **Step 6: Add `paths` to the redact lint's suspect-name list**

In `xtask/src/check_redact.rs:14`, extend the list:

```rust
    &["payload", "text", "clipboard", "image_bytes", "secret", "password", "paths"];
```

This is what makes the `Redact<Vec<String>>` decision in Task 2 enforced rather than remembered. File paths carry usernames.

- [ ] **Step 7: Verify the lint still passes on the current tree**

Run: `cargo run -p xtask -- check-redact`
Expected: PASS. No `Request` variant has a `paths` field yet, so adding the name changes nothing until Task 2.

- [ ] **Step 8: Commit**

```bash
git add crates/fastuse-proto/src/error.rs crates/fastuse-win/src/files/mod.rs crates/fastuse-win/src/lib.rs xtask/src/check_redact.rs
git commit -m "feat(win,proto): shared path resolution for file upload

resolve_paths canonicalizes, stats, and strips the \\?\ verbatim prefix that
both the common dialog and CF_HDROP consumers mishandle. Adds the five error
codes the three upload tools report, and puts 'paths' on the redact lint's
suspect-name list so the Redact wrapper is enforced."
```

---

## Task 2: `file_dialog_set` — wire types, win module, dispatch

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs` (`Request` enum, `Response` enum)
- Create: `crates/fastuse-win/src/files/dialog.rs`
- Modify: `crates/fastuse-win/src/files/mod.rs` (add `pub mod dialog;`)
- Modify: `crates/fastuse-daemon/src/dispatch.rs`

**Interfaces:**
- Consumes: `files::resolve_paths` from Task 1; `fastuse_win::window::list_windows::list_windows(process_name, title_substring, visible_only) -> Result<Vec<WindowInfo>, _>`; `uia_pool::UiaPoolHandle::run`; `input_thread::InputThreadHandle::run`.
- Produces: `Request::FileDialogSet { paths, hwnd, wait_for_dialog_ms, wait_for_close_ms, submit, opts }`, `Response::FileDialog(FileDialogResult)`, and `fastuse_win::files::dialog::file_dialog_set(...)`.

- [ ] **Step 1: Write the failing test**

Create `crates/fastuse-daemon/tests/file_dialog_e2e.rs`:

```rust
//! `file_dialog_set` end-to-end against Notepad's Save dialog.
//!
//! Run with: `cargo test -p fastuse-daemon --test file_dialog_e2e -- --ignored`
//! Requires a logged-in desktop session and a running daemon.

#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use fastuse_proto::{Redact, Request, Response};

mod harness;
use harness::*;

#[tokio::test]
#[ignore = "E2E — spawns notepad.exe; requires a running daemon"]
async fn file_dialog_set_saves_notepad_document() {
    let mut client = connect_or_skip().await;

    let target = std::env::temp_dir().join("fastuse_dialog_e2e.txt");
    std::fs::remove_file(&target).ok();

    let mut notepad = std::process::Command::new("notepad.exe")
        .spawn()
        .expect("spawn notepad");

    // Wait for the editor window, then type something so Save has content.
    let hwnd = wait_for_notepad(&mut client).await;
    client.call(Request::FocusWindow { hwnd, opts: None }).await.unwrap();
    client
        .call(Request::Type { text: Redact::new("upload plan".to_string()), opts: None })
        .await
        .unwrap();
    client
        .call(Request::Key { chord: "ctrl+s".to_string(), repeat: 1, opts: None })
        .await
        .unwrap();

    // The tool does the waiting: dialog appears, gets filled, submits, closes.
    let resp = client
        .call(Request::FileDialogSet {
            paths: Redact::new(vec![target.to_string_lossy().into_owned()]),
            hwnd: Some(hwnd),
            wait_for_dialog_ms: 5000,
            wait_for_close_ms: 5000,
            submit: true,
            opts: None,
        })
        .await
        .unwrap();

    match resp {
        Response::FileDialog(r) => assert!(r.closed, "dialog did not close: {r:?}"),
        other => panic!("unexpected response: {other:?}"),
    }

    assert!(target.exists(), "Notepad did not write {}", target.display());

    notepad.kill().ok();
    std::fs::remove_file(&target).ok();
}

async fn wait_for_notepad(client: &mut TestClient) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let r = client
            .call(Request::ListWindows {
                process_name: Some("notepad".into()),
                title_substring: None,
                visible_only: true,
            })
            .await
            .unwrap();
        if let Response::Windows(ws) = r {
            if let Some(w) = ws.first() {
                return w.hwnd;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("notepad window never appeared");
}
```

Verified against the existing harness while writing this plan: `crates/fastuse-daemon/tests/harness/mod.rs` exposes `pub struct TestClient` with `pub async fn call(&mut self, req: Request) -> std::io::Result<Response>`, and `pub async fn connect_or_skip() -> TestClient` (not a `Result`). `ListWindows` returns `Response::Windows(Vec<WindowInfo>)`, and `fastuse_proto::Error` has public `code`, `message`, and `hint` fields. The test above uses all four as written.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fastuse-daemon --test file_dialog_e2e -- --ignored`
Expected: FAIL — compile error, `Request::FileDialogSet` does not exist.

- [ ] **Step 3: Add the wire types**

In `crates/fastuse-proto/src/wire.rs`, add to the `Request` enum after the `WaitForIdle` variant:

```rust
    // --- v2.5.0: file upload ---
    /// Fill and submit a native common file dialog (`#32770`).
    FileDialogSet {
        /// Absolute paths to place in the filename field. Redacted: paths
        /// carry usernames.
        paths: Redact<Vec<String>>,
        /// Scope discovery to this window's process. `None` uses the
        /// foreground window's process at call time.
        hwnd: Option<u64>,
        /// Budget for the dialog to appear. `0` skips the wait and requires
        /// the dialog to already be up.
        wait_for_dialog_ms: u32,
        /// Budget for the dialog to close after submit. `0` skips the wait.
        wait_for_close_ms: u32,
        /// If false, fill the field and stop — no Enter, no close wait.
        submit: bool,
        /// Permit paths that do not exist yet. Required for Save dialogs,
        /// whose whole purpose is naming a file that is not there. The
        /// parent directory must still exist.
        allow_new: bool,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
```

Add to the `Response` enum, after the existing variants:

```rust
    /// Outcome of a `FileDialogSet`.
    FileDialog(FileDialogResult),
```

And the result struct, near the other Phase-4 payload structs:

```rust
/// What `file_dialog_set` observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDialogResult {
    /// HWND of the dialog that was filled.
    pub dialog_hwnd: u64,
    /// True when that specific dialog is gone. False when `submit` was
    /// false, or when the close wait expired.
    pub closed: bool,
    /// Any `#32770` that appeared after ours and is still up — typically a
    /// Save dialog's overwrite-confirm prompt. Answering it is the agent's
    /// call, not ours: it is a destructive choice.
    pub follow_up_dialogs: Vec<crate::coords::WindowInfo>,
    /// How the filename field was written: `"value_pattern"` or `"wm_settext"`.
    pub fill_method: String,
}
```

- [ ] **Step 4: Verify the redact lint accepts the new variant**

Run: `cargo run -p xtask -- check-redact`
Expected: PASS. `paths` is on the suspect list from Task 1 and is wrapped, so the lint is satisfied. If it fails, the field is not `Redact<...>` — fix the field, not the lint.

- [ ] **Step 5: Write the dialog module**

Create `crates/fastuse-win/src/files/dialog.rs`:

```rust
//! `file_dialog_set` — fill and submit a native common file dialog.
//!
//! Discovery is a poll over `list_windows` for class `#32770`, scoped to a
//! process, because a Chrome upload dialog and a leftover dialog from an
//! unrelated app are both `#32770` and typing a path into the wrong one hits
//! something the user cares about.
//!
//! The filename field is NOT located by a `FindWindowEx` class walk: the
//! Vista+ `IFileDialog` field is an `Edit` nested in a `ComboBoxEx32` while
//! the legacy `GetOpenFileName` field is a bare `Edit`, and the walk differs.
//! Instead every `Edit` descendant is tried through `ValuePattern::SetValue`,
//! which is atomic and does not depend on focus, with `WM_SETTEXT` as the
//! fallback for dialogs that expose no usable UIA tree.

use std::time::{Duration, Instant};

use fastuse_proto::{
    coords::WindowInfo, ControlType, Error as ProtoError, ErrorCode, FileDialogResult, Selector,
};

use crate::input_thread::InputThreadHandle;
use crate::uia_pool::UiaPoolHandle;

// Note: `press_chord` in `input/modifier_guard.rs` is a different thing (it
// takes `&[ModKey]` and returns a guard). The chord entry point is
// `input::handlers::key(chord_str, repeat)`.
use crate::window::list_windows::list_windows;

/// Window class of every Win32 common dialog.
const DIALOG_CLASS: &str = "#32770";

/// Fill (and optionally submit) a file dialog. See `Request::FileDialogSet`.
#[allow(clippy::too_many_arguments)]
pub fn file_dialog_set(
    uia_pool: &UiaPoolHandle,
    input: &InputThreadHandle,
    paths: Vec<String>,
    hwnd: Option<u64>,
    wait_for_dialog_ms: u32,
    wait_for_close_ms: u32,
    submit: bool,
    allow_new: bool,
) -> Result<FileDialogResult, ProtoError> {
    let resolved = if allow_new {
        super::resolve_paths_allowing_new(&paths)?
    } else {
        super::resolve_paths(&paths)?
    };
    let field_value = join_for_field(&resolved);

    let scope_pid = scope_pid(hwnd)?;
    let before: Vec<u64> = dialogs_for_pid(scope_pid)?.iter().map(|w| w.hwnd).collect();
    let dialog = wait_for_dialog(scope_pid, wait_for_dialog_ms)?;

    let fill_method = fill_field(uia_pool, dialog.hwnd, &field_value)?;

    if !submit {
        return Ok(FileDialogResult {
            dialog_hwnd: dialog.hwnd,
            closed: false,
            follow_up_dialogs: Vec::new(),
            fill_method,
        });
    }

    // Enter, not a click on the Open button: Enter is invariant to the
    // button's position and to the dialog's language.
    input.run(move || crate::input::handlers::key("enter", 1))?;

    let closed = wait_for_close(dialog.hwnd, wait_for_close_ms);
    let follow_up_dialogs = dialogs_for_pid(scope_pid)?
        .into_iter()
        .filter(|w| w.hwnd != dialog.hwnd && !before.contains(&w.hwnd))
        .collect::<Vec<_>>();

    if !closed && follow_up_dialogs.is_empty() {
        // The single most useful error we can give. A single-select dialog
        // treats the quoted multi-path string as one literal filename and
        // simply refuses to close, which is otherwise indistinguishable from
        // a wrong path.
        let hint = if resolved.len() > 1 {
            "dialog still open after submit; it is probably single-select and \
             cannot accept several paths"
        } else {
            "dialog still open after submit; the app rejected the path"
        };
        return Err(ProtoError::new(ErrorCode::DialogStillOpen, hint.to_string()));
    }

    Ok(FileDialogResult {
        dialog_hwnd: dialog.hwnd,
        closed,
        follow_up_dialogs,
        fill_method,
    })
}

/// Multiple paths go in the quoted form the common dialog understands.
fn join_for_field(paths: &[std::path::PathBuf]) -> String {
    if paths.len() == 1 {
        paths[0].to_string_lossy().into_owned()
    } else {
        paths
            .iter()
            .map(|p| format!("\"{}\"", p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Resolve the process whose dialogs we are willing to touch.
fn scope_pid(hwnd: Option<u64>) -> Result<u32, ProtoError> {
    let target = match hwnd {
        Some(h) => h,
        None => crate::uia::foreground_hwnd().ok_or_else(|| {
            ProtoError::new(ErrorCode::WindowNotFound, "no foreground window".to_string())
        })?,
    };
    let all = list_windows(None, None, true)?;
    all.iter()
        .find(|w| w.hwnd == target)
        .map(|w| w.pid)
        .ok_or_else(|| {
            ProtoError::new(ErrorCode::WindowNotFound, format!("hwnd {target} not found"))
        })
}

/// Every visible `#32770` owned by `pid`.
fn dialogs_for_pid(pid: u32) -> Result<Vec<WindowInfo>, ProtoError> {
    Ok(list_windows(None, None, true)?
        .into_iter()
        .filter(|w| w.pid == pid && w.class == DIALOG_CLASS)
        .collect())
}

/// Poll on the same 50ms cadence `wait_for_window` uses.
fn wait_for_dialog(pid: u32, timeout_ms: u32) -> Result<WindowInfo, ProtoError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        if let Some(w) = dialogs_for_pid(pid)?.into_iter().next() {
            return Ok(w);
        }
        if Instant::now() >= deadline {
            return Err(ProtoError::new(
                ErrorCode::DialogNotFound,
                format!("no #32770 dialog in pid {pid} within {timeout_ms}ms"),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Watch the specific HWND we filled — not "any #32770 is gone". Save dialogs
/// stack an overwrite-confirm on top, and watching the class would report
/// success the instant focus moved to that prompt.
fn wait_for_close(hwnd: u64, timeout_ms: u32) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        if !crate::window::is_window(hwnd) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Try every `Edit` descendant through `ValuePattern`, then `WM_SETTEXT`.
fn fill_field(
    uia_pool: &UiaPoolHandle,
    dialog_hwnd: u64,
    value: &str,
) -> Result<String, ProtoError> {
    let v = value.to_string();
    let via_uia: bool = uia_pool.run(move |uia| {
        let root = crate::uia::cache::get_or_fetch(uia, dialog_hwnd)?;
        let edits = crate::uia::find::find_all(uia, &root, &Selector::ByControlType(ControlType::Edit))?;
        for node in &edits {
            if crate::uia::element_actions::set_value_on_node(uia, &root, node, &v).is_ok() {
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    if via_uia {
        return Ok("value_pattern".to_string());
    }
    if crate::files::dialog_wm_settext(dialog_hwnd, value)? {
        return Ok("wm_settext".to_string());
    }
    Err(ProtoError::new(
        ErrorCode::ElementNotFound,
        "dialog exposes no writable filename field".to_string(),
    ))
}
```

Two helpers this leans on may not exist yet in the shapes named above. Before writing the module, check and add whichever are missing, keeping them beside their existing siblings:

- `crate::window::is_window(hwnd: u64) -> bool` — a thin `IsWindow` wrapper. Add to `crates/fastuse-win/src/window/mod.rs` if absent.
- `crate::uia::element_actions::set_value_on_node(uia, root, node, value) -> Result<(), ProtoError>` — relocate the cached node to a live element and call `UIValuePattern::set_value`. `element_actions.rs` already has `relocate_live_element` and `build_cached_root`; model the new function on `handle_scroll_into_view`, which does exactly this relocate-then-pattern dance.
- `crate::files::dialog_wm_settext(hwnd, value) -> Result<bool, ProtoError>` — `FindWindowExW` walk to the deepest `Edit` child, then `SendMessageW(WM_SETTEXT)`. Put it in `files/dialog.rs` itself and drop the `crate::files::` qualifier.

Register the submodule in `crates/fastuse-win/src/files/mod.rs`:

```rust
pub mod dialog;
```

- [ ] **Step 6: Add the dispatch arm**

In `crates/fastuse-daemon/src/dispatch.rs`, beside the other arms (the `Request::ClipboardSet` arm around line 254 is the shape to copy — note that `file_dialog_set` is deliberately NOT permission-gated, so it does not go through `gate_then`):

```rust
        Request::FileDialogSet {
            paths,
            hwnd,
            wait_for_dialog_ms,
            wait_for_close_ms,
            submit,
            allow_new,
            opts,
        } => match (ctx.uia.as_ref(), ctx.input.as_ref()) {
            (Some(uia), Some(input)) => {
                let w = Instant::now();
                let r = fastuse_win::files::dialog::file_dialog_set(
                    uia,
                    input,
                    paths.into_inner(),
                    hwnd,
                    wait_for_dialog_ms,
                    wait_for_close_ms,
                    submit,
                    allow_new,
                );
                win32_us = w.elapsed().as_micros() as i64;
                let inner = match r {
                    Ok(res) => Response::FileDialog(res),
                    Err(e) => Response::Error(e),
                };
                finalize(inner, opts, ctx)
            }
            _ => Response::Error(Error::new(
                ErrorCode::Internal,
                "uia pool or input thread unavailable".to_string(),
            )),
        },
```

Verified in `dispatch.rs` while writing this plan, so do not re-derive it: `DispatchCtx` field for the UIA pool is `uia: Option<Arc<UiaPoolHandle>>` (NOT `uia_pool`), and the input handle is `input: Option<Arc<InputThreadHandle>>`. Both are `Option`s that surrounding arms unwrap with `.as_ref()` and an explicit `None` error arm — the `Request::UiaQuery` arm around line 413 is the pattern to copy. Match its `None` message style if it differs from the placeholder above.

- [ ] **Step 7: Build and run the unit tests**

Run: `cargo test -p fastuse-win -p fastuse-proto -p fastuse-daemon`
Expected: PASS. The E2E test is `#[ignore]`d and does not run here.

- [ ] **Step 8: Run the E2E test for real**

Run: `cargo build --release && cargo test -p fastuse-daemon --test file_dialog_e2e -- --ignored --nocapture`
Expected: PASS — Notepad opens, the Save dialog is filled, and `%TEMP%\fastuse_dialog_e2e.txt` exists afterwards. This is the verification that matters; a green unit suite is not it. If it fails, read the actual error code before changing anything: `DIALOG_NOT_FOUND` means discovery scoping is wrong, `ELEMENT_NOT_FOUND` means neither fill path found the field, `DIALOG_STILL_OPEN` means the path was rejected.

- [ ] **Step 9: Commit**

```bash
git add crates/fastuse-proto/src/wire.rs crates/fastuse-win/src/files crates/fastuse-daemon/src/dispatch.rs crates/fastuse-daemon/tests/file_dialog_e2e.rs
git commit -m "feat(win,proto,daemon): file_dialog_set

Fills a #32770 common dialog through UIA ValuePattern with a WM_SETTEXT
fallback, submits with Enter, and confirms by watching the specific dialog
HWND rather than the class, so a Save dialog's overwrite-confirm is reported
instead of being mistaken for success."
```

---

## Task 3: `file_dialog_set` — MCP tool and CLI subcommand

**Files:**
- Modify: `crates/fastuse-mcp/src/handler.rs`
- Modify: `crates/fastuse-cli/src/main.rs`
- Modify: `crates/fastuse-cli/src/cmd_phase4.rs`

**Interfaces:**
- Consumes: `Request::FileDialogSet` and `Response::FileDialog(FileDialogResult)` from Task 2.
- Produces: MCP tool `file_dialog_set`; CLI `fastuse-cli file-dialog-set <paths...>`.

- [ ] **Step 1: Add the MCP args and output structs**

In `crates/fastuse-mcp/src/handler.rs`, beside the other `Args` structs (near `ClipboardSetTextArgs`, around line 786):

```rust
#[derive(Deserialize, schemars::JsonSchema)]
pub struct FileDialogSetArgs {
    /// Absolute paths to put in the dialog's filename field.
    pub paths: Vec<String>,
    /// Scope discovery to this window's process. Omit to use the foreground
    /// window's process.
    pub hwnd: Option<u64>,
    /// Budget for the dialog to appear, ms. Default 5000; 0 skips the wait.
    pub wait_for_dialog_ms: Option<u32>,
    /// Budget for the dialog to close after submit, ms. Default 5000.
    pub wait_for_close_ms: Option<u32>,
    /// Fill only, do not press Enter. Default false.
    pub fill_only: Option<bool>,
    /// Allow a path that does not exist yet. Set this for Save dialogs —
    /// naming a new file is what they are for. Default false.
    pub allow_new: Option<bool>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct FileDialogOutput {
    pub dialog_hwnd: u64,
    pub closed: bool,
    pub fill_method: String,
    pub follow_up_dialog_hwnds: Vec<u64>,
}
```

- [ ] **Step 2: Add the tool method**

In the same file, in the `#[tool_router]` impl block beside the clipboard tools:

```rust
    #[tool(
        name = "file_dialog_set",
        description = "Fill and submit a native Windows file dialog (#32770) that is open or about to open. \
                       Call this right after clicking a 'Choose file' / Save button. Waits for the dialog, \
                       writes the paths into the filename field, presses Enter, and confirms the dialog closed."
    )]
    async fn file_dialog_set(
        &self,
        Parameters(args): Parameters<FileDialogSetArgs>,
    ) -> Result<Json<FileDialogOutput>, McpError> {
        let req = Request::FileDialogSet {
            paths: Redact::new(args.paths),
            hwnd: args.hwnd,
            wait_for_dialog_ms: args.wait_for_dialog_ms.unwrap_or(5000),
            wait_for_close_ms: args.wait_for_close_ms.unwrap_or(5000),
            submit: !args.fill_only.unwrap_or(false),
            allow_new: args.allow_new.unwrap_or(false),
            opts: None,
        };
        match self.call(req).await? {
            Response::FileDialog(r) => Ok(Json(FileDialogOutput {
                dialog_hwnd: r.dialog_hwnd,
                closed: r.closed,
                fill_method: r.fill_method,
                follow_up_dialog_hwnds: r.follow_up_dialogs.iter().map(|w| w.hwnd).collect(),
            })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
```

- [ ] **Step 3: Add the CLI subcommand**

In `crates/fastuse-cli/src/main.rs`, in the `Cmd` enum beside `ClipboardSetText` (around line 351):

```rust
    /// Fill and submit a native file dialog.
    FileDialogSet {
        /// Absolute paths to place in the filename field.
        paths: Vec<String>,
        /// Scope discovery to this window's process.
        #[arg(long)]
        hwnd: Option<u64>,
        /// Budget for the dialog to appear, ms.
        #[arg(long, default_value_t = 5000)]
        wait_for_dialog_ms: u32,
        /// Budget for the dialog to close after submit, ms.
        #[arg(long, default_value_t = 5000)]
        wait_for_close_ms: u32,
        /// Fill the field but do not press Enter.
        #[arg(long)]
        fill_only: bool,
        /// Allow a path that does not exist yet (Save dialogs).
        #[arg(long)]
        allow_new: bool,
    },
```

And in the dispatch `match` (around line 879):

```rust
            Cmd::FileDialogSet {
                paths, hwnd, wait_for_dialog_ms, wait_for_close_ms, fill_only, allow_new,
            } => {
                cmd_phase4::file_dialog_set(
                    &identity.path, paths, hwnd, wait_for_dialog_ms, wait_for_close_ms,
                    !fill_only, allow_new,
                )
                .await
            }
```

- [ ] **Step 4: Add the CLI handler**

In `crates/fastuse-cli/src/cmd_phase4.rs`, after `clipboard_set_text`:

```rust
pub async fn file_dialog_set(
    pipe_path: &str,
    paths: Vec<String>,
    hwnd: Option<u64>,
    wait_for_dialog_ms: u32,
    wait_for_close_ms: u32,
    submit: bool,
    allow_new: bool,
) -> anyhow::Result<()> {
    let req = Request::FileDialogSet {
        paths: Redact::new(paths),
        hwnd,
        wait_for_dialog_ms,
        wait_for_close_ms,
        submit,
        allow_new,
        opts: None,
    };
    match one_call(pipe_path, req).await? {
        Response::FileDialog(r) => {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "dialog_hwnd": r.dialog_hwnd,
                    "closed": r.closed,
                    "fill_method": r.fill_method,
                    "follow_up_dialogs": r.follow_up_dialogs.len(),
                })
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}
```

- [ ] **Step 5: Build and verify the CLI end to end by hand**

Run:
```bash
cargo build --release
./target/release/fastuse-cli.exe launch-app notepad.exe
./target/release/fastuse-cli.exe computer key ctrl+s
./target/release/fastuse-cli.exe file-dialog-set "$TEMP/fastuse_cli_check.txt" --allow-new
```
Expected: JSON with `"closed": true`, and `%TEMP%\fastuse_cli_check.txt` on disk. Delete it afterwards. `--allow-new` is required here because this is a Save dialog naming a file that does not exist yet; without it the call fails validation, which is the intended behaviour.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-mcp/src/handler.rs crates/fastuse-cli/src
git commit -m "feat(mcp,cli): expose file_dialog_set"
```

---

## Task 4: `CF_HDROP` byte-buffer construction

**Files:**
- Create: `crates/fastuse-win/src/files/hdrop.rs`
- Modify: `crates/fastuse-win/src/files/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `fastuse_win::files::hdrop::build_hdrop<P: AsRef<Path>>(paths: &[P]) -> Vec<u8>` — a `DROPFILES` header followed by a double-null-terminated UTF-16 path list, ready to be copied into an `HGLOBAL`.

Keeping the byte layout in a pure function is what makes it testable at all: the clipboard path and the drag path both consume it, and neither is unit-testable on its own.

- [ ] **Step 1: Write the failing test**

Create `crates/fastuse-win/src/files/hdrop.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Decode the UTF-16 path list that follows the header.
    fn names_from(buf: &[u8]) -> Vec<String> {
        let off = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let units: Vec<u16> = buf[off..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        units
            .split(|u| *u == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn header_is_20_bytes_and_declares_wide() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        // pFiles is the offset to the list and must equal sizeof(DROPFILES).
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), 20);
        // fWide is the last BOOL in the header, at offset 16.
        assert_eq!(u32::from_le_bytes(buf[16..20].try_into().unwrap()), 1);
    }

    #[test]
    fn single_path_round_trips() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        assert_eq!(names_from(&buf), vec![r"C:\a.txt".to_string()]);
    }

    #[test]
    fn several_paths_round_trip_in_order() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt"), PathBuf::from(r"C:\b.png")]);
        assert_eq!(names_from(&buf), vec![r"C:\a.txt".to_string(), r"C:\b.png".to_string()]);
    }

    #[test]
    fn list_is_double_null_terminated() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        assert_eq!(&buf[buf.len() - 4..], &[0, 0, 0, 0], "needs a trailing extra NUL");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fastuse-win files::hdrop`
Expected: FAIL — `build_hdrop` is not defined.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/fastuse-win/src/files/hdrop.rs`:

```rust
//! `CF_HDROP` payload construction.
//!
//! Kept as a pure byte-buffer builder with no Win32 calls: both the clipboard
//! path and the drag path consume it, and neither of those is unit-testable on
//! its own. The layout is a `DROPFILES` header (`pFiles`, `pt`, `fNC`,
//! `fWide` — 20 bytes on x64 with 4-byte fields and an 8-byte POINT) followed
//! by each path as UTF-16, NUL-separated, with one extra NUL to close the list.

use std::path::Path;

/// `sizeof(DROPFILES)`: u32 + POINT(2×i32) + BOOL + BOOL.
const DROPFILES_SIZE: u32 = 4 + 8 + 4 + 4;

/// Build a `CF_HDROP` payload for `paths`.
pub fn build_hdrop<P: AsRef<Path>>(paths: &[P]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(DROPFILES_SIZE as usize + paths.len() * 64);
    buf.extend_from_slice(&DROPFILES_SIZE.to_le_bytes()); // pFiles
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    buf.extend_from_slice(&0u32.to_le_bytes()); // fNC = FALSE
    buf.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
    debug_assert_eq!(buf.len(), DROPFILES_SIZE as usize);

    for p in paths {
        for unit in p.as_ref().to_string_lossy().encode_utf16() {
            buf.extend_from_slice(&unit.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes()); // list terminator
    buf
}
```

Register it in `crates/fastuse-win/src/files/mod.rs`:

```rust
pub mod hdrop;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fastuse-win files::hdrop`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-win/src/files/hdrop.rs crates/fastuse-win/src/files/mod.rs
git commit -m "feat(win): CF_HDROP payload builder

Pure byte-buffer construction, shared by the clipboard and drag paths."
```

---

## Task 5: `clipboard_set_files` — publish, paste, dispatch

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs` (`ClipboardSet` enum)
- Modify: `crates/fastuse-win/src/clipboard/mod.rs`
- Modify: `crates/fastuse-core/src/perm.rs`
- Modify: `crates/fastuse-daemon/src/dispatch.rs`
- Create: `crates/fastuse-win/tests/hdrop_roundtrip.rs`

**Interfaces:**
- Consumes: `files::resolve_paths` (Task 1), `files::hdrop::build_hdrop` (Task 4).
- Produces: `ClipboardSet::Files { paths: Redact<Vec<String>>, paste: bool, hwnd: Option<u64> }`, handled inside the existing `clipboard_set`.

- [ ] **Step 1: Write the failing test**

Create `crates/fastuse-win/tests/hdrop_roundtrip.rs`:

```rust
//! Publish CF_HDROP to the real clipboard and read it back.
//!
//! Run with: `cargo test -p fastuse-win --test hdrop_roundtrip -- --ignored`
//! Ignored by default: it clobbers the developer's clipboard.

#![cfg(target_os = "windows")]

use fastuse_proto::{ClipboardSet, Redact};

#[test]
#[ignore = "clobbers the real clipboard"]
fn cf_hdrop_round_trips_with_copy_effect() {
    let p = std::env::temp_dir().join("fastuse_hdrop_roundtrip.txt");
    std::fs::write(&p, b"x").unwrap();

    fastuse_win::clipboard::clipboard_set(ClipboardSet::Files {
        paths: Redact::new(vec![p.to_string_lossy().into_owned()]),
        paste: false,
        hwnd: None,
    })
    .expect("clipboard_set files");

    let names = fastuse_win::clipboard::read_back_hdrop_for_test().expect("read back");
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with("fastuse_hdrop_roundtrip.txt"), "got {}", names[0]);

    // Without a preferred-drop-effect of COPY, Explorer treats a paste as a
    // MOVE and the user's source file disappears. That is data loss, so the
    // effect is asserted, not assumed.
    let effect = fastuse_win::clipboard::read_back_preferred_effect_for_test().expect("effect");
    const DROPEFFECT_COPY: u32 = 1;
    assert_eq!(effect, DROPEFFECT_COPY);

    std::fs::remove_file(&p).ok();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fastuse-win --test hdrop_roundtrip -- --ignored`
Expected: FAIL — compile error, `ClipboardSet::Files` does not exist.

- [ ] **Step 3: Add the wire variant**

In `crates/fastuse-proto/src/wire.rs`, add to the `ClipboardSet` enum:

```rust
    /// File list payload — published as `CF_HDROP`.
    Files {
        /// Absolute paths. Redacted: paths carry usernames.
        paths: Redact<Vec<String>>,
        /// Send Ctrl+V to the target after publishing.
        paste: bool,
        /// Window to focus before pasting. `None` uses the current
        /// foreground window.
        hwnd: Option<u64>,
    },
```

Extend the hand-written `impl core::fmt::Debug for ClipboardSet` in the same file with the new variant, recording only a count — never the paths:

```rust
            Self::Files { paths, paste, hwnd } => f
                .debug_struct("ClipboardSet::Files")
                .field("path_count", &paths.as_ref().len())
                .field("paste", paste)
                .field("hwnd", hwnd)
                .finish(),
```

If `Redact` exposes no `as_ref`, use whatever accessor `redact.rs` provides that does not unwrap into a loggable value; the requirement is that no path text reaches `Debug`.

- [ ] **Step 4: Add the permission row**

In `crates/fastuse-core/src/perm.rs`, in the `TOOLS` table beside the other clipboard rows:

```rust
    // Same resource clipboard_set_text/_image clobber, so the same tier.
    ToolPerm { name: "clipboard_set_files", default_tier: Tier::Confirmed },
```

- [ ] **Step 5: Handle the variant in the clipboard module**

In `crates/fastuse-win/src/clipboard/mod.rs`, add the format constants near `CF_UNICODETEXT`:

```rust
/// Standard clipboard format: CF_HDROP.
const CF_HDROP: u32 = 15;

/// `DROPEFFECT_COPY`. Published under `CFSTR_PREFERREDDROPEFFECT` so targets
/// copy rather than MOVE — without it Explorer relocates the user's source
/// file on paste, which is data loss, not a UX wart.
const DROPEFFECT_COPY: u32 = 1;
```

Add a `ClipboardSet::Files` arm to `clipboard_set` that, on the existing STA helper thread, opens the clipboard, empties it, and publishes two formats: the `build_hdrop` buffer under `CF_HDROP`, and a 4-byte `DROPEFFECT_COPY` under the registered format `CFSTR_PREFERREDDROPEFFECT` (obtained with `RegisterClipboardFormatW(w!("Preferred DropEffect"))`). Both payloads go into `GlobalAlloc(GMEM_MOVEABLE, len)` blocks, filled under `GlobalLock`/`GlobalUnlock` — copy the existing image path's allocation helper rather than writing a new one; ownership transfers to the clipboard on `SetClipboardData`, so the handles must not be freed afterwards.

Path resolution happens before the clipboard is touched: `let resolved = crate::files::resolve_paths(&paths.into_inner())?;` so a bad path never empties the user's clipboard.

When `paste` is true, after the clipboard is closed: focus `hwnd` if given via the existing focus helper, run the existing `wait_for_idle` drain against it, then send `ctrl+v` through the input thread. The drain is what stops Electron targets from swallowing the paste.

Add the two read-back helpers the test calls. `#[cfg(test)]` does **not** apply to an integration test under `tests/`, so these are ordinary `pub` functions marked `#[doc(hidden)]` — read `crates/fastuse-win/tests/bench_phase4.rs` first and match however it already reaches into this crate:

```rust
/// Read the clipboard's `CF_HDROP` back as plain strings.
///
/// Exists for the round-trip test: the publish path has no other observable
/// output, so without this the only check would be "it did not error".
#[doc(hidden)]
pub fn read_back_hdrop_for_test() -> Result<Vec<String>, FastuseError> {
    run_on_clipboard_thread(|| {
        // SAFETY: opened with a NULL owner; paired with CloseClipboard below.
        unsafe { OpenClipboard(Some(HWND::default())) }
            .map_err(|e| FastuseError::Io(format!("OpenClipboard: {e}")))?;
        let result = (|| -> Result<Vec<String>, FastuseError> {
            // SAFETY: handle stays owned by the clipboard; we only read it.
            let h = unsafe { GetClipboardData(CF_HDROP) }
                .map_err(|e| FastuseError::Io(format!("GetClipboardData(CF_HDROP): {e}")))?;
            let hdrop = HDROP(h.0);
            // SAFETY: count query form — u32::MAX index, NULL buffer.
            let n = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let mut buf = [0u16; 260];
                // SAFETY: buf outlives the call; len is its element count.
                let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
                out.push(String::from_utf16_lossy(&buf[..written]));
            }
            Ok(out)
        })();
        // SAFETY: paired with the OpenClipboard above.
        unsafe { let _ = CloseClipboard(); }
        result
    })
}

/// Read the `Preferred DropEffect` DWORD back.
#[doc(hidden)]
pub fn read_back_preferred_effect_for_test() -> Result<u32, FastuseError> {
    run_on_clipboard_thread(|| {
        // SAFETY: opened with a NULL owner; paired with CloseClipboard below.
        unsafe { OpenClipboard(Some(HWND::default())) }
            .map_err(|e| FastuseError::Io(format!("OpenClipboard: {e}")))?;
        let result = (|| -> Result<u32, FastuseError> {
            // SAFETY: same registered name the publish path uses.
            let fmt = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
            // SAFETY: handle stays owned by the clipboard.
            let h = unsafe { GetClipboardData(fmt) }
                .map_err(|e| FastuseError::Io(format!("GetClipboardData(effect): {e}")))?;
            // SAFETY: the blob is a single DWORD; unlocked immediately after.
            let p = unsafe { GlobalLock(HGLOBAL(h.0)) } as *const u32;
            if p.is_null() {
                return Err(FastuseError::Io("GlobalLock returned null".into()));
            }
            let v = unsafe { *p };
            // SAFETY: paired with the GlobalLock above.
            unsafe { let _ = GlobalUnlock(HGLOBAL(h.0)); }
            Ok(v)
        })();
        // SAFETY: paired with the OpenClipboard above.
        unsafe { let _ = CloseClipboard(); }
        result
    })
}
```

`DragQueryFileW` and `HDROP` need `Win32_UI_Shell`, already enabled. `RegisterClipboardFormatW` and `w!` come from `Win32_System_DataExchange` and `windows::core`, both already in use in this file. If `HDROP(h.0)` does not typecheck, read the actual `HANDLE`/`HDROP` field shapes in the installed crate rather than guessing at a cast.

- [ ] **Step 6: Add the dispatch tool name**

In `crates/fastuse-daemon/src/dispatch.rs`, extend the tool-name match inside the `Request::ClipboardSet` arm (line ~256):

```rust
                ClipboardSet::Files { .. } => "clipboard_set_files",
```

Nothing else in that arm changes — the gate and the call already route through `clipboard_set`.

- [ ] **Step 7: Run the unit tests, then the round-trip for real**

Run: `cargo test -p fastuse-win -p fastuse-proto -p fastuse-core`
Expected: PASS.

Run: `cargo test -p fastuse-win --test hdrop_roundtrip -- --ignored --nocapture`
Expected: PASS. Then paste into an Explorer window by hand once and confirm the file is **copied**, not moved — the automated assertion checks the flag, and this checks that the flag means what we think.

- [ ] **Step 8: Commit**

```bash
git add crates/fastuse-proto/src/wire.rs crates/fastuse-core/src/perm.rs crates/fastuse-win/src/clipboard crates/fastuse-win/tests/hdrop_roundtrip.rs crates/fastuse-daemon/src/dispatch.rs
git commit -m "feat(win,proto,core): clipboard_set_files via CF_HDROP

Publishes CF_HDROP plus a preferred-drop-effect of COPY, without which
Explorer treats a paste as a MOVE and relocates the user's source file.
Gated at the same tier as the other clipboard writes."
```

---

## Task 6: `clipboard_set_files` — MCP tool, CLI subcommand, docs

**Files:**
- Modify: `crates/fastuse-mcp/src/handler.rs`
- Modify: `crates/fastuse-cli/src/main.rs`, `crates/fastuse-cli/src/cmd_phase4.rs`
- Modify: `docs/permissions.md`

**Interfaces:**
- Consumes: `ClipboardSet::Files` from Task 5.
- Produces: MCP tool `clipboard_set_files`; CLI `fastuse-cli clipboard-set-files <paths...> [--paste] [--hwnd N]`.

- [ ] **Step 1: Add the MCP args struct**

In `crates/fastuse-mcp/src/handler.rs` beside `ClipboardSetTextArgs`:

```rust
#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardSetFilesArgs {
    /// Absolute paths to place on the clipboard as a file list.
    pub paths: Vec<String>,
    /// Send Ctrl+V to the target after copying. Default false.
    pub paste: Option<bool>,
    /// Window to focus before pasting. Omit to use the foreground window.
    pub hwnd: Option<u64>,
}
```

- [ ] **Step 2: Add the tool method**

```rust
    #[tool(
        name = "clipboard_set_files",
        description = "Put files on the clipboard as a file list (CF_HDROP), optionally pasting them into a \
                       window with Ctrl+V. Works in Discord, Slack, Explorer, most Electron apps, and many \
                       web drop zones. Permission-gated (Confirmed tier)."
    )]
    async fn clipboard_set_files(
        &self,
        Parameters(args): Parameters<ClipboardSetFilesArgs>,
    ) -> Result<Json<AckOutput>, McpError> {
        let req = Request::ClipboardSet {
            req: fastuse_proto::ClipboardSet::Files {
                paths: Redact::new(args.paths),
                paste: args.paste.unwrap_or(false),
                hwnd: args.hwnd,
            },
            opts: None,
        };
        match self.call(req).await? {
            Response::ClipboardSet => Ok(Json(AckOutput { ok: true, slept_us: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
```

- [ ] **Step 3: Add the CLI subcommand**

In `crates/fastuse-cli/src/main.rs`, in `Cmd`:

```rust
    /// Put files on the clipboard as CF_HDROP (permission-gated).
    ClipboardSetFiles {
        /// Absolute paths.
        paths: Vec<String>,
        /// Send Ctrl+V after copying.
        #[arg(long)]
        paste: bool,
        /// Window to focus before pasting.
        #[arg(long)]
        hwnd: Option<u64>,
    },
```

In the dispatch `match`:

```rust
            Cmd::ClipboardSetFiles { paths, paste, hwnd } => {
                cmd_phase4::clipboard_set_files(&identity.path, paths, paste, hwnd).await
            }
```

In `cmd_phase4.rs`:

```rust
pub async fn clipboard_set_files(
    pipe_path: &str,
    paths: Vec<String>,
    paste: bool,
    hwnd: Option<u64>,
) -> anyhow::Result<()> {
    let req = Request::ClipboardSet {
        req: ClipboardSet::Files { paths: Redact::new(paths), paste, hwnd },
        opts: None,
    };
    match one_call(pipe_path, req).await? {
        Response::ClipboardSet => {
            println!("{}", json!({"ok": true}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}
```

- [ ] **Step 4: Document the gate**

In `docs/permissions.md`, add to the "Default gated tools" list:

```markdown
- `clipboard_set_files` — overwrites the user's clipboard with a file list
```

- [ ] **Step 5: Verify by driving a real target**

Run:
```bash
cargo build --release
./target/release/fastuse-cli.exe clipboard-set-files "$TEMP/fastuse_hdrop_check.txt" --paste
```
with a Discord or Explorer window focused (create the file first). Expected: the file appears as an attachment or a copy. This is the verification; the unit tests only prove the bytes are shaped right.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-mcp/src/handler.rs crates/fastuse-cli/src docs/permissions.md
git commit -m "feat(mcp,cli): expose clipboard_set_files"
```

---

## Task 7: Spike — de-elevating a child process

**Files:**
- Create (throwaway): `crates/fastuse-win/examples/deelevate_spike.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a decision, recorded in the plan, about which de-elevation call to use in Task 8. The example is deleted at the end of the task; only the answer survives.

The daemon is High integrity. A `DoDragDrop` started there can never be completed by a medium-IL Chrome, because the drop target calls back into the source's `IDataObject` and that direction is blocked. Everything in Tasks 8–10 depends on being able to spawn a medium-IL child, so this is settled before any of it is written.

- [ ] **Step 1: Write the spike**

Create `crates/fastuse-win/examples/deelevate_spike.rs`:

```rust
//! Throwaway: does token-lowering produce a medium-IL child from our
//! High-IL daemon? Run elevated:
//!   cargo run -p fastuse-win --example deelevate_spike
//! Prints the child's integrity SID. Success is S-1-16-8192 (Medium).

/// The child writes its own integrity SID somewhere we can read it.
const CHILD_CMD: &str = r"cmd.exe /c whoami /groups > %TEMP%\fastuse_spike_a.txt";

fn main() {
    match approach_a(CHILD_CMD) {
        Ok(()) => println!("approach A spawned; read %TEMP%\\fastuse_spike_a.txt"),
        Err(e) => println!("approach A failed: {e}"),
    }
}
```

Step 2 writes `approach_a`, and Step 4 adds `approach_b` only if A fails. Both are throwaway; neither needs error types beyond `windows_core::Result`.

- [ ] **Step 2: Implement approach A**

Sequence: `OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY)` → `DuplicateTokenEx(..., SecurityImpersonation, TokenPrimary)` → build a `SID_AND_ATTRIBUTES` for `S-1-16-8192` via `ConvertStringSidToSidW` → `SetTokenInformation(dup, TokenIntegrityLevel, &til, size)` → `CreateProcessAsUserW(dup, "cmd.exe", "/c whoami /groups > %TEMP%\\fastuse_spike_a.txt", ...)`.

Read each signature out of the installed crate rather than from memory:
```bash
grep -rn "pub unsafe fn SetTokenInformation\|pub unsafe fn CreateProcessAsUserW\|pub unsafe fn DuplicateTokenEx" \
  ~/.cargo/registry/src/*/windows-0.62.2/src/Windows/Win32/Security/
```

These live behind `Win32_Security` (enabled) and `Win32_System_Threading` (enabled). If `ConvertStringSidToSidW` is missing, it is under `Win32_Security_Authorization` — add the feature in this task if so.

- [ ] **Step 3: Run approach A and read the result**

Run: `cargo run -p fastuse-win --example deelevate_spike` from an elevated shell, then `cat "$TEMP/fastuse_spike_a.txt" | grep S-1-16-`
Expected on success: `S-1-16-8192` (Medium Mandatory Level). Anything else — `S-1-16-12288`, or a failed `CreateProcessAsUserW` — means approach A does not work here.

- [ ] **Step 4: If A failed, implement and run approach B**

`GetShellWindow()` → `GetWindowThreadProcessId` → `OpenProcess(PROCESS_QUERY_INFORMATION)` → `OpenProcessToken(TOKEN_DUPLICATE)` → `DuplicateTokenEx(..., TokenPrimary)` → `CreateProcessWithTokenW(dup, 0, "cmd.exe", ...)`. Write to `fastuse_spike_b.txt` and check the same way.

- [ ] **Step 5: Record the answer in this plan**

Edit the Task 8 header below to name the winning approach, and note anything that surprised you. If **both** fail, stop and escalate: Task 8's design does not survive it, and the fallback the spec already names — dropping `drag_files` and routing its use case through `clipboard_set_files` — becomes the recommendation.

- [ ] **Step 6: Delete the spike and commit the answer**

```bash
rm crates/fastuse-win/examples/deelevate_spike.rs
git add docs/superpowers/plans/2026-09-07-file-upload.md crates/fastuse-win/Cargo.toml
git commit -m "chore(win): de-elevation spike — record which token path works

Spike code deleted; only the answer survives, recorded in the plan."
```

---

## Task 8: The de-elevated drag helper

**Files:**
- Modify: `crates/fastuse-win/Cargo.toml` (three new `windows` features)
- Create: `crates/fastuse-win/src/files/deelevate.rs`
- Create: `crates/fastuse-daemon/src/drag_helper.rs`
- Modify: `crates/fastuse-daemon/src/main.rs`

**Interfaces:**
- Consumes: `files::hdrop::build_hdrop` (Task 4); the de-elevation approach chosen in Task 7.
- Produces: `fastuse_win::files::deelevate::spawn_medium_il(exe: &std::path::Path, args: &[String]) -> Result<std::process::Child, ProtoError>`; the `--drag-helper` binary mode, which reads a JSON job on stdin and writes a JSON result on stdout.

- [ ] **Step 1: Add the Cargo features**

Three are needed, not one. Verified against the installed crate: `IDataObject_Vtbl::new` is `#[cfg(all(feature = "Win32_Graphics_Gdi", feature = "Win32_System_Com_StructuredStorage"))]`, and `IDropSource_Impl` is `#[cfg(feature = "Win32_System_SystemServices")]`.

In `crates/fastuse-win/Cargo.toml`, add to the `windows` feature list:

```toml
    "Win32_System_Ole",
    "Win32_System_Com_StructuredStorage",
    "Win32_System_SystemServices",
```

- [ ] **Step 2: Verify the features are sufficient**

Create a scratch file `crates/fastuse-win/src/files/drag_probe.rs` containing only:

```rust
#![allow(dead_code)]
use windows::Win32::System::Com::{IDataObject, FORMATETC, STGMEDIUM};
use windows::Win32::System::Ole::{DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT_COPY};

fn _probe(d: &IDataObject, s: &IDropSource) {
    let mut eff = DROPEFFECT_COPY;
    unsafe { let _ = DoDragDrop(d, s, DROPEFFECT_COPY, &mut eff); }
    let _: Option<FORMATETC> = None;
    let _: Option<STGMEDIUM> = None;
}
```

Run: `cargo build -p fastuse-win` with `pub mod drag_probe;` temporarily added.
Expected: PASS. Then delete the probe file and the `pub mod` line. If it fails, the error names the missing feature — add it before going further, because every remaining step in this task depends on these types resolving.

- [ ] **Step 3: Write the de-elevation helper**

Create `crates/fastuse-win/src/files/deelevate.rs` implementing `spawn_medium_il`, using whichever approach Task 7 proved. Doc comment must state why it exists — a reader six months from now needs to know this is not gratuitous:

```rust
//! Spawn a child at Medium integrity from our High-integrity daemon.
//!
//! OLE drag-drop reverses the direction of the data flow: the drop target
//! calls back into the source's `IDataObject`. A medium-IL Chrome cannot make
//! that call into a high-IL process, which is the same reason a file cannot be
//! dragged from an elevated Explorer into a normal application. So the drag
//! source has to live somewhere Chrome can call.
```

The function takes the current exe path and argv, returns a `std::process::Child` with piped stdin/stdout. `CreateProcessAsUserW` is not what `std::process::Command` calls, so this builds the child by hand and wraps the returned handle — check whether `std::os::windows::io::FromRawHandle` gives a usable `Child` here; if not, keep the raw `PROCESS_INFORMATION` in a small owned struct with a `wait`, a `kill`, and the two pipe handles, and document that instead.

- [ ] **Step 4: Write the helper mode**

Create `crates/fastuse-daemon/src/drag_helper.rs`. It reads one JSON job from stdin:

```rust
/// One drag job, daemon → helper over stdin.
#[derive(serde::Deserialize)]
pub struct DragJob {
    /// Already resolved and validated by the daemon.
    pub paths: Vec<String>,
    /// Where the 1×1 source window goes, virtual-desktop pixels.
    pub start_x: i32,
    pub start_y: i32,
    /// Hard deadline; QueryContinueDrag cancels past it so a drag can never
    /// wedge holding the user's mouse button down.
    pub deadline_ms: u32,
}

/// Outcome, helper → daemon over stdout.
#[derive(serde::Serialize)]
pub struct DragOutcome {
    /// True when DoDragDrop returned DRAGDROP_S_DROP.
    pub dropped: bool,
    /// The DROPEFFECT the target reported.
    pub effect: u32,
    /// Present when the drag failed.
    pub error: Option<String>,
}
```

Then:

1. `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` and `OleInitialize(None)` — `DoDragDrop` requires an STA with OLE initialized, not just COM.
2. Register a window class and create a 1×1 window at `(start_x, start_y)` with `WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, then `SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA)`. Alpha **1**, not 0: a fully transparent layered window can fall out of hit-testing, and this window exists precisely to be hit.
3. Write `{"ready": true}` to stdout and flush, so the daemon knows when to inject the button-down. Injecting before the window exists puts the click on whatever the user had there.
4. Pump messages. On `WM_LBUTTONDOWN`, call `DoDragDrop(&data_object, &drop_source, DROPEFFECT_COPY, &mut effect)`. Calling it from inside the button-down the window actually received — rather than blindly after the injection — is the sequence the API is built around and makes the mouse-capture handoff correct by construction.
5. `IDataObject` serves two formats from `build_hdrop`: `CF_HDROP` and the registered `Preferred DropEffect`. `QueryGetData` returns `S_OK` for those two and `DV_E_FORMATETC` otherwise; `GetData` returns an `HGLOBAL` `STGMEDIUM`; `SetData`, `DAdvise`, `DUnadvise`, `EnumDAdvise` return `E_NOTIMPL`; `EnumFormatEtc` returns `E_NOTIMPL` unless a target refuses without it, in which case implement `IEnumFORMATETC` over the two entries.
6. `IDropSource::QueryContinueDrag` returns `DRAGDROP_S_CANCEL` when `fEscapePressed` is true or the deadline has passed, `DRAGDROP_S_DROP` when the left button is no longer in `grfKeyState`, and `S_OK` otherwise. `GiveFeedback` returns `DRAGDROP_S_USEDEFAULTCURSORS`.
7. Print the `DragOutcome` as JSON, destroy the window, `OleUninitialize`, exit.

- [ ] **Step 5: Wire the mode into main**

In `crates/fastuse-daemon/src/main.rs`, before any daemon startup work (this process must not try to become the daemon, take the singleton, or open the pipe):

```rust
    if std::env::args().any(|a| a == "--drag-helper") {
        return drag_helper::run();
    }
```

Read the existing top of `main` first: `set_per_monitor_v2_first_call()` must still run first, and the singleton/sentinel logic must be skipped entirely for this mode.

- [ ] **Step 6: Verify the helper standalone**

Run it by hand, without the daemon, from a **non-elevated** shell so integrity is not a factor yet:

```bash
echo '{"paths":["C:\\Windows\\win.ini"],"start_x":400,"start_y":400,"deadline_ms":10000}' \
  | ./target/release/fastuse-daemon.exe --drag-helper
```

Then, within ten seconds, press and hold the left mouse button at (400, 400) yourself and drag onto an open Explorer window. Expected: the file copies, and the helper prints `{"dropped":true,...}`. Doing this by hand first separates "the OLE object graph is correct" from "the daemon's injection is correct", which is the difference between one bug and two.

- [ ] **Step 7: Commit**

```bash
git add crates/fastuse-win/Cargo.toml crates/fastuse-win/src/files/deelevate.rs crates/fastuse-daemon/src/drag_helper.rs crates/fastuse-daemon/src/main.rs
git commit -m "feat(daemon,win): de-elevated OLE drag helper

The daemon runs at High integrity and OLE drop targets call back into the
drag source, so a medium-IL browser can never complete a drag started in the
daemon. The source now lives in a medium-IL child re-executed from the same
binary."
```

---

## Task 9: `drag_files` — daemon-side orchestration

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs`
- Modify: `crates/fastuse-core/src/perm.rs`
- Create: `crates/fastuse-win/src/files/drag.rs`
- Modify: `crates/fastuse-daemon/src/dispatch.rs`

**Interfaces:**
- Consumes: `deelevate::spawn_medium_il` and the `DragJob`/`DragOutcome` shapes from Task 8; `files::resolve_paths` from Task 1.
- Produces: `Request::DragFiles { paths, x, y, start_x, start_y, opts }`, `Response::Drag(DragResult)`, and `fastuse_win::files::drag::drag_files(...)`.

- [ ] **Step 1: Add the wire types**

In `crates/fastuse-proto/src/wire.rs`, in `Request`:

```rust
    /// Drop files onto a screen coordinate via real OLE drag-and-drop.
    /// Runs in a de-elevated child process; see the design doc.
    DragFiles {
        /// Absolute paths. Redacted: paths carry usernames.
        paths: Redact<Vec<String>>,
        /// Drop target x, physical pixels, virtual-desktop origin.
        x: i32,
        /// Drop target y, physical pixels.
        y: i32,
        /// Where the drag starts. Defaults to a point on the target's
        /// monitor, away from the target itself.
        start_x: Option<i32>,
        /// See `start_x`.
        start_y: Option<i32>,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
```

In `Response`:

```rust
    /// Outcome of a `DragFiles`.
    Drag(DragResult),
```

And:

```rust
/// What `drag_files` observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DragResult {
    /// True when the target accepted the drop.
    pub dropped: bool,
    /// The `DROPEFFECT` the target reported (1 = copy).
    pub effect: u32,
}
```

In `crates/fastuse-core/src/perm.rs`:

```rust
    // Presses the real mouse button and walks the real cursor across the
    // desktop; same tier as the other tools that act on the user's session.
    ToolPerm { name: "drag_files", default_tier: Tier::Confirmed },
```

- [ ] **Step 2: Write the orchestration**

Create `crates/fastuse-win/src/files/drag.rs`:

```rust
//! Daemon-side half of `drag_files`.
//!
//! The daemon owns the mouse and the recovery; the de-elevated helper owns
//! the OLE object graph. Splitting it this way is forced by integrity levels
//! (see `deelevate.rs`), but it also puts the dangerous half — a held mouse
//! button — in the process that cannot crash without taking the daemon with
//! it.

use std::time::{Duration, Instant};

use fastuse_proto::{DragResult, Error as ProtoError, ErrorCode};

/// Hard ceiling on a single drag, matching the helper's own deadline.
const DRAG_DEADLINE_MS: u32 = 10_000;

/// Steps the cursor takes between start and target. Enough that the drop
/// target sees genuine `DragOver` traffic rather than a teleport.
const MOVE_STEPS: u32 = 20;
```

`drag_files` then:

1. `resolve_paths` first — before anything touches the mouse.
2. Pick `start_x`/`start_y` when not supplied: a point on the same monitor as `(x, y)` (use the existing monitor enumeration), offset far enough from the target that the initial button-down cannot land on the drop zone.
3. Spawn the helper with `spawn_medium_il(&current_exe, &["--drag-helper".to_string()])`, write the `DragJob` JSON to its stdin, and read the `{"ready":true}` line back. If the helper dies before that line, return `ErrorCode::HelperSpawnFailed`.
4. Inject the left-button-down at the start point through the input thread.
5. Walk the cursor to `(x, y)` in `MOVE_STEPS` steps with a short sleep between, then inject the left-button-up.
6. Read the `DragOutcome` line with a deadline.
7. **Recovery, unconditionally**, in a scope that runs on every exit path including the error ones: if the helper's process handle signals, or the deadline expires, inject a left-button-up anyway and kill the helper. A drag that fails must never leave the user's mouse button stuck down — `QueryContinueDrag`'s own deadline protects against a wedge *inside* the helper, and does nothing if the helper is killed. Implement this as a guard struct with a `Drop` impl rather than as a line at the bottom of the happy path, so an early `?` cannot skip it.
8. Map `dropped: false` to `Ok(DragResult { dropped: false, .. })`, not an error — the target legitimately refusing a drop is information, not a failure. Reserve `ErrorCode::DragFailed` for the helper reporting an error string.

- [ ] **Step 3: Add the dispatch arm**

In `crates/fastuse-daemon/src/dispatch.rs`, modelled on the `ClipboardSet` arm since this one **is** gated:

```rust
        Request::DragFiles { paths, x, y, start_x, start_y, opts } => {
            let inner = gate_then("drag_files", None, ctx, move |_| {
                let w = Instant::now();
                let r = fastuse_win::files::drag::drag_files(
                    paths.into_inner(), x, y, start_x, start_y,
                );
                (r.map(Response::Drag), w.elapsed().as_micros() as i64)
            })
            .await
            .into_response_or_err(&mut win32_us);
            finalize(inner, opts, ctx)
        }
```

`drag_files` needs the input-thread handle; take it from `ctx` the way the surrounding arms do, and pass it in as the first argument.

- [ ] **Step 4: Build and run the existing suite**

Run: `cargo test -p fastuse-win -p fastuse-proto -p fastuse-core -p fastuse-daemon`
Expected: PASS. There is no in-process check for this task — that is the honest position, and it is why the live verification in Task 10 is not optional.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-proto/src/wire.rs crates/fastuse-core/src/perm.rs crates/fastuse-win/src/files/drag.rs crates/fastuse-daemon/src/dispatch.rs
git commit -m "feat(win,proto,daemon): drag_files orchestration

The daemon spawns the medium-IL helper, walks the cursor, and guarantees a
button-up on every exit path via a Drop guard, so a crashed helper cannot
leave the user's mouse button held down."
```

---

## Task 10: `drag_files` — MCP tool, CLI subcommand, live verification

**Files:**
- Modify: `crates/fastuse-mcp/src/handler.rs`
- Modify: `crates/fastuse-cli/src/main.rs`, `crates/fastuse-cli/src/cmd_phase4.rs`
- Modify: `docs/permissions.md`, `CLAUDE.md`

**Interfaces:**
- Consumes: `Request::DragFiles` / `Response::Drag` from Task 9.
- Produces: MCP tool `drag_files`; CLI `fastuse-cli drag-files <paths...> --x N --y N`.

- [ ] **Step 1: Add the MCP args struct and tool**

```rust
#[derive(Deserialize, schemars::JsonSchema)]
pub struct DragFilesArgs {
    /// Absolute paths to drop.
    pub paths: Vec<String>,
    /// Drop target x, in the same coordinate space as `computer` clicks.
    pub x: i32,
    /// Drop target y.
    pub y: i32,
    /// Where the drag starts. Omit for an automatic point on the same monitor.
    pub start_x: Option<i32>,
    /// See `start_x`.
    pub start_y: Option<i32>,
}
```

```rust
    #[tool(
        name = "drag_files",
        description = "Drop files onto a screen coordinate with a real OLE drag-and-drop. Use for drop zones \
                       that have no file input and do not accept a paste. Takes the real cursor for a moment. \
                       Screenshot first: the coordinate must be current. Permission-gated (Confirmed tier)."
    )]
    async fn drag_files(
        &self,
        Parameters(args): Parameters<DragFilesArgs>,
    ) -> Result<Json<DragOutput>, McpError> {
        let req = Request::DragFiles {
            paths: Redact::new(args.paths),
            x: args.x,
            y: args.y,
            start_x: args.start_x,
            start_y: args.start_y,
            opts: None,
        };
        match self.call(req).await? {
            Response::Drag(r) => Ok(Json(DragOutput { dropped: r.dropped, effect: r.effect })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
```

With the output struct beside the other outputs:

```rust
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct DragOutput {
    pub dropped: bool,
    pub effect: u32,
}
```

Note: the MCP `computer` surface scales screenshots to ~1024-wide image space and the daemon translates back. Check how `tools/computer.rs` passes click coordinates through, and route `drag_files`'s `x`/`y` the same way — if MCP clicks are scaled and this is not, the agent will drop files in the wrong place while every click lands correctly, which is a nasty thing to debug.

- [ ] **Step 2: Add the CLI subcommand**

In `Cmd`:

```rust
    /// Drop files onto a coordinate via OLE drag-and-drop (permission-gated).
    DragFiles {
        /// Absolute paths.
        paths: Vec<String>,
        /// Drop target x, native virtual-desktop pixels.
        #[arg(long)]
        x: i32,
        /// Drop target y, native virtual-desktop pixels.
        #[arg(long)]
        y: i32,
        /// Drag start x.
        #[arg(long)]
        start_x: Option<i32>,
        /// Drag start y.
        #[arg(long)]
        start_y: Option<i32>,
    },
```

In the dispatch `match`:

```rust
            Cmd::DragFiles { paths, x, y, start_x, start_y } => {
                cmd_phase4::drag_files(&identity.path, paths, x, y, start_x, start_y).await
            }
```

In `cmd_phase4.rs`:

```rust
pub async fn drag_files(
    pipe_path: &str,
    paths: Vec<String>,
    x: i32,
    y: i32,
    start_x: Option<i32>,
    start_y: Option<i32>,
) -> anyhow::Result<()> {
    let req = Request::DragFiles {
        paths: Redact::new(paths), x, y, start_x, start_y, opts: None,
    };
    match one_call(pipe_path, req).await? {
        Response::Drag(r) => {
            println!("{}", json!({"ok": true, "dropped": r.dropped, "effect": r.effect}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}
```

- [ ] **Step 3: Live-verify against Chrome**

This is the only verification `drag_files` has. Open a page with a drop zone that has no file input — `https://www.google.com/drive` or any local test page with a `dragover`/`drop` handler and no `<input type=file>` — note the drop zone's coordinates from a screenshot, then:

```bash
./target/release/fastuse-cli.exe computer screenshot --out /tmp/dz.jpg
./target/release/fastuse-cli.exe drag-files "$TEMP/fastuse_drag_check.png" --x <X> --y <Y>
```

Expected: `"dropped": true` and the page showing the upload. Then verify the two failure paths deliberately, because they are the ones that hurt a user:

- Kill the helper mid-drag (`taskkill /F /PID <helper>` while the cursor is moving). Expected: the mouse button is released anyway and the desktop is left usable.
- Drop on a coordinate that accepts nothing, e.g. the desktop wallpaper. Expected: `"dropped": false` and no error, since a target refusing a drop is information rather than a failure.

- [ ] **Step 4: Document the gate and the tools**

Add to `docs/permissions.md` under "Default gated tools":

```markdown
- `drag_files` — takes the real cursor and holds the mouse button
```

And add all three tools to the MCP tool list in `CLAUDE.md`, beside the existing `mcp__fastuse__*` entries, so the next session knows they exist.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-mcp/src/handler.rs crates/fastuse-cli/src docs/permissions.md CLAUDE.md
git commit -m "feat(mcp,cli): expose drag_files, document the three upload tools"
```

---

## Notes for whoever executes this

Three things in this plan are load-bearing and easy to erode under time pressure.

The preferred-drop-effect blob in Task 5 is not a nicety. Without it a paste into Explorer **moves** the user's file. If a test for it starts failing, the bug is in the publish path, not in the assertion.

The `Drop`-guard button-up in Task 9 must stay a guard. Written as a line at the end of the happy path it silently stops running the first time someone adds an early `?` above it, and the failure mode is the user's mouse button stuck down.

Task 7's spike gates Tasks 8 through 10 entirely. If both de-elevation approaches fail, do not improvise a third — stop, report, and put the question back to the user, whose stated fallback is already recorded in the spec.
