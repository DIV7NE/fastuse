//! Permission tier model: Free / Confirmed / Blocked.
//!
//! Per Phase 4 D-Phase4-CONTEXT decisions:
//!
//! - Tiers resolve at dispatch time, NOT parse time. The session's allow-list
//!   is fixed at handshake and immutable for the session lifetime.
//! - Per-call `allow:true` in tool args is FORBIDDEN (Codex review fix).
//!   Closes the self-grant hole that would otherwise let any tool call grant
//!   itself a Confirmed permission.
//! - Deny-list is a hardcoded list of substrings (case-insensitive) covering
//!   2FA / banking / crypto-wallet apps. To extend: add to [`DENY_LIST`].
//!
//! This module is platform-neutral (no win32, no tokio): `fastuse-core` stays
//! the platform-agnostic layer.

use crate::error::FastuseError;

/// Permission tier resolution outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Free: no consent needed, dispatch invokes the handler.
    Free,
    /// Confirmed: requires session-level grant via `--allow=<tool>` (CLI) or
    /// `FASTUSE_ALLOW=<tool>` (MCP env). Dispatch returns `PermissionRequired`
    /// with a hint pointing the agent at the exact arg.
    Confirmed,
    /// Blocked: deny-list match or daemon-self protection. Cannot be granted.
    Blocked,
}

/// One declared tool with its default tier.
#[derive(Debug, Clone, Copy)]
pub struct ToolPerm {
    /// Stable tool identifier used by the dispatcher and the `--allow` parser.
    pub name: &'static str,
    /// Default tier when no `--allow=<tool>` (or `*`) grant matches.
    pub default_tier: Tier,
}

/// Static table of every tool gated by the permission model.
///
/// Phase 4 tools listed here. Phase 2/3 tools that are inherently Free still
/// don't need an entry — `resolve` defaults unknown tools to `Free` (most
/// read-only primitives). Add a row when introducing a Confirmed/Blocked tool.
pub static TOOLS: &[ToolPerm] = &[
    // Clipboard. Per Codex review: only text-get is Free. Image-get is
    // Confirmed because it can leak 2FA codes / screenshots from other apps.
    ToolPerm { name: "clipboard_get_text", default_tier: Tier::Free },
    ToolPerm { name: "clipboard_get_image", default_tier: Tier::Confirmed },
    ToolPerm { name: "clipboard_set_text", default_tier: Tier::Confirmed },
    ToolPerm { name: "clipboard_set_image", default_tier: Tier::Confirmed },
    // Same resource clipboard_set_text/_image clobber, so the same tier.
    ToolPerm { name: "clipboard_set_files", default_tier: Tier::Confirmed },
    // Shell exec.
    ToolPerm { name: "shell_exec", default_tier: Tier::Confirmed },
    // App launch — uniformly Confirmed in v1 (no Start-Menu / path downgrade).
    ToolPerm { name: "launch_app", default_tier: Tier::Confirmed },
    // Process management.
    ToolPerm { name: "list_processes", default_tier: Tier::Free },
    ToolPerm { name: "kill_process", default_tier: Tier::Confirmed },
    // Presses the real mouse button and walks the real cursor across the
    // desktop; same tier as the other tools that act on the user's session.
    ToolPerm { name: "drag_files", default_tier: Tier::Confirmed },
];

/// Hardcoded deny-list (case-insensitive substring match).
///
/// Targets that match any pattern here resolve to [`Tier::Blocked`] regardless
/// of the session allow-list. Substring `*foo*` form is informational — we
/// always match as a substring; the asterisks are documentation noise.
///
/// **To extend**: add a new lowercase substring entry. Conservative bias:
/// false-positives only block legitimate apps with overlapping names; the
/// matched pattern is included in the error message so the user can
/// recognize and override locally.
pub static DENY_LIST: &[&str] = &[
    "authy",
    "google authenticator",
    "microsoft authenticator",
    "duo mobile",
    "banking",
    "coinbase",
    "metamask",
    "ledger live",
    "trezor",
];

/// Session-scoped allow-list. Set at handshake, IMMUTABLE thereafter.
///
/// Contains tool names (e.g. `"shell_exec"`) or the wildcard `"*"` granting
/// every Confirmed tool. Empty by default — every Confirmed call returns
/// `PermissionRequired` until the operator restarts with `--allow=...`.
pub type SessionAllow = Vec<String>;

/// Resolve the effective tier for a single tool dispatch.
///
/// Resolution order:
///   1. Deny-list match on `target` → [`Tier::Blocked`].
///   2. Tool's declared default tier.
///   3. If the default is [`Tier::Confirmed`] and `session_allow` contains the
///      tool name (or `"*"`), downgrade to [`Tier::Free`].
///
/// Unknown tool names default to [`Tier::Free`] (read-only primitives that
/// don't gate). Add a [`TOOLS`] entry to gate a new tool.
pub fn resolve(tool: &str, target: Option<&str>, session_allow: &SessionAllow) -> Tier {
    // 1. Deny-list — overrides everything.
    if let Some(t) = target {
        let lc = t.to_ascii_lowercase();
        for pat in DENY_LIST {
            if lc.contains(pat) {
                return Tier::Blocked;
            }
        }
    }
    // 2. Default tier from table.
    let default = TOOLS
        .iter()
        .find(|p| p.name == tool)
        .map(|p| p.default_tier)
        .unwrap_or(Tier::Free);

    // 3. Allow-list downgrade for Confirmed.
    if matches!(default, Tier::Confirmed) {
        for entry in session_allow {
            if entry == "*" || entry == tool {
                return Tier::Free;
            }
        }
    }
    default
}

