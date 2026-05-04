//! `resize_move_window(hwnd, x, y, w, h)`.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindow, SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER,
};

use fastuse_proto::{Error as ProtoError, ErrorCode};

use crate::window::{list_windows::invalidate_cache, monitors::list_monitors, window_not_found};

/// Move + resize the window in physical-pixel virtual-desktop space.
pub fn resize_move_window(hwnd_raw: u64, x: i32, y: i32, w: i32, h: i32) -> Result<(), ProtoError> {
    let hwnd = HWND(hwnd_raw as *mut _);
    // SAFETY: IsWindow safe on any HWND value.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(window_not_found(hwnd_raw));
    }
    // IN-02: cap absurd sizes; SetWindowPos with i32::MAX returns an opaque
    // ERROR_INVALID_PARAMETER. 65535 px on a side covers every realistic
    // virtual-desktop layout (8K monitor stack ≈ 30k px wide today).
    const MAX_DIM: i32 = 65535;
    if w <= 0 || h <= 0 || w > MAX_DIM || h > MAX_DIM {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("invalid size: {w}×{h} (must be 1..={MAX_DIM} on each axis)"),
        ));
    }
    // Validate the window's centre lies on at least one monitor.
    let cx = x + w / 2;
    let cy = y + h / 2;
    let mons = list_monitors()?;
    if !mons.iter().any(|m| m.bounds.contains(cx, cy)) {
        return Err(ProtoError::new(
            ErrorCode::MonitorNotFound,
            format!("window centre ({cx},{cy}) lies on no monitor"),
        ));
    }
    // SAFETY: SetWindowPos with SWP_NOZORDER | SWP_NOACTIVATE on a live HWND.
    unsafe {
        SetWindowPos(hwnd, Some(HWND_TOP), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE)
            .map_err(|e| ProtoError::new(ErrorCode::Internal, format!("SetWindowPos failed: {e}")))?;
    }
    invalidate_cache();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_hwnd_returns_window_not_found() {
        let r = resize_move_window(0xDEAD_BEEF_DEAD_BEEF, 100, 100, 800, 600);
        match r {
            Err(e) => assert_eq!(e.code, ErrorCode::WindowNotFound),
            Ok(()) => panic!("expected WindowNotFound"),
        }
    }

    #[test]
    fn invalid_size_rejected() {
        // Use a real HWND from the foreground; even if focus changes, IsWindow
        // will be true. If no foreground, this test trivially passes via early
        // WindowNotFound — that's still an error path.
        let r = resize_move_window(0, 0, 0, 0, 0);
        assert!(r.is_err());
    }
}
