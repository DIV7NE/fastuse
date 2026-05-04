//! Per-Monitor-Aware-V2 DPI bootstrap (D-24).
//!
//! Must be the first executable stmt of every binary's `main()`. Enforced by
//! `cargo xtask check-firstcall`.

use windows::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// Outcome of [`set_per_monitor_v2_first_call`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiOutcome {
    /// Process is now PerMonitorV2 because we set it.
    Set,
    /// Manifest already set PerMonitorV2 — `SetProcessDpiAwarenessContext`
    /// returned `ERROR_ACCESS_DENIED`. Treat as success.
    AlreadySetByManifest,
}

/// Set the process DPI awareness to PerMonitorV2.
///
/// This MUST be the first executable statement in `main()`. Tolerates the
/// `ERROR_ACCESS_DENIED` case where the manifest beat us to it.
pub fn set_per_monitor_v2_first_call() -> DpiOutcome {
    // SAFETY: SetProcessDpiAwarenessContext is a static API; the constant
    // PER_MONITOR_AWARE_V2 is process-global state owned by the OS.
    let res = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if res.is_ok() {
        return DpiOutcome::Set;
    }
    // SAFETY: GetLastError is always callable.
    let err = unsafe { GetLastError() };
    if err == ERROR_ACCESS_DENIED {
        return DpiOutcome::AlreadySetByManifest;
    }
    // Any other failure is logged but non-fatal (still want main() to proceed).
    // Use eprintln! rather than tracing::warn! because every binary calls
    // this helper *before* tracing_init runs, and the tracing global
    // subscriber is not yet installed — a tracing::warn! here would be
    // silently dropped (WR-10). stderr is always available.
    eprintln!(
        "fastuse: SetProcessDpiAwarenessContext failed (LastError={}); continuing without PerMonitorV2",
        err.0
    );
    DpiOutcome::Set
}
