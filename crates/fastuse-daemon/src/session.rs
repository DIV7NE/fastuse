//! Per-connection session state.
//!
//! `Session.allow` is set ONCE at handshake from the daemon's startup
//! `--allow` flag (or `FASTUSE_ALLOW` env). Per the Phase 4 Codex review,
//! it is **immutable for the lifetime of the session** — there is no
//! API to mutate it. Per-call `allow:true` in tool args is forbidden;
//! the dispatcher MUST NOT read any per-call allow field.

use fastuse_core::perm::SessionAllow;

/// Immutable per-session permission grant set.
#[derive(Debug, Clone)]
pub struct Session {
    allow: SessionAllow,
}

impl Session {
    /// Construct a session with the given allow-list. Empty by default.
    pub fn new(allow: SessionAllow) -> Self {
        Self { allow }
    }

    /// Read-only view onto the allow-list.
    pub fn allow(&self) -> &SessionAllow {
        &self.allow
    }
}
