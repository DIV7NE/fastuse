//! `focus_window(hwnd)` via the AttachThreadInput dance to bypass
//! `SetForegroundWindow`'s lockout heuristic (PITFALLS #6).

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
    SetForegroundWindow, ASFW_ANY,
};

use fastuse_proto::{Error as ProtoError, ErrorCode};

use crate::window::{list_windows::invalidate_cache, window_not_found};

/// Bring `hwnd` to the foreground. Returns `WindowNotFound` if HWND is
/// stale.
pub fn focus_window(hwnd_raw: u64) -> Result<(), ProtoError> {
    let hwnd = HWND(hwnd_raw as *mut _);
    // SAFETY: IsWindow accepts any value.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(window_not_found(hwnd_raw));
    }
    // SAFETY: GetWindowThreadProcessId is always safe.
    let target_thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
    // SAFETY: GetForegroundWindow is always safe.
    let fg = unsafe { GetForegroundWindow() };
    // SAFETY: GetWindowThreadProcessId on possibly-null fg is safe (returns 0).
    let fg_thread = unsafe { GetWindowThreadProcessId(fg, None) };
    // SAFETY: GetCurrentThreadId is always safe.
    let our_thread = unsafe { GetCurrentThreadId() };

    // Best-effort: AllowSetForegroundWindow lifts the lockout on our
    // children. Returns FALSE if not entitled — we don't treat that as
    // fatal.
    // SAFETY: ASFW_ANY is the documented sentinel.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };

    // Attach our input queue to the target thread (and the current
    // foreground thread) so SetForegroundWindow won't be denied.
    let attach1 = if fg_thread != 0 && fg_thread != our_thread {
        // SAFETY: AttachThreadInput accepts any TIDs; returns FALSE if invalid.
        unsafe { AttachThreadInput(our_thread, fg_thread, true) }.as_bool()
    } else {
        false
    };
    let attach2 = if target_thread != 0 && target_thread != our_thread {
        // SAFETY: same.
        unsafe { AttachThreadInput(our_thread, target_thread, true) }.as_bool()
    } else {
        false
    };

    // SAFETY: SetForegroundWindow is always safe.
    let r = unsafe { SetForegroundWindow(hwnd) };

    // Detach in reverse order. Drop attachments unconditionally — we don't
    // care about their return values.
    if attach2 {
        // SAFETY: AttachThreadInput symmetric detach.
        let _ = unsafe { AttachThreadInput(our_thread, target_thread, false) };
    }
    if attach1 {
        // SAFETY: AttachThreadInput symmetric detach.
        let _ = unsafe { AttachThreadInput(our_thread, fg_thread, false) };
    }

    if !r.as_bool() {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("SetForegroundWindow on HWND {hwnd_raw:#x} returned FALSE"),
        )
        .with_hint(
            "the AttachThreadInput dance was attempted; some applications (full-screen games, secure desktop) cannot be focused programmatically".to_string(),
        ));
    }

    invalidate_cache();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_hwnd_returns_window_not_found() {
        let r = focus_window(0xDEAD_BEEF_DEAD_BEEF);
        match r {
            Err(e) => assert_eq!(e.code, ErrorCode::WindowNotFound),
            Ok(()) => panic!("expected WindowNotFound"),
        }
    }
}
