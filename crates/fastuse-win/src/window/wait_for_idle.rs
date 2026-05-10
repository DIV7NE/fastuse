//! `wait_for_idle` — block until a window's input queue drains.
//!
//! Heuristic: `SendMessageTimeout(hwnd, WM_NULL, 0, 0, SMTO_BLOCK, t, _)` is
//! the canonical Win32 way to wait for a window's message loop to reach the
//! next pump cycle — the call returns the moment the target thread processes
//! our zero-cost WM_NULL. After that we sleep one ~16ms frame so any paint
//! triggered by the just-completed work has a chance to land before the
//! caller fires its next click/type.
//!
//! Use between a `click` and a follow-up `type` when the app debounces or
//! when an ImGui-style focus shift needs a frame to settle.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsWindow, SendMessageTimeoutW, SMTO_BLOCK, WM_NULL,
};

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// Result of a `wait_for_idle` call.
pub struct WaitForIdleResult {
    /// Total wall-clock time spent waiting (milliseconds).
    pub waited_ms: u32,
    /// `true` if the window acknowledged our `WM_NULL` ping (queue drained);
    /// `false` if the timeout elapsed first.
    pub paint_observed: bool,
}

/// Block until `hwnd`'s input queue drains, or `timeout_ms` elapses. Passing
/// `None` waits on the foreground window.
pub fn wait_for_idle(hwnd: Option<u64>, timeout_ms: u32) -> Result<WaitForIdleResult, ProtoError> {
    let target_hwnd: HWND = match hwnd {
        Some(h) => HWND(h as *mut core::ffi::c_void),
        None => {
            // SAFETY: always safe.
            let h = unsafe { GetForegroundWindow() };
            if h.0 as isize == 0 {
                return Err(ProtoError::new(
                    ErrorCode::WindowNotFound,
                    "no foreground window to wait on".to_string(),
                ));
            }
            h
        }
    };
    // SAFETY: IsWindow accepts any HWND, returns false on stale.
    if !unsafe { IsWindow(Some(target_hwnd)) }.as_bool() {
        return Err(ProtoError::new(
            ErrorCode::WindowNotFound,
            format!("HWND {:#x} is not a live window", target_hwnd.0 as usize),
        ));
    }

    let start = Instant::now();
    let mut result: usize = 0;
    // SAFETY: SendMessageTimeoutW with a stack-local `result` slot is the
    // standard pump-drain idiom. SMTO_BLOCK lets us wait on a thread that
    // is itself blocked on another SendMessage. WM_NULL is the zero-cost
    // ping message — the window proc returns 0 without touching state.
    let ret = unsafe {
        SendMessageTimeoutW(
            target_hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_BLOCK,
            timeout_ms,
            Some(&mut result as *mut usize),
        )
    };
    let acknowledged = ret.0 != 0;
    // One frame budget for the paint that the queue-drain probably triggered.
    if acknowledged {
        std::thread::sleep(Duration::from_millis(16));
    }
    let waited_ms = start.elapsed().as_millis().min(u32::MAX as u128) as u32;
    Ok(WaitForIdleResult {
        waited_ms,
        paint_observed: acknowledged,
    })
}
