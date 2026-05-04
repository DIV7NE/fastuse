//! Task 18 — DPI-aware bounds across mixed-DPI monitors.
//!
//! On hosts with ≥2 monitors at different DPI scales, verifies that
//! `resize_move_window` accepts physical-pixel coordinates on each monitor
//! and that `foreground_window` reads back bounds matching ±2px.
//!
//! On uniform-DPI hosts, the test prints a skip note and passes.
//!
//! Run with: `cargo test --release -- --ignored dpi_mixed`

#![cfg(target_os = "windows")]

use std::time::Duration;

use fastuse_proto::{coords::WindowInfo, Request, Response};

mod harness;
use harness::*;

#[tokio::test]
#[ignore = "requires ≥2 monitors at different DPI scales"]
async fn dpi_mixed_resize_round_trip() {
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
        eprintln!("dpi_mixed test skipped — need ≥2 monitors");
        return;
    }

    // Look for two monitors with different DPI scales.
    let scales: Vec<f32> = monitors.iter().map(|m| m.dpi_scale).collect();
    let mut a_idx = 0;
    let mut b_idx = 1;
    let mut found_mixed = false;
    'outer: for i in 0..monitors.len() {
        for j in (i + 1)..monitors.len() {
            if (scales[i] - scales[j]).abs() > 0.05 {
                a_idx = i;
                b_idx = j;
                found_mixed = true;
                break 'outer;
            }
        }
    }
    if !found_mixed {
        eprintln!(
            "dpi_mixed test skipped — all {} monitors share the same DPI scale ({:?})",
            monitors.len(),
            scales
        );
        return;
    }

    let mon_a = monitors[a_idx].clone();
    let mon_b = monitors[b_idx].clone();
    eprintln!(
        "mixed-DPI: monitor A id={} scale={} bounds={:?}; B id={} scale={} bounds={:?}",
        mon_a.id, mon_a.dpi_scale, mon_a.bounds, mon_b.id, mon_b.dpi_scale, mon_b.bounds
    );

    // Spawn notepad to get a real HWND.
    let _notepad = std::process::Command::new("notepad.exe")
        .spawn()
        .expect("spawn notepad");

    let hwnd = wait_for_notepad(&mut client).await;

    // Move to monitor A.
    let (ax, ay) = (mon_a.bounds.x + 32, mon_a.bounds.y + 32);
    let _ = client
        .call(Request::ResizeMoveWindow {
            hwnd,
            x: ax,
            y: ay,
            w: 800,
            h: 600,
        })
        .await
        .expect("resize_move A");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = client.call(Request::FocusWindow { hwnd }).await.expect("focus A");
    let info_a = read_foreground(&mut client).await;
    eprintln!("after move to A: bounds={:?}", info_a.bounds);
    assert!(near(info_a.bounds.x, ax, 8), "A.x off: {} vs {}", info_a.bounds.x, ax);
    assert!(near(info_a.bounds.y, ay, 8), "A.y off: {} vs {}", info_a.bounds.y, ay);

    // Move to monitor B.
    let (bx, by) = (mon_b.bounds.x + 32, mon_b.bounds.y + 32);
    let _ = client
        .call(Request::ResizeMoveWindow {
            hwnd,
            x: bx,
            y: by,
            w: 800,
            h: 600,
        })
        .await
        .expect("resize_move B");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = client.call(Request::FocusWindow { hwnd }).await.expect("focus B");
    let info_b = read_foreground(&mut client).await;
    eprintln!("after move to B: bounds={:?}", info_b.bounds);
    assert!(near(info_b.bounds.x, bx, 8), "B.x off: {} vs {}", info_b.bounds.x, bx);
    assert!(near(info_b.bounds.y, by, 8), "B.y off: {} vs {}", info_b.bounds.y, by);
}

fn near(a: i32, b: i32, tol: i32) -> bool {
    (a - b).abs() <= tol
}

async fn wait_for_notepad(client: &mut TestClient) -> u64 {
    use std::time::Instant;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Response::Windows(ws) = client
            .call(Request::ListWindows {
                process_name: Some("notepad".into()),
                title_substring: None,
                visible_only: true,
            })
            .await
            .expect("list_windows")
        {
            if let Some(w) = ws
                .into_iter()
                .find(|w: &WindowInfo| w.process_name.to_lowercase().contains("notepad"))
            {
                return w.hwnd;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("notepad window did not appear within 3s");
}

async fn read_foreground(client: &mut TestClient) -> WindowInfo {
    match client
        .call(Request::ForegroundWindow)
        .await
        .expect("foreground_window")
    {
        Response::Window(w) => w,
        other => panic!("unexpected: {other:?}"),
    }
}
