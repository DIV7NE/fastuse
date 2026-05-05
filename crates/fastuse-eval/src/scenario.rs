use serde::{Deserialize, Serialize};

/// Top-level description of a single eval scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub tier: u8, // 1, 2, or 3
    pub launch: LaunchSpec,
    pub task: String,        // natural-language prompt for Claude
    pub success: SuccessCheck,
    pub timeout_seconds: u32,
}

/// How to launch the target application before running the task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub command: String,
    pub args: Vec<String>,
    pub wait_for_window_title_substr: Option<String>,
    pub wait_timeout_ms: u32,
}

/// Deterministic post-run check used to classify pass/fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SuccessCheck {
    /// Read the value of a UIA element and assert it matches.
    UiaValueEquals { selector: String, expected: String },
    /// Run an OCR pass against a region after the run; pass if needle found.
    /// (OCR-for-verification, NOT for targeting — we removed that.)
    ScreenshotContainsText { region_native: Option<[i32; 4]>, needle: String },
    /// A custom shell predicate that exits 0 on success.
    Shell { command: String },
}
