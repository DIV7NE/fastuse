//! Phase 2 window/monitor module — list_monitors, cursor_position,
//! foreground_window, list_windows, focus_window, resize_move_window,
//! wait_for_window.

pub mod cursor_position;
pub mod focus;
pub mod foreground;
pub mod list_windows;
pub mod monitors;
pub mod move_resize;
pub mod wait_for_idle;
pub mod wait_for_window;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, MAX_PATH, RECT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
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
    // SAFETY: opening a process with QUERY_LIMITED_INFORMATION (works for
    // most foreign processes including elevated ones in our session).
    let proc: HANDLE = unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return None,
        }
    };
    let mut buf = vec![0u16; MAX_PATH as usize];
    let mut len: u32 = buf.len() as u32;
    // QueryFullProcessImageNameW works with PROCESS_QUERY_LIMITED_INFORMATION
    // (GetModuleBaseNameW requires PROCESS_VM_READ which we deliberately do
    // not request — Rule 1 fix discovered during dpi_mixed E2E).
    // SAFETY: proc is a valid handle; buf/len describe the output buffer.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            proc,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(proc) };
    if ok.is_err() || len == 0 {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    // Basename: strip everything up to and including the last backslash.
    let basename = full
        .rsplit_once('\\')
        .map(|(_, b)| b.to_string())
        .unwrap_or(full);
    Some(basename)
}

/// Build the standard `WindowNotFound` error.
pub fn window_not_found(hwnd: u64) -> ProtoError {
    ProtoError::new(
        ErrorCode::WindowNotFound,
        format!("HWND {hwnd:#x} is not a live top-level window"),
    )
}
