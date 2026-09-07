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
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_SETTEXT,
};

use crate::input_thread::InputThreadHandle;
use crate::uia_pool::UiaPoolHandle;
use crate::window::list_windows::list_windows;

/// Window class of every Win32 common dialog.
const DIALOG_CLASS: &str = "#32770";

/// How long a fresh dialog gets to appear before we fall back to one that was
/// already open. Short on purpose: an agent typically screenshots, sees the
/// dialog, and only then calls, so "already up" is the normal case. Capped by
/// the caller's own budget, so `wait_for_dialog_ms: 0` still means "it must
/// already be up".
const FRESH_DIALOG_GRACE_MS: u64 = 750;

/// Cross-process `SendMessageW` to a modal dialog blocks forever if its UI
/// thread is not pumping, so every message we send is timeout-bounded.
const SEND_TIMEOUT_MS: u32 = 500;

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
    let dialog = wait_for_dialog(scope_pid, &before, wait_for_dialog_ms)?;

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

/// Poll on the same 50ms cadence `wait_for_window` uses, preferring a dialog
/// that was not already up when the call started.
///
/// A process can be sitting on an unrelated `#32770` — a leftover prompt, or a
/// dialog the user opened themselves — and filling that one types a path into
/// something the caller never asked about. So a dialog absent from `before`
/// wins the moment it appears.
///
/// An already-open dialog is still usable once the grace has passed: the
/// screenshot-then-call flow means "already up" is the normal case, and
/// `wait_for_dialog_ms: 0` documents it outright (see `Request::FileDialogSet`).
fn wait_for_dialog(
    pid: u32,
    before: &[u64],
    timeout_ms: u32,
) -> Result<WindowInfo, ProtoError> {
    let started = Instant::now();
    let grace = Duration::from_millis(FRESH_DIALOG_GRACE_MS.min(timeout_ms as u64));
    let deadline = started + Duration::from_millis(timeout_ms as u64);
    loop {
        let open = dialogs_for_pid(pid)?;
        if let Some(w) = pick_dialog(&open, before, started.elapsed(), grace) {
            return Ok(w.clone());
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

/// The dialog to fill: one that was not already up, or — once `waited` has
/// reached `grace` — whichever was.
fn pick_dialog<'a>(
    open: &'a [WindowInfo],
    before: &[u64],
    waited: Duration,
    grace: Duration,
) -> Option<&'a WindowInfo> {
    open.iter()
        .find(|w| !before.contains(&w.hwnd))
        .or_else(|| if waited >= grace { open.first() } else { None })
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

/// Write the filename field through `ValuePattern`, then `WM_SETTEXT`.
fn fill_field(
    uia_pool: &UiaPoolHandle,
    dialog_hwnd: u64,
    value: &str,
) -> Result<String, ProtoError> {
    let v = value.to_string();
    let via_uia: bool = uia_pool.run(move |uia| {
        let root = crate::uia::cache::get_or_fetch(uia, dialog_hwnd)?;
        crate::uia::element_actions::set_value_first_match(uia, &root, &filename_selectors(), &v)
    })?;
    if via_uia {
        return Ok("value_pattern".to_string());
    }
    if dialog_wm_settext(dialog_hwnd, value)? {
        return Ok("wm_settext".to_string());
    }
    Err(ProtoError::new(
        ErrorCode::ElementNotFound,
        "dialog exposes no writable filename field".to_string(),
    ))
}

/// Where the filename field is, most specific first. The dialog's list view
/// exposes an `Edit` per visible column cell — dozens of them, ahead of the
/// real field in tree order — so an unqualified "first Edit" writes into a
/// column header and the dialog then refuses to close. `1148` is `cmb13`, the
/// filename combo of the Win11 `IFileDialog` this was verified against. The
/// class tier is the fallback for dialogs that publish another id: the noise
/// controls are `UIProperty` / `SearchEditBox`, never a plain `Edit`.
fn filename_selectors() -> Vec<Selector> {
    let edit = Selector::ByControlType(ControlType::Edit);
    vec![
        Selector::And(vec![edit.clone(), Selector::ByAutomationId("1148".into())]),
        Selector::And(vec![edit, Selector::ByClass("Edit".into())]),
    ]
}

/// `WM_SETTEXT` on the dialog's filename `Edit`, found by class walk: a bare
/// `Edit` child (legacy `GetOpenFileName`) or the one nested in the Vista+
/// `ComboBoxEx32` → `ComboBox` → `Edit` chain.
fn dialog_wm_settext(dialog_hwnd: u64, value: &str) -> Result<bool, ProtoError> {
    let dialog = HWND(dialog_hwnd as *mut core::ffi::c_void);
    let Some(edit) = find_edit(dialog) else {
        return Ok(false);
    };
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut out: usize = 0;
    // SAFETY: `wide` outlives the call; SMTO_ABORTIFHUNG + timeout bound the
    // wait on a dialog whose UI thread may not be pumping.
    let r = unsafe {
        SendMessageTimeoutW(
            edit,
            WM_SETTEXT,
            WPARAM(0),
            LPARAM(wide.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            SEND_TIMEOUT_MS,
            Some(&mut out as *mut usize as *mut _),
        )
    };
    Ok(r.0 != 0 && out != 0)
}

/// Locate the filename edit under `dialog`, or `None`.
fn find_edit(dialog: HWND) -> Option<HWND> {
    if let Some(e) = child(dialog, "Edit") {
        return Some(e);
    }
    let combo_ex = child(dialog, "ComboBoxEx32")?;
    let combo = child(combo_ex, "ComboBox")?;
    child(combo, "Edit")
}

fn child(parent: HWND, class: &str) -> Option<HWND> {
    let wide: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; a stale parent
    // HWND makes FindWindowExW fail rather than fault.
    unsafe { FindWindowExW(Some(parent), None, PCWSTR(wide.as_ptr()), PCWSTR::null()) }.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_path_is_unquoted_multi_is_quoted() {
        let one = join_for_field(&[std::path::PathBuf::from(r"C:\a\b.txt")]);
        assert_eq!(one, r"C:\a\b.txt");
        let two = join_for_field(&[
            std::path::PathBuf::from(r"C:\a\b.txt"),
            std::path::PathBuf::from(r"C:\a\c.txt"),
        ]);
        assert_eq!(two, "\"C:\\a\\b.txt\" \"C:\\a\\c.txt\"");
    }

    fn dialog(hwnd: u64) -> WindowInfo {
        WindowInfo {
            hwnd,
            title: "Save as".into(),
            class: DIALOG_CLASS.into(),
            process_name: "notepad.exe".into(),
            pid: 1,
            bounds: fastuse_proto::Rect { x: 0, y: 0, w: 1, h: 1 },
        }
    }

    const GRACE: Duration = Duration::from_millis(FRESH_DIALOG_GRACE_MS);
    const EARLY: Duration = Duration::from_millis(0);

    #[test]
    fn a_leftover_dialog_never_wins_over_a_fresh_one() {
        let open = vec![dialog(1), dialog(2)];
        // 1 was already up when the call started: filling it would type a path
        // into a dialog the caller never asked about.
        assert_eq!(pick_dialog(&open, &[1], EARLY, GRACE).map(|w| w.hwnd), Some(2));
        // Nothing new yet, and the grace has not passed — keep waiting.
        assert!(pick_dialog(&open[..1], &[1], EARLY, GRACE).is_none());
        // Even after the grace, a fresh dialog still beats the leftover.
        assert_eq!(pick_dialog(&open, &[1], GRACE, GRACE).map(|w| w.hwnd), Some(2));
        // No leftovers: the only dialog is the right one.
        assert_eq!(pick_dialog(&open, &[], EARLY, GRACE).map(|w| w.hwnd), Some(1));
    }

    #[test]
    fn after_the_grace_an_already_open_dialog_is_used() {
        let open = vec![dialog(1)];
        // The screenshot-then-call flow: the dialog was up before the request
        // landed, so waiting the whole budget for a "fresh" one would just be
        // latency the caller pays for nothing.
        assert_eq!(pick_dialog(&open, &[1], GRACE, GRACE).map(|w| w.hwnd), Some(1));
        // `wait_for_dialog_ms: 0` caps the grace to zero — accept immediately.
        assert_eq!(
            pick_dialog(&open, &[1], EARLY, Duration::ZERO).map(|w| w.hwnd),
            Some(1)
        );
        // Nothing open at all is still DialogNotFound's job, not this one's.
        assert!(pick_dialog(&[], &[1], GRACE, GRACE).is_none());
    }

    #[test]
    fn is_window_rejects_a_bogus_handle() {
        assert!(!crate::window::is_window(0));
    }
}
