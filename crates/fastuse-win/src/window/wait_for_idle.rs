//! `wait_for_idle` — block until a window's input queue drains AND focus has
//! actually been delivered to the window (or one of its descendants).
//!
//! Two-phase:
//!
//! 1. **Pump check.** `SendMessageTimeoutW(hwnd, WM_NULL, SMTO_BLOCK, t)` —
//!    the canonical Win32 way to prove the target thread's message loop is
//!    alive. Returns the moment the target processes the zero-cost ping.
//!
//! 2. **Focus settle.** Poll `GetGUIThreadInfo(thread_id).hwndFocus` until it
//!    equals `hwnd` or is a descendant of `hwnd` (via `IsChild`). Without this
//!    second phase, `wait_for_idle` returns the moment WM_NULL is acked — but
//!    Modern Notepad / WinUI / D2D-rendered apps deliver focus to the inner
//!    edit control on a separate cycle, and a subsequent `type` races the
//!    focus flush. Symptom: leading characters dropped, `:` eaten, whitespace
//!    garbled.
//!
//! Use between a `focus-window` and a follow-up `type` to guarantee the inner
//! edit control is ready to receive keystrokes.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, IsChild, IsWindow,
    SendMessageTimeoutW, GUITHREADINFO, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL,
};

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// Result of a `wait_for_idle` call.
pub struct WaitForIdleResult {
    /// Total wall-clock time spent waiting (milliseconds).
    pub waited_ms: u32,
    /// `true` if the window acknowledged our `WM_NULL` ping (queue drained);
    /// `false` if the pump-check timeout elapsed first.
    pub paint_observed: bool,
    /// `true` if the target HWND (or one of its descendants) holds keyboard
    /// focus on its owning GUI thread. `false` if the focus-settle phase
    /// timed out — typing is still likely to race.
    pub focus_settled: bool,
}

/// Block until `hwnd`'s input queue drains AND focus is delivered to `hwnd`
/// or one of its descendants. Passing `None` waits on the foreground window.
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
    let budget = Duration::from_millis(timeout_ms as u64);

    // --- Phase 1: pump check ---
    let mut result: usize = 0;
    let pump_budget_ms = timeout_ms.min(250); // pump should ack quickly; leave the rest for focus settle
    // SAFETY: SendMessageTimeoutW with a stack-local `result` slot is the
    // standard pump-drain idiom. SMTO_BLOCK lets us wait on a thread that
    // is itself blocked on another SendMessage. SMTO_ABORTIFHUNG bails
    // immediately if the target window proc is hung — avoids wasting our
    // budget on a frozen app. WM_NULL is the zero-cost ping — the window
    // proc returns 0 without touching state.
    let ret = unsafe {
        SendMessageTimeoutW(
            target_hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_BLOCK | SMTO_ABORTIFHUNG,
            pump_budget_ms,
            Some(&mut result as *mut usize),
        )
    };
    let acknowledged = ret.0 != 0;
    if !acknowledged {
        let waited_ms = start.elapsed().as_millis().min(u32::MAX as u128) as u32;
        return Ok(WaitForIdleResult {
            waited_ms,
            paint_observed: false,
            focus_settled: false,
        });
    }

    // --- Phase 2: focus settle ---
    // Pump acked. Now wait for the GUI thread to actually hold focus on
    // `target_hwnd` or one of its descendants. Poll at ~8ms cadence to
    // catch focus delivery without burning CPU.
    //
    // GetWindowThreadProcessId returns the thread that owns the window;
    // GetGUIThreadInfo on that thread tells us what HWND currently has
    // keyboard focus *within that thread's GUI state*.
    //
    // SAFETY: GetWindowThreadProcessId accepts any HWND, returns 0 on
    // failure. GetGUIThreadInfo with a properly-sized GUITHREADINFO and
    // valid thread id is safe.
    let thread_id = unsafe { GetWindowThreadProcessId(target_hwnd, None) };
    let mut focus_settled = false;
    if thread_id != 0 {
        loop {
            let elapsed = start.elapsed();
            if elapsed >= budget {
                break;
            }
            let mut gti = GUITHREADINFO {
                cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: see above.
            let ok = unsafe { GetGUIThreadInfo(thread_id, &mut gti as *mut _) };
            if ok.is_ok() {
                let focus = gti.hwndFocus;
                if !focus.is_invalid() {
                    if focus == target_hwnd {
                        focus_settled = true;
                        break;
                    }
                    // SAFETY: IsChild accepts any HWND pair.
                    if unsafe { IsChild(target_hwnd, focus) }.as_bool() {
                        focus_settled = true;
                        break;
                    }
                }
            }
            // Sleep a short interval before re-polling. Cap by remaining budget.
            let remaining = budget.saturating_sub(elapsed);
            let sleep_for = remaining.min(Duration::from_millis(8));
            if sleep_for.is_zero() {
                break;
            }
            std::thread::sleep(sleep_for);
        }
    }

    let waited_ms = start.elapsed().as_millis().min(u32::MAX as u128) as u32;
    Ok(WaitForIdleResult {
        waited_ms,
        paint_observed: acknowledged,
        focus_settled,
    })
}
