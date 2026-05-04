//! `foreground_window()` — current foreground HWND → `WindowInfo`.

use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use fastuse_proto::{Error as ProtoError, WindowInfo};

use crate::window::{build_window_info, window_not_found};

/// Return the foreground window's `WindowInfo`.
pub fn foreground_window() -> Result<WindowInfo, ProtoError> {
    // SAFETY: always safe.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 as isize == 0 {
        return Err(window_not_found(0));
    }
    build_window_info(hwnd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_some_window_or_window_not_found() {
        // Headless / no-foreground systems return WindowNotFound; otherwise
        // a populated WindowInfo. Both are acceptable; we just shouldn't
        // panic.
        let _ = foreground_window();
    }
}
