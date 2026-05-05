//! `%LOCALAPPDATA%\fastuse\config.toml` loader.
//!
//! Optional file. Sensible defaults if absent. Sections:
//! - [permissions] safe_mode (bool), gated_tools (Vec<String>)
//! - [input] default_humanize (bool), typing_mean_ms (u32), typing_stddev_ms (u32)
//! - [scaling] target_max (u32, default 1024)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration loaded from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Permission and gating settings.
    pub permissions: PermissionsConfig,
    /// Input simulation settings.
    pub input: InputConfig,
    /// Image scaling settings.
    pub scaling: ScalingConfig,
}

/// Permission-related configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionsConfig {
    /// Whether safe mode is enabled (restricts destructive operations).
    pub safe_mode: bool,
    /// Explicitly gated tool names.
    pub gated_tools: Vec<String>,
}

/// Input simulation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    /// Whether to humanize (add jitter to) typing by default.
    pub default_humanize: bool,
    /// Mean typing delay in milliseconds.
    pub typing_mean_ms: u32,
    /// Standard deviation of typing delay in milliseconds.
    pub typing_stddev_ms: u32,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self { default_humanize: true, typing_mean_ms: 80, typing_stddev_ms: 30 }
    }
}

/// Image scaling configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScalingConfig {
    /// Maximum dimension (width or height) for scaled screenshots.
    pub target_max: u32,
}

impl Default for ScalingConfig {
    fn default() -> Self { Self { target_max: 1024 } }
}

/// Default config path: `%LOCALAPPDATA%\fastuse\config.toml`.
pub fn default_path() -> Option<PathBuf> {
    crate::local_app_data().ok().map(|p| p.join("config.toml"))
}

/// Load config from `path`. Missing file = `Config::default()`. Parse errors
/// logged via `tracing` and fall back to defaults.
pub fn load_or_default(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(s) => match toml::from_str(&s) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, path = ?path, "config parse failed; using defaults");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let c = Config::default();
        assert!(!c.permissions.safe_mode);
        assert!(c.permissions.gated_tools.is_empty());
        assert!(c.input.default_humanize);
        assert_eq!(c.input.typing_mean_ms, 80);
        assert_eq!(c.scaling.target_max, 1024);
    }

    #[test]
    fn parses_full_config() {
        let toml_str = r#"
            [permissions]
            safe_mode = true
            gated_tools = ["kill_process", "shell_exec"]

            [input]
            default_humanize = false
            typing_mean_ms = 120
            typing_stddev_ms = 40

            [scaling]
            target_max = 1280
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(c.permissions.safe_mode);
        assert_eq!(c.permissions.gated_tools.len(), 2);
        assert!(!c.input.default_humanize);
        assert_eq!(c.input.typing_mean_ms, 120);
        assert_eq!(c.scaling.target_max, 1280);
    }

    #[test]
    fn parses_partial_config() {
        let toml_str = r#"
            [permissions]
            safe_mode = true
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(c.permissions.safe_mode);
        assert!(c.permissions.gated_tools.is_empty()); // default
        assert!(c.input.default_humanize); // default true
        assert_eq!(c.scaling.target_max, 1024); // default
    }

    #[test]
    fn missing_file_returns_defaults() {
        let nowhere = std::path::Path::new("Z:\\definitely\\not\\here\\config.toml");
        let c = load_or_default(nowhere);
        assert_eq!(c.scaling.target_max, 1024);
    }
}
