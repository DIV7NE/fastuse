//! Foreground HWND → UIA root cache (Phase 3 Task 11 partial).
//!
//! Strategy: keep the cache simple and TTL-bounded. The plan calls for a
//! `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` to drive eager invalidation;
//! that hook is wired separately on the input thread (see Phase 2's
//! message-only window) and calls [`invalidate`] from its callback.
//!
//! For now we also expose a 250 ms TTL so even without the hook, agents
//! never observe a stale root for more than a quarter second.
//!
//! Resolution rule (UIA-09): `get_or_fetch(hwnd)` returns the cached root
//! if it was fetched <250 ms ago; otherwise fetches via
//! `IUIAutomation::ElementFromHandle` and inserts.
//!
//! Note on threading: `IUIAutomationElement` is COM-affine but `UIAutomation`
//! is documented free-threaded. The cache stores cloned `UIElement` handles
//! — they're refcounted COM proxies and are valid on the calling MTA pool
//! worker. We never share an entry across non-MTA threads.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fastuse_proto::Error as ProtoError;
use once_cell::sync::Lazy;
use uiautomation::{UIAutomation, UIElement};
use uiautomation::types::Handle;
use windows::Win32::Foundation::HWND;

/// Cache TTL (UIA-09).
pub const TTL: Duration = Duration::from_millis(250);

struct CacheEntry {
    root: UIElement,
    fetched_at: Instant,
}

// SAFETY: UIElement is a COM proxy. We only ever borrow entries from the
// MTA UIA pool worker thread that fetched them. The Mutex serializes access
// across pool workers, and IUIAutomationElement is itself thread-safe to
// pass between MTA threads (Microsoft's free-threaded marshaller handles it).
unsafe impl Send for CacheEntry {}

static CACHE: Lazy<Mutex<HashMap<u64, CacheEntry>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Resolve the UIA root for a given HWND, using the TTL cache.
///
/// WR-02: The lock is held across the COM fetch so two pool workers
/// racing on the same HWND do not both pay the ~5–50ms `element_from_handle`
/// round-trip. Serializing UIA root resolution is acceptable: each fetch
/// is bounded and the cache's whole purpose is to coalesce concurrent
/// callers onto one cached root.
pub fn get_or_fetch(uia: &UIAutomation, hwnd: u64) -> Result<UIElement, ProtoError> {
    let mut guard = CACHE
        .lock()
        .map_err(|e| ProtoError::new(fastuse_proto::ErrorCode::Internal, format!("cache poisoned: {e}")))?;

    // Fresh hit: return the cached root.
    if let Some(entry) = guard.get(&hwnd) {
        if entry.fetched_at.elapsed() < TTL {
            return Ok(entry.root.clone());
        }
        // Stale; evict before fetching.
        guard.remove(&hwnd);
    }

    // Miss / stale: fetch under the lock so concurrent racers wait for us.
    let h = HWND(hwnd as *mut core::ffi::c_void);
    let element = uia
        .element_from_handle(Handle::from(h))
        .map_err(|e| ProtoError::new(fastuse_proto::ErrorCode::WindowNotFound, format!("element_from_handle({hwnd}): {e}")))?;
    guard.insert(
        hwnd,
        CacheEntry {
            root: element.clone(),
            fetched_at: Instant::now(),
        },
    );
    Ok(element)
}

/// Invalidate one HWND's cache entry. Called by the win-event hook
/// (Phase 3 Task 11 follow-up) when EVENT_SYSTEM_FOREGROUND fires.
pub fn invalidate(hwnd: u64) {
    if let Ok(mut guard) = CACHE.lock() {
        guard.remove(&hwnd);
    }
}

/// Drop every cached entry. Useful on `WM_DISPLAYCHANGE` and during shutdown.
pub fn invalidate_all() {
    if let Ok(mut guard) = CACHE.lock() {
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidate_all_clears_state() {
        // Sanity: invalidate_all does not panic on an already-empty cache.
        invalidate_all();
        assert!(CACHE.lock().unwrap().is_empty());
    }
}
