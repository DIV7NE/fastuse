//! UIA automation helpers (Phase 3 Task 08, UIA-01).
//!
//! The actual `IUIAutomation` singleton lives in `uia_pool` (a `OnceLock`
//! initialized inside the first MTA worker). This module re-exports a thin
//! convenience helper for resolving the foreground HWND from inside a UIA
//! pool closure, plus the owning-process-name lookup used by the degraded
//! heuristic.

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Return the current foreground HWND as `u64` (proto-friendly), or `None`
/// when there is no foreground window (e.g. lock screen, switching).
pub fn foreground_hwnd() -> Option<u64> {
    // SAFETY: GetForegroundWindow is safe to call from any thread; returns
    // 0 when there is no foreground window.
    let h = unsafe { GetForegroundWindow() };
    if h.is_invalid() {
        None
    } else {
        Some(h.0 as u64)
    }
}

/// Best-effort process basename for the given HWND. Same approach as Phase 2
/// `window::list_windows` (PROCESS_QUERY_LIMITED_INFORMATION +
/// QueryFullProcessImageNameW). Returns `None` on any failure — the degraded
/// heuristic still runs against the count branch.
pub fn process_name_for_hwnd(hwnd: u64) -> Option<String> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let mut pid: u32 = 0;
    // SAFETY: GetWindowThreadProcessId only writes to the out-pid pointer.
    let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: opening with limited info; closed below via CloseHandle.
    let proc_h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    // SAFETY: filled with up to `size` UTF-16 code units; size is updated.
    let res = unsafe {
        QueryFullProcessImageNameW(
            proc_h,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: matched by the OpenProcess above.
    let _ = unsafe { CloseHandle(proc_h) };
    res.ok()?;

    // WR-05: defensive clamp. On API success `size` is the count of UTF-16
    // code units written (excluding the NUL terminator), but a malformed
    // driver returning size > buf.len() would panic in the slice. Clamp.
    let size = (size as usize).min(buf.len());
    let path = String::from_utf16_lossy(&buf[..size]);
    let basename = path
        .rsplit_once('\\')
        .map(|(_, b)| b.to_string())
        .unwrap_or(path);
    Some(basename)
}
