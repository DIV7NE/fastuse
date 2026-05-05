//! `wait_for_window` — poll until a matching top-level window appears.

use std::time::{Duration, Instant};

use fastuse_proto::{wire::WaitForWindowRequest, WindowInfo};

use crate::window::list_windows::list_windows;

/// Poll `list_windows` at 50 ms cadence until a window matching the request
/// filters appears or `timeout_ms` elapses.
///
/// Returns the first matching [`WindowInfo`] on success, or `None` if the
/// deadline expired before any match was found.
pub fn wait_for_window(req: &WaitForWindowRequest) -> Option<WindowInfo> {
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms as u64);
    loop {
        let list = list_windows(
            req.process_name.as_deref(),
            req.title_substr.as_deref(),
            true,
        );
        if let Ok(windows) = list {
            if let Some(first) = windows.into_iter().next() {
                return Some(first);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
