//! Phase 1 headline integration test: 100 pings via fastuse-cli, p50 < 5000us.
//!
//! This test runs `fastuse-cli ping --bench 100` against the auto-spawned
//! daemon and parses the JSON. It's the canonical proof of FND-10/FND-12.

use std::process::Command;
use std::time::Duration;

fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fastuse-cli")
}

#[test]
fn ping_rtt_under_5ms_p50() {
    // Best-effort: stop any running daemon first so the test starts cold.
    let _ = Command::new(cli_bin()).arg("stop").output();
    std::thread::sleep(Duration::from_millis(500));

    let out = Command::new(cli_bin())
        .args(["ping", "--bench", "100"])
        .output()
        .expect("run fastuse-cli ping --bench 100");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    println!("stdout: {stdout}");
    println!("stderr: {stderr}");
    assert!(out.status.success(), "ping --bench 100 exit failure");

    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("ping --bench output is JSON");
    let p50 = json["warm_p50_us"].as_u64().expect("warm_p50_us field");
    let p99 = json["warm_p99_us"].as_u64().expect("warm_p99_us field");
    let cold = json["cold_total_us"].as_u64().expect("cold_total_us field");
    println!("cold: {cold} us, warm p50: {p50} us, warm p99: {p99} us");
    assert!(
        p50 < 5_000,
        "warm p50 {} us exceeded 5000 us target",
        p50
    );

    // Cleanup.
    let _ = Command::new(cli_bin()).arg("stop").output();
}
