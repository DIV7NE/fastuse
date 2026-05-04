//! Phase 4 error variants — extends the wire `ErrorCode` set with the
//! permission / shell / launch / process error categories.
//!
//! `fastuse-proto::ErrorCode` is the wire-stable enum. This local
//! `FastuseError` is the rich, structured form used inside the daemon and
//! converted to the wire shape at dispatch time.

use fastuse_proto::ErrorCode;

/// Daemon-side rich error type. Conversion to [`ErrorCode`] + message + hint
/// happens at the dispatch boundary.
#[derive(Debug, Clone, thiserror::Error)]
pub enum FastuseError {
    /// Confirmed-tier tool called without `--allow=<tool>`.
    #[error("permission required: {tool}")]
    PermissionRequired {
        /// Tool name.
        tool: &'static str,
        /// Operator-facing hint with the exact `--allow=<tool>` arg.
        hint: String,
    },
    /// Deny-list match or daemon-self protection.
    #[error("permission blocked: {tool} ({reason})")]
    PermissionBlocked {
        /// Tool name.
        tool: &'static str,
        /// Static reason (deny-list pattern, daemon-self, etc.).
        reason: &'static str,
    },
    /// Shell command exceeded its timeout.
    #[error("shell timeout after {ms}ms")]
    ShellTimeout {
        /// Timeout that fired in milliseconds.
        ms: u64,
    },
    /// Shell command produced more output than the cap allows.
    #[error("shell output truncated at {cap_bytes} bytes")]
    ShellOutputTruncated {
        /// Configured cap in bytes.
        cap_bytes: usize,
    },
    /// `launch_app` could not resolve `query` to any executable / AUMID / .lnk.
    #[error("app not found: {query}")]
    AppNotFound {
        /// User-supplied query.
        query: String,
    },
    /// `kill_process` could not match `selector` to any running process.
    #[error("process not found: {selector}")]
    ProcessNotFound {
        /// PID-or-name selector that didn't match.
        selector: String,
    },
    /// `.lnk` resolution returned an empty target (shell folder etc.).
    #[error("lnk resolution failed: {path}")]
    LnkResolutionFailed {
        /// Path of the .lnk file that failed to resolve.
        path: String,
    },
    /// UWP `IApplicationActivationManager::ActivateApplication` returned non-S_OK.
    #[error("aumid activation failed: {aumid} (hr=0x{hr:08x})")]
    AumidActivationFailed {
        /// AUMID we tried to activate.
        aumid: String,
        /// HRESULT from ActivateApplication.
        hr: i32,
    },
    /// Generic underlying I/O.
    #[error("io: {0}")]
    Io(String),
    /// Internal invariant violation.
    #[error("internal: {0}")]
    Internal(String),
}

impl FastuseError {
    /// Map to the wire `ErrorCode` vocabulary.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::PermissionRequired { .. } => ErrorCode::PermissionRequired,
            Self::PermissionBlocked { .. } => ErrorCode::PermissionBlocked,
            Self::ShellTimeout { .. } => ErrorCode::ShellTimeout,
            Self::ShellOutputTruncated { .. } => ErrorCode::ShellOutputTruncated,
            Self::AppNotFound { .. } => ErrorCode::AppNotFound,
            Self::ProcessNotFound { .. } => ErrorCode::ProcessNotFound,
            Self::LnkResolutionFailed { .. } => ErrorCode::LnkResolutionFailed,
            Self::AumidActivationFailed { .. } => ErrorCode::AumidActivationFailed,
            Self::Io(_) | Self::Internal(_) => ErrorCode::Internal,
        }
    }

    /// Operator-facing hint, if any.
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::PermissionRequired { hint, .. } => Some(hint.clone()),
            Self::PermissionBlocked { reason, .. } => Some((*reason).to_string()),
            Self::AppNotFound { .. } => Some(
                "verify the binary name or .lnk exists; for UAC-elevated apps run daemon as administrator"
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// Convert into a wire-level `fastuse_proto::Error`.
    pub fn to_wire(&self) -> fastuse_proto::Error {
        let mut e = fastuse_proto::Error::new(self.code(), self.to_string());
        if let Some(h) = self.hint() {
            e = e.with_hint(h);
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_required_maps_to_wire_code() {
        let e = FastuseError::PermissionRequired {
            tool: "shell_exec",
            hint: "use --allow=shell_exec".into(),
        };
        assert_eq!(e.code(), ErrorCode::PermissionRequired);
        let w = e.to_wire();
        assert_eq!(w.hint.as_deref(), Some("use --allow=shell_exec"));
    }

    #[test]
    fn shell_timeout_serializes() {
        let e = FastuseError::ShellTimeout { ms: 30_000 };
        let w = e.to_wire();
        assert_eq!(w.code, ErrorCode::ShellTimeout);
        assert!(w.message.contains("30000"));
    }
}
