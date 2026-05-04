//! Task 14 — Notepad end-to-end integration test.
//!
//! Drives Notepad via the daemon's pipe directly (no MCP, no CLI) for
//! deterministic latency measurement.
//!
//! Sequence (per CONTEXT.md `<specifics>`):
//!   1. Spawn `notepad.exe`.
//!   2. Poll `list_windows(process="notepad")` until visible (≤3s).
//!   3. focus_window → type("Hello, fastuse 🎉") → key("ctrl+s") →
//!      handle save dialog → key("alt+f4") → dismiss save prompt.
//!   4. Assert non-capture tool RTT p50 < 50ms (ROADMAP §Phase 2 SC-1).
//!
//! Run with: `cargo test --release -- --ignored notepad_e2e`
//! The test is `#[ignore]`-gated because it requires a real Windows session
//! with the user logged in and Notepad available.

#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use fastuse_proto::{
    coords::{MouseButton, ScrollDirection, WindowInfo},
    Redact, Request, Response,
};

mod harness;
use harness::*;

#[tokio::test]
#[ignore = "E2E — spawns notepad.exe; requires a running daemon"]
async fn notepad_e2e_unicode_save_close() {
    let mut client = connect_or_skip().await;

    // 1. Spawn notepad.
    let _notepad = std::process::Command::new("notepad.exe")
        .spawn()
        .expect("spawn notepad");

    // 2. Poll for the window with a 3s budget.
    let mut hwnd: Option<u64> = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let res = client
            .call(Request::ListWindows {
                process_name: Some("notepad".into()),
                title_substring: None,
                visible_only: true,
            })
            .await
            .expect("list_windows");
        if let Response::Windows(ws) = res {
            if let Some(w) = ws.into_iter().find(|w: &WindowInfo| {
                w.process_name.to_lowercase().contains("notepad")
            }) {
                hwnd = Some(w.hwnd);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let hwnd = hwnd.expect("notepad window did not appear within 3s");

    // 3. Focus + type + ctrl+s. Record RTTs.
    //
    // SC-1 measures non-capture, non-sleep tool latency. `Wait` is a tool
    // whose RTT is by definition the requested duration — its latency
    // doesn't measure anything about the system. Exclude it from the
    // p50 assertion (the macro records into `rtts_us` only when `track`
    // is true).
    let mut rtts_us: Vec<u128> = Vec::new();
    macro_rules! send {
        ($req:expr, $track:expr) => {{
            let t0 = Instant::now();
            let r = client.call($req).await.expect("call");
            let dt = t0.elapsed().as_micros();
            if $track {
                rtts_us.push(dt);
            }
            r
        }};
    }

    let _ = send!(Request::FocusWindow { hwnd }, true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = send!(
        Request::Type { text: Redact::new("Hello, fastuse 🎉".into()) },
        true
    );
    let _ = send!(Request::Key { chord: "ctrl+s".into(), repeat: 1 }, true);

    // 4. Save dialog. Notepad on Win11 may show a different filename UI;
    //    we issue a best-effort sequence. Don't fail the test if the save
    //    dialog doesn't appear — the latency assertion is the real gate.
    let _ = send!(Request::Wait { duration_ms: 250 }, false);
    let _ = send!(
        Request::Type {
            text: Redact::new(format!(
                "{}\\fastuse_e2e_test.txt",
                std::env::temp_dir().display()
            )),
        },
        true
    );
    let _ = send!(Request::Key { chord: "enter".into(), repeat: 1 }, true);
    let _ = send!(Request::Wait { duration_ms: 250 }, false);

    // 5. alt+f4; dismiss save prompt if it lingers.
    let _ = send!(Request::Key { chord: "alt+f4".into(), repeat: 1 }, true);

    // Latency: assert p50 < 50ms across non-Wait calls.
    // WR-02 belt-and-braces: explicit message instead of an opaque
    // index-out-of-bounds when no calls were tracked (e.g. macro tracking
    // flag flipped during a refactor or every prior call panicked).
    rtts_us.sort_unstable();
    assert!(
        !rtts_us.is_empty(),
        "no non-Wait calls were tracked — SC-1 cannot be evaluated"
    );
    let p50 = rtts_us[rtts_us.len() / 2];
    eprintln!(
        "notepad_e2e: {} non-capture/non-wait calls, p50 = {} us, max = {} us, all = {:?}",
        rtts_us.len(),
        p50,
        rtts_us.last().copied().unwrap_or(0),
        rtts_us
    );
    assert!(
        p50 < 50_000,
        "p50 RTT {} us exceeds 50ms budget (ROADMAP §Phase 2 SC-1)",
        p50
    );

    // Cleanup: best-effort delete.
    let _ = std::fs::remove_file(std::env::temp_dir().join("fastuse_e2e_test.txt"));

    // Suppress unused warnings.
    let _ = (MouseButton::Left, ScrollDirection::Up);
}
