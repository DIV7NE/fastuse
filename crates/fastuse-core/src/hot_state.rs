//! Daemon-side hot caches that downstream phases populate.
//!
//! Phase 1 ships empty placeholders so the dispatcher API is stable from day one.
//! Phase 2 fills `foreground_hwnd_cache`/`monitor_cache`; Phase 3 fills
//! `uia_root_cache`.

use std::collections::HashMap;

/// Foreground HWND cache slot (Phase 2 fills shape).
#[derive(Debug, Default, Clone)]
pub struct ForegroundHwndCache {
    /// Last observed foreground HWND value (raw isize).
    pub last_hwnd: Option<isize>,
}

/// Monitor enumeration cache slot (Phase 2 fills shape).
#[derive(Debug, Default, Clone)]
pub struct MonitorCache {
    /// Last enumerated monitor handles (raw isize values).
    pub handles: Vec<isize>,
}

/// UIA root element cache slot (Phase 3 fills shape).
#[derive(Debug, Default, Clone)]
pub struct UiaRootCache {
    /// Per-HWND root-element generation counter (real cache lives in fastuse-win).
    pub generations: HashMap<isize, u64>,
}

/// Hot state shared across daemon dispatcher and worker threads.
#[derive(Debug, Default, Clone)]
pub struct HotState {
    /// Foreground HWND cache (Phase 2).
    pub foreground_hwnd_cache: ForegroundHwndCache,
    /// Monitor list cache (Phase 2).
    pub monitor_cache: MonitorCache,
    /// UIA root-element cache (Phase 3).
    pub uia_root_cache: UiaRootCache,
}
