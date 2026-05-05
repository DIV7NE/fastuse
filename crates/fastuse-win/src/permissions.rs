//! Tool permission gating. Default = open. Safe-mode (env `FASTUSE_SAFE_MODE=1`
//! or config.toml `[permissions] safe_mode = true`) gates the destructive set.

use std::collections::HashSet;

/// Default tools gated when safe_mode is active.
pub const DEFAULT_GATED: &[&str] = &[
    "launch_app",
    "kill_process",
    "shell_exec",
    "clipboard_set_text",
    "clipboard_set_image",
];

/// Holds the active permission policy for this daemon instance.
#[derive(Debug, Clone)]
pub struct Permissions {
    /// When `true`, only tools not in `gated` may be invoked.
    pub safe_mode: bool,
    /// Set of tool names that are blocked when `safe_mode` is active.
    pub gated: HashSet<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        // Default = wide open. safe_mode off, no tools gated.
        Self { safe_mode: false, gated: HashSet::new() }
    }
}

impl Permissions {
    /// Build a `Permissions` from environment + (optional) config.
    /// Precedence: per-call env (caller resolves) > daemon-launched env > config.
    pub fn from_env_and_config(safe_mode_cfg: bool, gated_cfg: Vec<String>) -> Self {
        let env_safe = std::env::var("FASTUSE_SAFE_MODE")
            .ok()
            .and_then(|v| match v.as_str() {
                "1" | "true" | "TRUE" => Some(true),
                "0" | "false" | "FALSE" => Some(false),
                _ => None,
            });
        let safe_mode = env_safe.unwrap_or(safe_mode_cfg);
        let gated = if safe_mode {
            if gated_cfg.is_empty() {
                DEFAULT_GATED.iter().map(|s| s.to_string()).collect()
            } else {
                gated_cfg.into_iter().collect()
            }
        } else {
            HashSet::new()
        };
        Self { safe_mode, gated }
    }

    /// Returns `true` if `tool` may be invoked under the current policy.
    pub fn is_allowed(&self, tool: &str) -> bool {
        if !self.safe_mode { return true; }
        !self.gated.contains(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_permissions_allow_everything() {
        let p = Permissions::default();
        assert!(p.is_allowed("kill_process"));
        assert!(p.is_allowed("launch_app"));
        assert!(p.is_allowed("computer"));
    }

    #[test]
    fn safe_mode_with_default_gates_destructive_tools() {
        // Note: from_env_and_config reads FASTUSE_SAFE_MODE — for this test
        // we construct directly to avoid env races.
        let p = Permissions {
            safe_mode: true,
            gated: DEFAULT_GATED.iter().map(|s| s.to_string()).collect(),
        };
        assert!(!p.is_allowed("kill_process"));
        assert!(!p.is_allowed("launch_app"));
        assert!(!p.is_allowed("shell_exec"));
        assert!(p.is_allowed("computer"));
        assert!(p.is_allowed("list_windows"));
    }

    #[test]
    fn custom_gated_list_overrides_default() {
        let p = Permissions {
            safe_mode: true,
            gated: ["focus_window".to_string()].into_iter().collect(),
        };
        assert!(!p.is_allowed("focus_window"));
        assert!(p.is_allowed("kill_process")); // not in custom list
    }
}
