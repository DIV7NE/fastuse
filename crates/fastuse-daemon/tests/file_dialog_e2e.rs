//! `file_dialog_set` end-to-end against Notepad's Open and Save dialogs.
//!
//! Run with: `cargo test -p fastuse-daemon --test file_dialog_e2e -- --ignored`
//! Requires a logged-in desktop session and a running daemon.

#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use fastuse_proto::{Redact, Request, Response};

mod harness;
use harness::*;

/// Notepad's **Open** dialog: the path exists, so the strict resolver accepts
/// it, and no overwrite prompt can appear.
#[tokio::test]
#[ignore = "E2E — spawns notepad.exe; requires a running daemon"]
async fn file_dialog_set_opens_notepad_document() {
    let mut client = connect_or_skip().await;

    let target = std::env::temp_dir().join("fastuse_dialog_open_e2e.txt");
    std::fs::write(&target, b"upload plan").expect("seed the file to open");

    let hwnd = spawn_notepad(&mut client).await;
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
            allow_new: false,
            opts: None,
        })
        .await
        .unwrap();

    close_notepad(&mut client, hwnd).await;
    std::fs::remove_file(&target).ok();

    match resp {
        Response::FileDialog(r) => {
            // Which fill path fired matters: WM_SETTEXT rescuing a silently
            // broken UIA path would otherwise pass unnoticed.
            println!("open: fill_method = {}", r.fill_method);
            assert!(r.closed, "dialog did not close: {r:?}");
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

/// Notepad's **Save As** dialog naming a file that does not exist yet — the
/// case `allow_new` exists for. Nothing to overwrite, so the dialog closes and
/// Notepad writes the file.
#[tokio::test]
#[ignore = "E2E — spawns notepad.exe; requires a running daemon"]
async fn file_dialog_set_saves_new_notepad_document() {
    let mut client = connect_or_skip().await;

    let target = std::env::temp_dir().join("fastuse_dialog_save_new_e2e.txt");
    std::fs::remove_file(&target).ok();

    let hwnd = spawn_notepad(&mut client).await;
    client
        .call(Request::Type {
            text: Redact::new("upload plan".to_string()),
            opts: None,
        })
        .await
        .unwrap();
    open_save_dialog(&mut client, hwnd).await;

    let resp = client
        .call(Request::FileDialogSet {
            paths: Redact::new(vec![target.to_string_lossy().into_owned()]),
            hwnd: Some(hwnd),
            wait_for_dialog_ms: 5000,
            wait_for_close_ms: 5000,
            submit: true,
            allow_new: true,
            opts: None,
        })
        .await
        .unwrap();

    let written = std::fs::read_to_string(&target).ok();
    close_notepad(&mut client, hwnd).await;
    std::fs::remove_file(&target).ok();

    match resp {
        Response::FileDialog(r) => {
            println!("save-new: fill_method = {}", r.fill_method);
            assert!(r.closed, "dialog did not close: {r:?}");
        }
        other => panic!("unexpected response: {other:?}"),
    }
    // Notepad wrote the file it was told to write. The exact bytes are not
    // this test's business: the window restores previous session tabs, and
    // `Type` into a freshly restored Notepad tab is lossy (an "upload plan"
    // arrived as "loadupload nnnn" here), which says something about the
    // typing primitive, not about the dialog being filled and submitted.
    let written = written.expect("Notepad did not write the file");
    assert!(!written.is_empty(), "Notepad wrote an empty file");
}

/// Save over a file that already exists: Notepad stacks an overwrite-confirm
/// on top of the Save dialog. The tool must report that prompt rather than
/// mistake the still-open dialog for success — answering it is destructive and
/// belongs to the caller.
#[tokio::test]
#[ignore = "E2E — spawns notepad.exe; requires a running daemon"]
async fn file_dialog_set_reports_overwrite_prompt() {
    let mut client = connect_or_skip().await;

    let target = std::env::temp_dir().join("fastuse_dialog_overwrite_e2e.txt");
    std::fs::write(&target, b"already here").expect("seed the file to overwrite");

    let hwnd = spawn_notepad(&mut client).await;
    client
        .call(Request::Type {
            text: Redact::new("upload plan".to_string()),
            opts: None,
        })
        .await
        .unwrap();
    open_save_dialog(&mut client, hwnd).await;

    let resp = client
        .call(Request::FileDialogSet {
            paths: Redact::new(vec![target.to_string_lossy().into_owned()]),
            hwnd: Some(hwnd),
            wait_for_dialog_ms: 5000,
            // Short: the dialog is expected to stay up behind the prompt.
            wait_for_close_ms: 1500,
            submit: true,
            allow_new: false,
            opts: None,
        })
        .await
        .unwrap();

    // Answer "No" to the prompt (Escape), then leave the Save dialog.
    client
        .call(Request::Key {
            chord: "escape".to_string(),
            repeat: 2,
            opts: None,
        })
        .await
        .unwrap();
    close_notepad(&mut client, hwnd).await;
    let untouched = std::fs::read_to_string(&target).ok();
    std::fs::remove_file(&target).ok();

    match resp {
        Response::FileDialog(r) => {
            assert!(!r.closed, "dialog closed despite the overwrite prompt: {r:?}");
            assert!(
                !r.follow_up_dialogs.is_empty(),
                "overwrite prompt was not reported: {r:?}"
            );
            println!("overwrite: follow_up = {:?}", r.follow_up_dialogs);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert_eq!(
        untouched.as_deref(),
        Some("already here"),
        "the tool must not answer the overwrite prompt itself"
    );
}

/// Spawn a Notepad window, return its HWND, and leave it focused.
///
/// Win11's Notepad is single-instance: the process we spawn hands off and
/// exits, so the new window is found by diffing against the ones already up
/// rather than by pid.
async fn spawn_notepad(client: &mut TestClient) -> u64 {
    // Start from a fresh process. Win11's Notepad is single-instance and its
    // state outlives a window: once this suite has driven one file dialog
    // through it, later windows in the same process stop honouring
    // Ctrl+Shift+S entirely. This closes any Notepad the developer had open —
    // Notepad restores its tabs on next launch, and these tests already take
    // over the desktop's mouse and keyboard.
    let _ = std::process::Command::new("taskkill")
        .args(["/IM", "notepad.exe", "/F"])
        .output();
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let before = notepad_hwnds(client).await;
    std::process::Command::new("notepad.exe")
        .spawn()
        .expect("spawn notepad");

    let deadline = Instant::now() + Duration::from_secs(5);
    let hwnd = loop {
        if let Some(h) = notepad_hwnds(client)
            .await
            .into_iter()
            .find(|h| !before.contains(h))
        {
            break h;
        }
        assert!(Instant::now() < deadline, "notepad window never appeared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // A window this fresh drops the first keystrokes sent to it, chords
    // included: Notepad is a WinUI app and its accelerator table is not live
    // the moment the HWND appears. Give it a beat before focusing.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    focus(client, hwnd).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    hwnd
}

/// Send Save As and keep sending it until the dialog is actually up.
///
/// Save As rather than Save: Win11's Notepad restores session tabs, and Ctrl+S
/// on a tab that already has a file path writes it with no dialog at all.
///
/// The retry is not a timing guess. Opening a file dialog on this desktop
/// raises a shell "Sign in" popup asynchronously, and when it takes the
/// foreground between the focus check and the keystroke the chord lands in it
/// and Notepad never sees it. Retrying on the observable outcome — a `#32770`
/// owned by Notepad — is the only way to be robust to that.
async fn open_save_dialog(client: &mut TestClient, hwnd: u64) {
    for _ in 0..4 {
        focus(client, hwnd).await;
        client
            .call(Request::Key {
                chord: "ctrl+shift+s".to_string(),
                repeat: 1,
                opts: None,
            })
            .await
            .unwrap();
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if dialog_present(client, hwnd).await {
                return;
            }
        }
    }
    panic!("Save As dialog never opened for hwnd {hwnd}");
}

/// True when the process owning `hwnd` has a `#32770` up.
async fn dialog_present(client: &mut TestClient, hwnd: u64) -> bool {
    let Response::Windows(ws) = client
        .call(Request::ListWindows {
            process_name: None,
            title_substring: None,
            visible_only: true,
        })
        .await
        .unwrap()
    else {
        return false;
    };
    let Some(pid) = ws.iter().find(|w| w.hwnd == hwnd).map(|w| w.pid) else {
        return false;
    };
    ws.iter().any(|w| w.pid == pid && w.class == "#32770")
}

/// Focus and prove it, by reading the foreground back.
///
/// `SetForegroundWindow` reporting success is not enough on a live desktop:
/// opening a file dialog here raises a shell "Sign in" popup that takes the
/// foreground a moment later, and every keystroke after that lands in it.
async fn focus(client: &mut TestClient, hwnd: u64) {
    let mut seen = None;
    for _ in 0..10 {
        client
            .call(Request::FocusWindow { hwnd, opts: None })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        match client.call(Request::ForegroundWindow).await.unwrap() {
            Response::Window(w) if w.hwnd == hwnd => return,
            other => seen = Some(other),
        }
    }
    panic!("could not hold the foreground for hwnd {hwnd}; it is held by {seen:?}");
}

async fn notepad_hwnds(client: &mut TestClient) -> Vec<u64> {
    match client
        .call(Request::ListWindows {
            process_name: Some("notepad".into()),
            title_substring: None,
            visible_only: true,
        })
        .await
        .unwrap()
    {
        Response::Windows(ws) => ws
            .into_iter()
            .filter(|w| w.class == "Notepad")
            .map(|w| w.hwnd)
            .collect(),
        other => panic!("unexpected response: {other:?}"),
    }
}

/// Best-effort tidy-up: Escape until this process owns no `#32770`.
///
/// Not a closing chord (Ctrl+W, Alt+F4): one of those leaves its modifier
/// logically down, because the window is gone before the key-up reaches it,
/// and the next test's chord then arrives as a different chord. A leftover
/// window costs nothing — every test finds its own by diffing HWNDs — but a
/// leftover *dialog* is picked up by the next test's discovery poll.
async fn close_notepad(client: &mut TestClient, hwnd: u64) {
    for _ in 0..10 {
        let Response::Windows(ws) = client
            .call(Request::ListWindows {
                process_name: None,
                title_substring: None,
                visible_only: true,
            })
            .await
            .unwrap()
        else {
            return;
        };
        let Some(pid) = ws.iter().find(|w| w.hwnd == hwnd).map(|w| w.pid) else {
            return;
        };
        if !ws.iter().any(|w| w.pid == pid && w.class == "#32770") {
            return;
        }
        client
            .call(Request::Key {
                chord: "escape".to_string(),
                repeat: 1,
                opts: None,
            })
            .await
            .ok();
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    // Best effort: the next test kills the process before it starts, so a
    // stubborn prompt costs it nothing.
}
