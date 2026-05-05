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

async fn run_one(path: PathBuf) -> Result<()> {
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {path:?}"))?;
    let scenario: Scenario = toml::from_str(&raw).with_context(|| format!("parse {path:?}"))?;
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
