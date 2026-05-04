//! Phase 2 window/monitor module — list_monitors, cursor_position,
//! foreground_window, list_windows, focus_window, resize_move_window.

pub mod cursor_position;
pub mod focus;
pub mod foreground;
pub mod list_windows;
pub mod monitors;
pub mod move_resize;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, MAX_PATH, RECT};
use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowRect, GetWindowThreadProcessId, IsWindow,
};

use fastuse_proto::{Error as ProtoError, ErrorCode, Rect, WindowInfo};

use crate::window::list_windows::safe_get_window_text;

/// Build a `WindowInfo` for a live HWND.
pub fn build_window_info(hwnd: HWND) -> Result<WindowInfo, ProtoError> {
    // SAFETY: IsWindow accepts any HWND value; returns FALSE on stale.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(window_not_found(hwnd.0 as u64));
    }

    // Title via the safe (timeout-bounded) helper.
    let title = safe_get_window_text(hwnd, 100).unwrap_or_default();

    // Class name.
    let mut class_buf = [0u16; 256];
    // SAFETY: class_buf is sized; HWND is live (just checked).
    let class_n = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    let class = if class_n > 0 {
        String::from_utf16_lossy(&class_buf[..class_n as usize])
    } else {
        String::new()
    };

    // PID + process basename.
    let mut pid: u32 = 0;
    // SAFETY: GetWindowThreadProcessId writes pid out.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let process_name = process_basename(pid).unwrap_or_default();

    // Bounds.
    let mut rect = RECT::default();
    // SAFETY: GetWindowRect writes a single RECT.
    let bounds = if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
        Rect {
            x: rect.left,
            y: rect.top,
            w: rect.right - rect.left,
            h: rect.bottom - rect.top,
        }
    } else {
        Rect { x: 0, y: 0, w: 0, h: 0 }
    };

    Ok(WindowInfo {
        hwnd: hwnd.0 as u64,
        title,
        class,
        process_name,
        pid,
        bounds,
    })
}

fn process_basename(pid: u32) -> Option<String> {
    // SAFETY: opening a process for query-only.
    let proc: HANDLE = unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return None,
        }
    };
    let mut buf = vec![0u16; MAX_PATH as usize];
    // SAFETY: buf is sized for MAX_PATH; len is in u16 units.
    let n = unsafe { GetModuleBaseNameW(proc, None, &mut buf) };
    let _ = unsafe { CloseHandle(proc) };
    if n == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

/// Build the standard `WindowNotFound` error.
pub fn window_not_found(hwnd: u64) -> ProtoError {
    ProtoError::new(
        ErrorCode::WindowNotFound,
        format!("HWND {hwnd:#x} is not a live top-level window"),
    )
}
