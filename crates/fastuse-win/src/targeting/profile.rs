//! Window/element fingerprinting. Produces `WindowSignals` and a ranked
//! candidate list. Cached per composite key (hwnd, pid, process_start, gen).

use dashmap::DashMap;
use fastuse_proto::coords::Rect;
use std::sync::OnceLock;
use windows::Win32::Foundation::FILETIME;

use crate::targeting::candidate::TargetCandidate;

/// Composite cache key. HWND alone is unsafe — Windows reuses HWND values on
/// long-lived sessions; without `pid` + `process_start_time` a stale entry can
/// alias a new process at the same handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProfileCacheKey {
    /// Top-level HWND of the target window, cast to u64 for cross-thread reuse.
    pub hwnd: u64,
    /// Owning process ID.
    pub pid: u32,
    /// Process creation time low 64 bits (`FILETIME` flattened). Stable for
    /// the lifetime of the process; differs across reincarnations of the same PID.
    pub process_start: u64,
    /// Bumped whenever we detect a window-rect / DPI / display-topology change
    /// that invalidates cached candidate bounds.
    pub generation: u64,
}

impl ProfileCacheKey {
    /// Flatten a `FILETIME` into a single u64.
    pub fn flatten_filetime(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
    }
}

/// Heuristic verdict on whether the UIA tree exposed for this window is rich
/// enough to act on directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeQuality {
    /// Named descendants with AutomationIds, multiple ControlTypes — ordinary.
    Healthy,
    /// Some named descendants, but anonymous Pane regions dominate (Electron
    /// with assistive tech enabled, Qt with QtAccessibilityPlugin).
    Mixed,
    /// Mostly anonymous Pane elements with no AutomationIds — Electron without
    /// assistive tech, custom canvas renderers, games.
    Degraded,
}

/// Process integrity level — for UIPI gate before action attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityLevel {
    /// Lower than ours — fine, but we shouldn't be elevated against it.
    Low,
    /// Equal to ours — UIPI permits.
    Medium,
    /// Higher than ours — UIPI blocks SendInput. Return PermissionRequired
    /// before attempting.
    High,
    /// Could not determine.
    Unknown,
}

/// Signals describing the target window. Hint, not authority — `framework_id`
/// can lie (mixed-provider Electron+WebView2), `window_class` is the hard backstop.
#[derive(Debug, Clone)]
pub struct WindowSignals {
    /// `IUIAutomation::NativeWindowHandle.FrameworkId` — provider-reported,
    /// may be missing/stale for mixed-provider apps.
    pub framework_id: Option<String>,
    /// `GetClassNameW` of the target HWND. Always populated.
    pub window_class: String,
    /// `GetClassNameW` of every visible child HWND, depth ≤ 2. Surfaces
    /// markers like `Chrome_RenderWidgetHostHWND`, `WebView2`, `Qt*`,
    /// `Windows.UI.Core.CoreWindow`.
    pub child_classes: Vec<String>,
    /// Two-level UIA probe verdict.
    pub uia_tree_quality: TreeQuality,
    /// Process integrity level vs ours.
    pub integrity_level: IntegrityLevel,
    /// `DwmGetWindowAttribute(DWMWA_CLOAKED)` returned non-zero.
    pub cloaked: bool,
    /// `IsIconic` true.
    pub minimized: bool,
    /// Bounding rect overlaps a topmost window owned by another process.
    pub occluded: bool,
    /// `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` — modern apps
    /// have window rect ≠ visible frame.
    pub dwm_extended_frame_bounds: Rect,
    /// `GetForegroundWindow() == hwnd` (foreground eligibility hint).
    pub is_foreground: bool,
}

/// Full target profile = window signals + ranked candidates.
#[derive(Debug, Clone)]
pub struct TargetProfile {
    /// Per-window signals.
    pub window: WindowSignals,
    /// Candidates sorted by `score` descending; top one is the chosen target
    /// unless overridden by selector specificity.
    pub candidates: Vec<TargetCandidate>,
}

static PROFILE_CACHE: OnceLock<DashMap<ProfileCacheKey, TargetProfile>> = OnceLock::new();

fn cache() -> &'static DashMap<ProfileCacheKey, TargetProfile> {
    PROFILE_CACHE.get_or_init(DashMap::new)
}

/// Resolve or build a profile for `hwnd`. Returns the cached profile if the
/// composite key still matches. Real implementation lives behind a closure
/// dispatched to `uia_pool` (Task 3 wires the actual probing).
pub fn profile_window(_hwnd: u64) -> Result<TargetProfile, ProfileError> {
    // Stub — Task 3 implements the real probe via uia_pool.
    Err(ProfileError::NotImplemented)
}

/// Drop a cache entry. Called from process-exit / window-destroy hooks (already
/// wired in `fastuse-win/src/uia/cache.rs`).
pub fn invalidate_profile_cache(key: ProfileCacheKey) {
    cache().remove(&key);
}

/// Errors from profile construction.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// Phase-1 stub — replaced in Task 3.
    #[error("profile_window not yet implemented")]
    NotImplemented,
    /// HWND is no longer valid.
    #[error("window vanished or HWND invalid")]
    WindowGone,
    /// UIA pool unavailable.
    #[error("uia pool unavailable")]
    UiaUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_round_trip() {
        let k = ProfileCacheKey {
            hwnd: 0xdead_beef,
            pid: 4242,
            process_start: 0x0123_4567_89ab_cdef,
            generation: 7,
        };
        let copy = k;
        assert_eq!(k, copy);
    }

    #[test]
    fn flatten_filetime_layout() {
        let ft = FILETIME {
            dwHighDateTime: 0x1122_3344,
            dwLowDateTime: 0x5566_7788,
        };
        assert_eq!(
            ProfileCacheKey::flatten_filetime(ft),
            0x1122_3344_5566_7788_u64
        );
    }
}
