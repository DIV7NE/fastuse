//! Aimbot smoke fixtures. Run with:
//!   ./target/release/fastuse-cli.exe bench aimbot --scenario calculator
//!   ./target/release/fastuse-cli.exe bench aimbot --scenario discord
//!   ./target/release/fastuse-cli.exe bench aimbot --scenario lconnect3
//!
//! Reports per-scenario: strategy used, verified bool, evidence, waited_ms,
//! and overall pass/fail.
//!
//! Implementation strategy: shell out to the current `fastuse-cli` binary
//! (via `std::env::current_exe`) for each subcommand and parse the JSON
//! response into a `StepReport`. Each step has its own hard timeout so a
//! stuck call cannot hang the whole scenario.

use serde::Serialize;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioReport {
    pub scenario: String,
    pub steps: Vec<StepReport>,
    pub pass: bool,
    pub total_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub name: String,
    pub verified: bool,
    pub evidence: String,
    pub strategy_used: String,
    pub waited_ms: Option<u32>,
    pub elapsed_ms: u32,
}

const STEP_TIMEOUT: Duration = Duration::from_secs(15);

fn cli_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fastuse-cli.exe"))
}

/// Run one CLI subcommand. `args` is what comes after the binary path.
/// Returns (parsed_json, raw_stdout, raw_stderr, elapsed_ms, timed_out).
async fn run_cli<I, S>(
    args: I,
    extra_env: &[(&str, &str)],
) -> (Option<Value>, String, String, u32, bool)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let started = Instant::now();
    let mut cmd = Command::new(cli_exe());
    cmd.args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);
    let fut = cmd.output();
    let res = tokio::time::timeout(STEP_TIMEOUT, fut).await;
    let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    match res {
        Err(_) => (
            None,
            String::new(),
            "step timed out".into(),
            elapsed_ms,
            true,
        ),
        Ok(Err(e)) => (
            None,
            String::new(),
            format!("spawn error: {e}"),
            elapsed_ms,
            false,
        ),
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            // Both success and error JSON go to stdout (success) or stderr (error
            // wrapper from main.rs). Try stdout first, then stderr.
            let parsed = serde_json::from_str::<Value>(&stdout)
                .ok()
                .or_else(|| serde_json::from_str::<Value>(&stderr).ok());
            (parsed, stdout, stderr, elapsed_ms, false)
        }
    }
}

/// Build a `StepReport` from a parsed CLI JSON response. The wire-level
/// `ActionResult` includes `verified` / `evidence` / `strategy_used` /
/// `waited_ms` fields when the response is an action result; otherwise we
/// extract what we can.
fn step_from_value(name: &str, v: Option<&Value>, elapsed_ms: u32, fallback: &str) -> StepReport {
    let v = match v {
        Some(v) => v,
        None => {
            return StepReport {
                name: name.into(),
                verified: false,
                evidence: fallback.into(),
                strategy_used: "n/a".into(),
                waited_ms: None,
                elapsed_ms,
            };
        }
    };

    // The phase3 click-element / type-into-element commands print the inner
    // ActionResult shape. main.rs's error wrapper prints `{ok:false,error:...}`.
    let verified = v
        .get("verified")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| v.get("ok").and_then(Value::as_bool).unwrap_or(false));
    let strategy_used = v
        .get("strategy_used")
        .and_then(Value::as_str)
        .unwrap_or("n/a")
        .to_string();
    let evidence = v
        .get("evidence")
        .map(|e| e.to_string())
        .or_else(|| v.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| v.to_string());
    let waited_ms = v
        .get("waited_ms")
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32);
    StepReport {
        name: name.into(),
        verified,
        evidence,
        strategy_used,
        waited_ms,
        elapsed_ms,
    }
}

