//! Task 15 — multi-monitor click correctness test.
//!
//! On a host with ≥2 monitors:
//!   1. list_monitors → pick a non-primary one.
//!   2. mouse_move to (mon.bounds.x + 100, mon.bounds.y + 100).
//!   3. cursor_position → assert (x, y) match and monitor_id == picked.id.
//!
//! On a single-monitor host: print a skip note and pass.
//!
//! Run with: `cargo test --release -- --ignored multi_monitor`

#![cfg(target_os = "windows")]

use fastuse_proto::{Request, Response};

mod harness;
use harness::*;

#[tokio::test]
#[ignore = "requires ≥2 physical monitors"]
async fn multi_monitor_cursor_roundtrip() {
    let mut client = connect_or_skip().await;

    let monitors = match client
        .call(Request::ListMonitors)
        .await
        .expect("list_monitors")
    {
        Response::Monitors(v) => v,
        other => panic!("unexpected: {other:?}"),
    };

    if monitors.len() < 2 {
        eprintln!(
            "multi-monitor test skipped — only {} monitor(s) detected",
            monitors.len()
        );
        return;
    }

    // Pick a non-primary monitor; fall back to monitors[1] if all are
    // flagged primary (shouldn't happen, but defensive).
    let mon = monitors
        .iter()
        .find(|m| !m.is_primary)
        .unwrap_or(&monitors[1])
        .clone();

    let target_x = mon.bounds.x + 100;
    let target_y = mon.bounds.y + 100;

    let _ = client
        .call(Request::MouseMove {
            x: target_x,
            y: target_y,
            opts: None,
        })
        .await
        .expect("mouse_move");

    // Brief pause: SetCursorPos is synchronous but some compositors lag.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let pos = match client
        .call(Request::CursorPosition)
        .await
        .expect("cursor_position")
    {
        Response::CursorPos { x, y, monitor_id } => (x, y, monitor_id),
        other => panic!("unexpected: {other:?}"),
    };

    eprintln!(
        "picked monitor id={} bounds={:?}; mouse_move({},{}) → cursor_position {:?}",
        mon.id, mon.bounds, target_x, target_y, pos
    );

    // Allow ±2px tolerance for DWM compositing rounding.
    assert!((pos.0 - target_x).abs() <= 2, "x mismatch: got {} want {}", pos.0, target_x);
    assert!((pos.1 - target_y).abs() <= 2, "y mismatch: got {} want {}", pos.1, target_y);
    assert_eq!(
        pos.2, mon.id,
        "cursor monitor_id ({}) does not match picked monitor ({})",
        pos.2, mon.id
    );
}
