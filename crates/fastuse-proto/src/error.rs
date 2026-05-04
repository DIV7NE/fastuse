//! Error code enum and structured Error per D-18.
//!
//! Codes are explicit enum variants (NOT free-form strings) so later phases
//! reuse the same vocabulary. JSON shape on the MCP edge:
//!     {"error":{"code":"DAEMON_DEAD","message":"...","hint":"..."}}

use serde::{Deserialize, Serialize};

/// Stable error vocabulary — Phase 1 set per D-18.
/// Later phases extend with new variants (NEVER renumber/rename existing ones).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    /// UI Privilege Isolation blocked the operation (Phase 2).
    UipiBlocked,
    /// The daemon was contacted but is no longer responding.
    DaemonDead,
    /// Auto-spawn could not bring the daemon up.
    DaemonSpawnFailed,
    /// The named pipe denied the connection (ACL mismatch, foreign user).
    PipeDenied,
    /// A bounded operation exceeded its deadline.
    Timeout,
    /// Wire protocol version mismatch between client and daemon.
    ProtocolVersionMismatch,
    /// Catch-all for unexpected internal errors.
    Internal,
    /// Target HWND is not a live top-level window (Phase 2).
    WindowNotFound,
    /// No monitor contains the requested coordinates / id (Phase 2).
    MonitorNotFound,
    /// SendInput accepted fewer events than requested — likely a low-level
    /// hook (anti-cheat, password manager, screen reader) is dropping input
    /// (Phase 2).
    InputBlockedHang,
    /// Chord string failed to parse (Phase 2).
    InvalidChord,
    /// Confirmed-tier tool called without `--allow=<tool>` (Phase 4).
    PermissionRequired,
    /// Deny-list match or daemon-self-PID protection (Phase 4).
    PermissionBlocked,
    /// `shell_exec` exceeded its timeout (Phase 4).
    ShellTimeout,
    /// `shell_exec` output exceeded the 1 MiB cap (Phase 4).
    ShellOutputTruncated,
    /// `launch_app` could not resolve a query to any binary / AUMID / .lnk (Phase 4).
    AppNotFound,
    /// `kill_process` selector matched no running process (Phase 4).
    ProcessNotFound,
    /// `.lnk` resolution returned an empty target (shell-folder path) (Phase 4).
    LnkResolutionFailed,
    /// UWP AUMID activation returned non-S_OK (Phase 4).
    AumidActivationFailed,
}

impl ErrorCode {
    /// Stable string label used on the JSON wire (D-18).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UipiBlocked => "UIPI_BLOCKED",
            Self::DaemonDead => "DAEMON_DEAD",
            Self::DaemonSpawnFailed => "DAEMON_SPAWN_FAILED",
            Self::PipeDenied => "PIPE_DENIED",
            Self::Timeout => "TIMEOUT",
            Self::ProtocolVersionMismatch => "PROTOCOL_VERSION_MISMATCH",
            Self::Internal => "INTERNAL",
            Self::WindowNotFound => "WINDOW_NOT_FOUND",
            Self::MonitorNotFound => "MONITOR_NOT_FOUND",
            Self::InputBlockedHang => "INPUT_BLOCKED_HANG",
            Self::InvalidChord => "INVALID_CHORD",
            Self::PermissionRequired => "PERMISSION_REQUIRED",
            Self::PermissionBlocked => "PERMISSION_BLOCKED",
            Self::ShellTimeout => "SHELL_TIMEOUT",
            Self::ShellOutputTruncated => "SHELL_OUTPUT_TRUNCATED",
            Self::AppNotFound => "APP_NOT_FOUND",
            Self::ProcessNotFound => "PROCESS_NOT_FOUND",
            Self::LnkResolutionFailed => "LNK_RESOLUTION_FAILED",
            Self::AumidActivationFailed => "AUMID_ACTIVATION_FAILED",
        }
    }
}

