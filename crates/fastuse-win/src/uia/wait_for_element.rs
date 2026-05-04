//! `wait_for_element` handler (Phase 3 Task 17, UIA-07).
//!
//! Polls at 50ms cadence on the dispatch thread, dispatching each
//! `find_first` probe through the UIA pool. The polling itself does not
//! occupy a pool worker — only each probe does — so other tools remain
//! responsive. Default timeout 5000ms when caller passes 0.
//!
//! WR-04: The handler is invoked from inside a `tokio::task::spawn_blocking`
//! worker (see `daemon::server::serve_connection`). When a Tokio runtime
//! handle is reachable from this worker we use `tokio::time::sleep` via
//! `Handle::block_on` so cooperative async sleeps replace pure thread
//! parking — keeping the blocking pool less stressed under fan-out.
//! When no runtime handle is reachable (tests / non-tokio harness), we
//! fall back to `std::thread::sleep`.

use std::time::{Duration, Instant};

use fastuse_proto::{Error as ProtoError, ErrorCode, Response, Selector};

use crate::uia::automation::foreground_hwnd;
use crate::uia::cache::get_or_fetch;
use crate::uia::find::find_first;
use crate::uia_pool::UiaPoolHandle;

const POLL_CADENCE: Duration = Duration::from_millis(50);
const DEFAULT_TIMEOUT_MS: u32 = 5000;

/// Sleep cooperatively if we're on a Tokio runtime; otherwise block.
fn wait_for(d: Duration) {
    if d.is_zero() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(tokio::time::sleep(d)),
        Err(_) => std::thread::sleep(d),
    }
}

/// Handle a `Request::WaitForElement`.
///
/// Returns `Response::Element { matched: true }` on first match, or
/// `matched: false` on timeout. We do NOT return Err on timeout — it's a
/// valid signal to the caller, not a failure.
pub fn handle_wait_for_element(
    pool: &UiaPoolHandle,
    selector: Selector,
    timeout_ms: u32,
) -> Result<Response, ProtoError> {
    let timeout = Duration::from_millis(if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS as u64
    } else {
        timeout_ms as u64
    });
    let deadline = Instant::now() + timeout;

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(Response::Element { matched: false });
        }

        // Each probe is a fresh closure dispatched onto the pool. Foreground
        // HWND can change between probes — re-resolve every iteration so we
        // wait against whichever window is current.
        let sel = selector.clone();
        let hwnd = match foreground_hwnd() {
            Some(h) => h,
            None => {
                // No foreground window right now — sleep + retry.
                wait_for(POLL_CADENCE);
                continue;
            }
        };
        let probe: Result<Option<fastuse_proto::UIANode>, ProtoError> = pool.run(move |uia| {
            let root = get_or_fetch(uia, hwnd).map_err(|_e| {
                ProtoError::new(
                    ErrorCode::WindowNotFound,
                    "foreground HWND vanished".to_string(),
                )
            })?;
            find_first(uia, &root, &sel)
        });
        match probe {
            Ok(Some(_)) => return Ok(Response::Element { matched: true }),
            Ok(None) => { /* keep polling */ }
            Err(_) => { /* transient: keep polling */ }
        }
        // Sleep what's left of the cadence.
        let remaining = POLL_CADENCE.saturating_sub(now.elapsed());
        if !remaining.is_zero() {
            wait_for(remaining);
        }
    }
}
