//! Usage: cargo run --release -p fastuse-eval -- run scenarios/calculator.toml
//! Usage: cargo run --release -p fastuse-eval -- run-tier 1

use anyhow::{Context, Result};
use fastuse_eval::scenario::{Scenario, SuccessCheck};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 { eprintln!("usage: eval run <scenario.toml> | run-tier <N>"); std::process::exit(2); }
    match args[1].as_str() {
        "run" => run_one(PathBuf::from(&args[2])).await,
        "run-tier" => {
            let tier: u8 = args[2].parse()?;
            let dir = PathBuf::from("crates/fastuse-eval/scenarios");
            let mut failures = 0;
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("toml") { continue; }
                let raw = std::fs::read_to_string(&path)?;
                let s: Scenario = toml::from_str(&raw)?;
                if s.tier != tier { continue; }
                println!("=> {} (tier {})", s.name, s.tier);
                if let Err(e) = run_one(path).await {
                    eprintln!("FAIL: {e:#}");
                    failures += 1;
                }
            }
            if failures > 0 { std::process::exit(1); }
            Ok(())
        }
        other => { eprintln!("unknown subcommand: {other}"); std::process::exit(2); }
    }
}

/// Expand `${VAR}` placeholders using environment variables.
/// Only replaces `${...}` syntax; bare `$VAR` is left untouched (used by shells).
/// Returns `Err` with the name of the first missing variable.
fn expand_env(s: &str) -> std::result::Result<String, String> {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let before = &rest[..start];
        result.push_str(before);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let var_name = &after[..end];
            match std::env::var(var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => return Err(var_name.to_string()),
            }
            rest = &after[end + 1..];
        } else {
            // No closing brace — treat as literal
            result.push_str("${");
            rest = after;
        }
    }
    result.push_str(rest);
    Ok(result)
}

/// Expand env vars in the scenario's launch and success fields.
/// Returns `None` if a required env var is unset (caller should skip the scenario).
fn expand_scenario(scenario: &mut Scenario) -> Option<()> {
    // Expand launch fields
    scenario.launch.command = expand_env(&scenario.launch.command)
        .map_err(|var| eprintln!("  SKIP (env var not set: {var})"))
        .ok()?;
    if let Some(ref title) = scenario.launch.wait_for_window_title_substr.clone() {
        scenario.launch.wait_for_window_title_substr = Some(
            expand_env(title)
                .map_err(|var| eprintln!("  SKIP (env var not set: {var})"))
                .ok()?
        );
    }
    // Expand success fields
    match scenario.success.clone() {
        SuccessCheck::ScreenshotContainsText { needle, region_native } => {
            let expanded = expand_env(&needle)
                .map_err(|var| eprintln!("  SKIP (env var not set: {var})"))
                .ok()?;
            scenario.success = SuccessCheck::ScreenshotContainsText { needle: expanded, region_native };
        }
        SuccessCheck::Shell { command } => {
            let expanded = expand_env(&command)
                .map_err(|var| eprintln!("  SKIP (env var not set: {var})"))
                .ok()?;
            scenario.success = SuccessCheck::Shell { command: expanded };
        }
        SuccessCheck::UiaValueEquals { selector, expected } => {
            let expanded_sel = expand_env(&selector)
                .map_err(|var| eprintln!("  SKIP (env var not set: {var})"))
                .ok()?;
            let expanded_exp = expand_env(&expected)
                .map_err(|var| eprintln!("  SKIP (env var not set: {var})"))
                .ok()?;
            scenario.success = SuccessCheck::UiaValueEquals { selector: expanded_sel, expected: expanded_exp };
        }
    }
    Some(())
}

async fn run_one(path: PathBuf) -> Result<()> {
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {path:?}"))?;
    let mut scenario: Scenario = toml::from_str(&raw).with_context(|| format!("parse {path:?}"))?;
    // Expand env vars in launch + success fields; skip gracefully if vars are unset.
    if expand_scenario(&mut scenario).is_none() {
        eprintln!("  (scenario {} skipped — required env var(s) not set)", scenario.name);
        return Ok(());
    }
    // 1. Launch the target app via fastuse-cli (uses fastuse-mcp under the hood).
    // 2. Spawn `claude` (Claude Code headless) with the task as prompt.
    //    For v1 of the eval harness, document this as TODO at the runtime
    //    level — manual "you run Claude Code, paste this prompt, press
    //    enter, watch what happens" is acceptable for the first rev.
    //    Automating the headless invocation depends on Claude Code's
    //    --print/--prompt mode; revisit when we have time.
    eprintln!("TODO: drive Claude Code with prompt:\n{}", scenario.task);
    eprintln!("After Claude finishes, run success check manually:");
    match &scenario.success {
        SuccessCheck::UiaValueEquals { selector, expected } => {
            eprintln!("  fastuse-cli uia-query '{selector}' (expect value {expected:?})");
        }
        SuccessCheck::ScreenshotContainsText { region_native, needle } => {
            eprintln!("  Screenshot region {region_native:?}, find {needle:?} in OCR output");
        }
        SuccessCheck::Shell { command } => {
            eprintln!("  Run: {command}");
        }
    }
    Ok(())
}