/// Calculator: launch, type 9+10=, verify result reads "19".
pub async fn run_calculator() -> ScenarioReport {
    let start = Instant::now();
    let mut steps: Vec<StepReport> = Vec::new();

    // Step 1 — launch calc.exe (gated).
    let (v, _stdout, stderr, elapsed, timed_out) = run_cli(
        ["launch-app", "calc.exe"],
        &[("FASTUSE_ALLOW", "launch_app")],
    )
    .await;
    let mut step = step_from_value(
        "launch_app calc.exe",
        v.as_ref(),
        elapsed,
        if timed_out { "timed out" } else { &stderr },
    );
    if !timed_out && step.evidence == "null" {
        step.verified = true;
    }
    steps.push(step);

    // Step 2 — wait for the Calculator results display to appear.
    let (v, _stdout, stderr, elapsed, _) = run_cli(
        [
            "wait-for-element",
            r#"{"ByAutomationId":"CalculatorResults"}"#,
            "--timeout-ms",
            "8000",
        ],
        &[],
    )
    .await;
    steps.push(step_from_value(
        "wait CalculatorResults",
        v.as_ref(),
        elapsed,
        &stderr,
    ));

    // Step 3 — type the expression. `type` accepts literal Unicode and
    // routes through SendInput so "+" / "=" are sent verbatim.
    let (v, _stdout, stderr, elapsed, _) = run_cli(["type", "9+10="], &[]).await;
    steps.push(step_from_value("type 9+10=", v.as_ref(), elapsed, &stderr));

    // Step 4 — query the result and assert it contains "19".
    let (v, stdout, stderr, elapsed, _) = run_cli(
        ["uia-query", r#"{"ByAutomationId":"CalculatorResults"}"#],
        &[],
    )
    .await;
    let raw = if stdout.is_empty() { stderr.clone() } else { stdout.clone() };
    let contains_19 = raw.contains("19");
    let mut step = step_from_value("read CalculatorResults", v.as_ref(), elapsed, &raw);
    step.verified = contains_19;
    if contains_19 {
        step.evidence = format!("result contains 19 (raw={})", raw.chars().take(160).collect::<String>());
        step.strategy_used = "uia_query".into();
    }
    steps.push(step);

    let pass = steps.iter().all(|s| s.verified);
    ScenarioReport {
        scenario: "calculator".into(),
        steps,
        pass,
        total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}

/// Discord: focus existing Discord window, send "hello" to "Quara" DM via
/// Ctrl+K quick-switcher path. UIA tree is degraded (Electron); should hit
/// the OCR fallback.
pub async fn run_discord() -> ScenarioReport {
    let start = Instant::now();
    let mut steps: Vec<StepReport> = Vec::new();

    // Step 1 — find Discord HWND.
    let (v, stdout, stderr, elapsed, _) =
        run_cli(["list-windows", "--process", "Discord"], &[]).await;
    let raw = if stdout.is_empty() { stderr.clone() } else { stdout.clone() };
    let hwnd = v
        .as_ref()
        .and_then(|j| j.get("windows").or_else(|| j.get("ok")).cloned())
        .and_then(|w| w.as_array().and_then(|a| a.first().cloned()))
        .and_then(|w| w.get("hwnd").and_then(Value::as_u64));
    let mut step = step_from_value("list-windows Discord", v.as_ref(), elapsed, &raw);
    if let Some(h) = hwnd {
        step.verified = true;
        step.evidence = format!("hwnd={h}");
        step.strategy_used = "list_windows".into();
    } else {
        step.verified = false;
        step.evidence = format!("Discord not running ({})", raw.chars().take(160).collect::<String>());
        steps.push(step);
        return ScenarioReport {
            scenario: "discord".into(),
            steps,
            pass: false,
            total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
        };
    }
    let hwnd = hwnd.unwrap();
    steps.push(step);

    // Step 2 — focus the window.
    let hwnd_str = hwnd.to_string();
    let (v, _stdout, stderr, elapsed, _) =
        run_cli(["focus-window", &hwnd_str], &[]).await;
    steps.push(step_from_value("focus-window", v.as_ref(), elapsed, &stderr));

    // Step 3 — Ctrl+K quick switcher.
    let (v, _stdout, stderr, elapsed, _) = run_cli(["key", "ctrl+k"], &[]).await;
    steps.push(step_from_value("key ctrl+k", v.as_ref(), elapsed, &stderr));

    // brief settle
    let (_v, _, _, _, _) = run_cli(["wait", "--ms", "300"], &[]).await;

    // Step 4 — type "Quara".
    let (v, _stdout, stderr, elapsed, _) = run_cli(["type", "Quara"], &[]).await;
    steps.push(step_from_value("type Quara", v.as_ref(), elapsed, &stderr));

    let (_v, _, _, _, _) = run_cli(["wait", "--ms", "500"], &[]).await;

    // Step 5 — enter to open DM.
    let (v, _stdout, stderr, elapsed, _) = run_cli(["key", "enter"], &[]).await;
    steps.push(step_from_value("key enter (open DM)", v.as_ref(), elapsed, &stderr));

    let (_v, _, _, _, _) = run_cli(["wait", "--ms", "500"], &[]).await;

    // Step 6 — type message.
    let (v, _stdout, stderr, elapsed, _) = run_cli(["type", "hello"], &[]).await;
    steps.push(step_from_value("type hello", v.as_ref(), elapsed, &stderr));

    // Step 7 — enter to send.
    let (v, _stdout, stderr, elapsed, _) = run_cli(["key", "enter"], &[]).await;
    steps.push(step_from_value("key enter (send)", v.as_ref(), elapsed, &stderr));

    let pass = steps.iter().all(|s| s.verified);
    ScenarioReport {
        scenario: "discord".into(),
        steps,
        pass,
        total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}

/// L-Connect3: focus running L-Connect3 window, click Settings tab. UIA tree
/// is degraded (Electron); should hit the OCR fallback first try.
pub async fn run_lconnect3() -> ScenarioReport {
    let start = Instant::now();
    let mut steps: Vec<StepReport> = Vec::new();

    // Step 1 — find L-Connect 3 HWND.
    let (v, stdout, stderr, elapsed, _) =
        run_cli(["list-windows", "--title", "L-Connect 3"], &[]).await;
    let raw = if stdout.is_empty() { stderr.clone() } else { stdout.clone() };
    let hwnd = v
        .as_ref()
        .and_then(|j| j.get("windows").or_else(|| j.get("ok")).cloned())
        .and_then(|w| w.as_array().and_then(|a| a.first().cloned()))
        .and_then(|w| w.get("hwnd").and_then(Value::as_u64));
    let mut step = step_from_value("list-windows L-Connect 3", v.as_ref(), elapsed, &raw);
    if let Some(h) = hwnd {
        step.verified = true;
        step.evidence = format!("hwnd={h}");
        step.strategy_used = "list_windows".into();
    } else {
        step.verified = false;
        step.evidence = "L-Connect 3 not running".into();
        steps.push(step);
        return ScenarioReport {
            scenario: "lconnect3".into(),
            steps,
            pass: false,
            total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
        };
    }
    let hwnd = hwnd.unwrap();
    steps.push(step);

    // Step 2 — focus.
    let hwnd_str = hwnd.to_string();
    let (v, _stdout, stderr, elapsed, _) =
        run_cli(["focus-window", &hwnd_str], &[]).await;
    steps.push(step_from_value("focus-window", v.as_ref(), elapsed, &stderr));

    // Step 3 — strict click on Settings. Strict disables escalation, forcing
    // the strategy picker to commit on first try; with the v1.0 design the
    // Electron-degraded path resolves Settings via OCR fallback.
    let (v, _stdout, stderr, elapsed, _) = run_cli(
        [
            "click-element",
            r#"{"ByName":"Settings"}"#,
            "--strict",
        ],
        &[],
    )
    .await;
    let mut step = step_from_value("click-element Settings (strict)", v.as_ref(), elapsed, &stderr);
    // L-Connect3 PASS criterion (per task brief): strategy_used==bounds_click_ocr AND verified.
    step.verified = step.verified && step.strategy_used == "bounds_click_ocr";
    steps.push(step);

    let pass = steps.iter().all(|s| s.verified);
    ScenarioReport {
        scenario: "lconnect3".into(),
        steps,
        pass,
        total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}
