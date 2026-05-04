//! Process-wide MTA bump (D-25).
//!
//! `CoIncrementMTAUsage` keeps the MTA alive for the lifetime of the process
//! so that worker threads can call `CoInitializeEx(COINIT_MULTITHREADED)` and
//! pay zero per-thread setup beyond their own apartment ref-count.

use std::sync::OnceLock;

use windows::Win32::System::Com::{CoIncrementMTAUsage, CO_MTA_USAGE_COOKIE};

/// Opaque handle that, when held, keeps the MTA alive process-wide.
///
/// Stored in a `OnceLock` and never released. Idempotent: subsequent calls are
/// no-ops returning a reference to the original token.
#[derive(Debug)]
pub struct MtaToken {
    _cookie: CO_MTA_USAGE_COOKIE,
}

// SAFETY: CO_MTA_USAGE_COOKIE is a process-global handle managed by the OS;
// dropping references between threads is safe.
unsafe impl Send for MtaToken {}
unsafe impl Sync for MtaToken {}

static MTA_TOKEN: OnceLock<MtaToken> = OnceLock::new();

/// Idempotently bump the process-wide MTA usage count.
///
/// Call exactly once at daemon startup, after `set_per_monitor_v2_first_call`
/// and before spawning any COM-using thread.
pub fn increment_mta_once() -> &'static MtaToken {
    MTA_TOKEN.get_or_init(|| {
        // SAFETY: CoIncrementMTAUsage is callable from any thread at any time.
        let cookie = unsafe { CoIncrementMTAUsage() }
            .expect("CoIncrementMTAUsage failed at daemon startup");
        MtaToken { _cookie: cookie }
    })
}
