//! Idle watcher: posts shutdown when no client has been connected for the
//! configured timeout.
#![allow(missing_docs)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct ActivityClock {
    /// Number of currently-connected clients.
    pub active: AtomicU64,
    /// Wall-clock micros (since process start) of last meaningful activity.
    pub last_activity_us: AtomicU64,
}

impl ActivityClock {
    pub fn note_connect(&self) {
        self.active.fetch_add(1, Ordering::SeqCst);
    }
    pub fn note_disconnect(&self, base: Instant) {
        let _ = self.active.fetch_sub(1, Ordering::SeqCst);
        self.last_activity_us
            .store(base.elapsed().as_micros() as u64, Ordering::SeqCst);
    }
    pub fn touch(&self, base: Instant) {
        self.last_activity_us
            .store(base.elapsed().as_micros() as u64, Ordering::SeqCst);
    }
}

/// Spawn the idle watcher tokio task. Sets `shutdown_flag` to true when no
/// clients have been connected for `timeout_secs` consecutive seconds.
/// `timeout_secs == 0` disables the watcher (never times out).
pub fn spawn_watcher(
    clock: Arc<ActivityClock>,
    shutdown_flag: Arc<AtomicBool>,
    timeout_secs: u64,
    base: Instant,
) {
    if timeout_secs == 0 {
        return;
    }
    tokio::spawn(async move {
        let timeout = Duration::from_secs(timeout_secs);
        // Initialize last_activity to "now" so the timer starts fresh.
        clock.touch(base);
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if shutdown_flag.load(Ordering::SeqCst) {
                return;
            }
            if clock.active.load(Ordering::SeqCst) > 0 {
                clock.touch(base);
                continue;
            }
            let last_us = clock.last_activity_us.load(Ordering::SeqCst);
            let now_us = base.elapsed().as_micros() as u64;
            if now_us.saturating_sub(last_us) >= timeout.as_micros() as u64 {
                tracing::info!(timeout_secs, "idle timeout reached; shutting down");
                shutdown_flag.store(true, Ordering::SeqCst);
                return;
            }
        }
    });
}