/// Generate the operator-facing hint for a `PermissionRequired` error.
///
/// **Never** suggests per-call `allow:true` (Codex review fix; closes the
/// self-grant hole).
pub fn hint(tool: &str) -> String {
    format!(
        "restart the session with --allow={tool} (CLI) or set FASTUSE_ALLOW={tool} in the MCP server env"
    )
}

/// Look up a deny-list pattern that matches `target`, if any.
pub fn deny_list_match(target: &str) -> Option<&'static str> {
    let lc = target.to_ascii_lowercase();
    for pat in DENY_LIST {
        if lc.contains(pat) {
            return Some(pat);
        }
    }
    None
}

/// Refuse to operate on the daemon's own PID. Returns `Err` with
/// `PermissionBlocked` whenever `target_pid == daemon_pid`.
pub fn daemon_self_pid_check(target_pid: u32, daemon_pid: u32) -> Result<(), FastuseError> {
    if target_pid == daemon_pid {
        return Err(FastuseError::PermissionBlocked {
            tool: "kill_process",
            reason: "refusing to terminate the fastuse daemon itself",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_tools_resolve_free_with_no_allow() {
        let allow: SessionAllow = vec![];
        assert_eq!(resolve("clipboard_get_text", None, &allow), Tier::Free);
        assert_eq!(resolve("list_processes", None, &allow), Tier::Free);
    }

    #[test]
    fn confirmed_without_allow_stays_confirmed() {
        let allow: SessionAllow = vec![];
        assert_eq!(resolve("shell_exec", None, &allow), Tier::Confirmed);
        assert_eq!(resolve("clipboard_set_text", None, &allow), Tier::Confirmed);
        assert_eq!(resolve("clipboard_get_image", None, &allow), Tier::Confirmed);
        assert_eq!(resolve("launch_app", None, &allow), Tier::Confirmed);
        assert_eq!(resolve("kill_process", None, &allow), Tier::Confirmed);
    }

    #[test]
    fn explicit_allow_downgrades_to_free() {
        let allow: SessionAllow = vec!["shell_exec".into()];
        assert_eq!(resolve("shell_exec", None, &allow), Tier::Free);
        // Other Confirmed tools NOT in the list stay Confirmed.
        assert_eq!(resolve("kill_process", None, &allow), Tier::Confirmed);
    }

    #[test]
    fn wildcard_allow_downgrades_all_confirmed() {
        let allow: SessionAllow = vec!["*".into()];
        assert_eq!(resolve("shell_exec", None, &allow), Tier::Free);
        assert_eq!(resolve("kill_process", None, &allow), Tier::Free);
        assert_eq!(resolve("launch_app", None, &allow), Tier::Free);
    }

    #[test]
    fn deny_list_blocks_authy() {
        let allow: SessionAllow = vec!["*".into()];
        assert_eq!(resolve("kill_process", Some("Authy"), &allow), Tier::Blocked);
        assert_eq!(resolve("kill_process", Some("authy.exe"), &allow), Tier::Blocked);
    }

    #[test]
    fn deny_list_blocks_coinbase_substring() {
        let allow: SessionAllow = vec![];
        assert_eq!(
            resolve("kill_process", Some("MyCoinbaseHelper"), &allow),
            Tier::Blocked
        );
    }

    #[test]
    fn deny_list_blocks_launch_app_authenticator() {
        let allow: SessionAllow = vec!["*".into()];
        assert_eq!(
            resolve("launch_app", Some("Google Authenticator"), &allow),
            Tier::Blocked
        );
    }

    #[test]
    fn hint_phrasing_never_suggests_per_call_allow() {
        let h = hint("shell_exec");
        assert!(h.contains("--allow=shell_exec"));
        assert!(h.contains("FASTUSE_ALLOW=shell_exec"));
        // Critical: never suggests per-call allow:true (Codex review fix).
        assert!(!h.to_ascii_lowercase().contains("allow:true"));
        assert!(!h.to_ascii_lowercase().contains("allow: true"));
        assert!(!h.to_ascii_lowercase().contains("per-call"));
    }

    #[test]
    fn daemon_self_pid_refused() {
        assert!(daemon_self_pid_check(1234, 1234).is_err());
        assert!(daemon_self_pid_check(1234, 5678).is_ok());
    }

    #[test]
    fn deny_list_match_returns_pattern() {
        assert_eq!(deny_list_match("Authy.exe"), Some("authy"));
        assert!(deny_list_match("notepad.exe").is_none());
    }
}
