//! `file_dialog_set` end-to-end against Notepad's Save dialog.
//!
//! Run with: `cargo test -p fastuse-daemon --test file_dialog_e2e -- --ignored`
//! Requires a logged-in desktop session and a running daemon.

#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use fastuse_proto::{Redact, Request, Response};

mod harness;
use harness::*;

/// Notepad's **Open** dialog: the path exists, so `resolve_paths` accepts it,
/// and no overwrite prompt can appear — the one native flow where a successful
/// fill+submit is observable as `closed: true`.
#[tokio::test]
#[ignore = "E2E — spawns notepad.exe; requires a running daemon"]
async fn file_dialog_set_opens_notepad_document() {
    let mut client = connect_or_skip().await;

    let target = std::env::temp_dir().join("fastuse_dialog_open_e2e.txt");
    std::fs::write(&target, b"upload plan").expect("seed the file to open");

    let mut notepad = std::process::Command::new("notepad.exe")
        .spawn()
        .expect("spawn notepad");

    let hwnd = wait_for_notepad(&mut client).await;
    client
        .call(Request::FocusWindow { hwnd, opts: None })
        .await
        .unwrap();
    client
        .call(Request::Key {
            chord: "ctrl+o".to_string(),
            repeat: 1,
            opts: None,
        })
        .await
        .unwrap();

    let resp = client
        .call(Request::FileDialogSet {
            paths: Redact::new(vec![target.to_string_lossy().into_owned()]),
            hwnd: Some(hwnd),
            wait_for_dialog_ms: 5000,
            wait_for_close_ms: 5000,
            submit: true,
            opts: None,
        })
        .await
        .unwrap();

    notepad.kill().ok();
    std::fs::remove_file(&target).ok();

    match resp {
        Response::FileDialog(r) => {
            // Which fill path fired matters: WM_SETTEXT rescuing a silently
            // broken UIA path would otherwise pass unnoticed.
            println!("fill_method = {}", r.fill_method);
            assert!(r.closed, "dialog did not close: {r:?}");
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

/// BLOCKED pending a decision on save-mode paths: `resolve_paths` rejects a
/// not-yet-existing target, and pre-creating one makes Notepad raise the
/// overwrite prompt, which by design leaves `closed: false`. See the task-2
/// report.
#[tokio::test]
#[ignore = "E2E — blocked: see file_dialog_set_opens_notepad_document"]
async fn file_dialog_set_saves_notepad_document() {
    let mut client = connect_or_skip().await;

    let target = std::env::temp_dir().join("fastuse_dialog_e2e.txt");
    std::fs::remove_file(&target).ok();

    let mut notepad = std::process::Command::new("notepad.exe")
        .spawn()
        .expect("spawn notepad");

    // Wait for the editor window, then type something so Save has content.
    let hwnd = wait_for_notepad(&mut client).await;
    client
        .call(Request::FocusWindow { hwnd, opts: None })
        .await
        .unwrap();
    client
        .call(Request::Type {
            text: Redact::new("upload plan".to_string()),
            opts: None,
        })
        .await
        .unwrap();
    client
        .call(Request::Key {
            chord: "ctrl+s".to_string(),
            repeat: 1,
            opts: None,
        })
        .await
        .unwrap();

    // The tool does the waiting: dialog appears, gets filled, submits, closes.
    let resp = client
        .call(Request::FileDialogSet {
            paths: Redact::new(vec![target.to_string_lossy().into_owned()]),
            hwnd: Some(hwnd),
            wait_for_dialog_ms: 5000,
            wait_for_close_ms: 5000,
            submit: true,
            opts: None,
        })
        .await
        .unwrap();

    match resp {
        Response::FileDialog(r) => {
            // Which fill path fired matters: WM_SETTEXT rescuing a silently
            // broken UIA path would otherwise pass unnoticed.
            println!("fill_method = {}", r.fill_method);
            assert!(r.closed, "dialog did not close: {r:?}");
        }
        other => panic!("unexpected response: {other:?}"),
    }

    assert!(target.exists(), "Notepad did not write {}", target.display());

    notepad.kill().ok();
    std::fs::remove_file(&target).ok();
}

async fn wait_for_notepad(client: &mut TestClient) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let r = client
            .call(Request::ListWindows {
                process_name: Some("notepad".into()),
                title_substring: None,
                visible_only: true,
            })
            .await
            .unwrap();
        if let Response::Windows(ws) = r {
            if let Some(w) = ws.first() {
                return w.hwnd;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("notepad window never appeared");
}