/// Structured error carried over the wire (D-18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    /// Stable machine-readable error vocabulary.
    pub code: ErrorCode,
    /// Human-readable explanation of the failure.
    pub message: String,
    /// Optional hint suggesting next steps for the operator.
    pub hint: Option<String>,
}

impl Error {
    /// Construct a new `Error` with no hint.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    /// Attach a hint for operator follow-up.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " (hint: {h})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{decode_frame, encode_frame};

    #[test]
    fn every_error_code_round_trips() {
        for code in [
            ErrorCode::UipiBlocked,
            ErrorCode::DaemonDead,
            ErrorCode::DaemonSpawnFailed,
            ErrorCode::PipeDenied,
            ErrorCode::Timeout,
            ErrorCode::ProtocolVersionMismatch,
            ErrorCode::Internal,
            ErrorCode::WindowNotFound,
            ErrorCode::MonitorNotFound,
            ErrorCode::InputBlockedHang,
            ErrorCode::InvalidChord,
            ErrorCode::PermissionRequired,
            ErrorCode::PermissionBlocked,
            ErrorCode::ShellTimeout,
            ErrorCode::ShellOutputTruncated,
            ErrorCode::AppNotFound,
            ErrorCode::ProcessNotFound,
            ErrorCode::LnkResolutionFailed,
            ErrorCode::AumidActivationFailed,
        ] {
            let err = Error::new(code, "test message");
            let bytes = encode_frame(&err).unwrap();
            let mut cursor = std::io::Cursor::new(bytes);
            let decoded: Error = decode_frame(&mut cursor).unwrap();
            assert_eq!(decoded.code, code);
            assert_eq!(decoded.message, "test message");
        }
    }

    #[test]
    fn error_code_string_labels_stable() {
        // These strings are part of the wire contract; never change them.
        assert_eq!(ErrorCode::UipiBlocked.as_str(), "UIPI_BLOCKED");
        assert_eq!(ErrorCode::DaemonDead.as_str(), "DAEMON_DEAD");
        assert_eq!(ErrorCode::DaemonSpawnFailed.as_str(), "DAEMON_SPAWN_FAILED");
        assert_eq!(ErrorCode::PipeDenied.as_str(), "PIPE_DENIED");
        assert_eq!(ErrorCode::Timeout.as_str(), "TIMEOUT");
        assert_eq!(ErrorCode::ProtocolVersionMismatch.as_str(), "PROTOCOL_VERSION_MISMATCH");
        assert_eq!(ErrorCode::Internal.as_str(), "INTERNAL");
        assert_eq!(ErrorCode::WindowNotFound.as_str(), "WINDOW_NOT_FOUND");
        assert_eq!(ErrorCode::MonitorNotFound.as_str(), "MONITOR_NOT_FOUND");
        assert_eq!(ErrorCode::InputBlockedHang.as_str(), "INPUT_BLOCKED_HANG");
        assert_eq!(ErrorCode::InvalidChord.as_str(), "INVALID_CHORD");
        assert_eq!(ErrorCode::PermissionRequired.as_str(), "PERMISSION_REQUIRED");
        assert_eq!(ErrorCode::PermissionBlocked.as_str(), "PERMISSION_BLOCKED");
        assert_eq!(ErrorCode::ShellTimeout.as_str(), "SHELL_TIMEOUT");
        assert_eq!(ErrorCode::ShellOutputTruncated.as_str(), "SHELL_OUTPUT_TRUNCATED");
        assert_eq!(ErrorCode::AppNotFound.as_str(), "APP_NOT_FOUND");
        assert_eq!(ErrorCode::ProcessNotFound.as_str(), "PROCESS_NOT_FOUND");
        assert_eq!(ErrorCode::LnkResolutionFailed.as_str(), "LNK_RESOLUTION_FAILED");
        assert_eq!(ErrorCode::AumidActivationFailed.as_str(), "AUMID_ACTIVATION_FAILED");
    }
}
