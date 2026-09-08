//! Phase 4 system-surface CLI subcommands.
//!
//! Pattern matches `cmd_phase3.rs`: one_call + handshake helpers, JSON output.

use fastuse_proto::{
    ClipFormat, ClipboardGet, ClipboardGetResp, ClipboardSet, KillProcess, LaunchApp,
    ListProcesses, ProcFilter, ProcessSelector, Redact, Request, Response, ShellExec, ShellKind,
};
use std::time::Duration;
use serde_json::json;
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::proto_io::{read_response, write_request};
use crate::spawn::connect_or_spawn;

async fn one_call(pipe_path: &str, req: Request) -> anyhow::Result<Response> {
    let mut pipe = connect_or_spawn(pipe_path).await?;
    handshake(&mut pipe).await?;
    write_request(&mut pipe, &req).await?;
    Ok(read_response(&mut pipe).await?)
}

async fn handshake(pipe: &mut NamedPipeClient) -> anyhow::Result<()> {
    let hello = Request::Hello {
        client_kind: "cli".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        requested_idle_timeout_secs: None,
    };
    write_request(pipe, &hello).await?;
    let _ = read_response(pipe).await?;
    Ok(())
}

fn parse_shell(s: Option<&str>) -> anyhow::Result<ShellKind> {
    Ok(match s.map(str::to_lowercase).as_deref() {
        Some("powershell") => ShellKind::Powershell,
        Some("pwsh") => ShellKind::Pwsh,
        Some("bash") => ShellKind::Bash,
        Some("cmd") | None => ShellKind::Cmd,
        Some(other) => anyhow::bail!("unknown shell: {other}"),
    })
}

fn parse_proc_selector(s: &str) -> anyhow::Result<ProcessSelector> {
    if let Some(pid) = s.strip_prefix("pid:") {
        Ok(ProcessSelector::Pid(pid.parse()?))
    } else if let Some(name) = s.strip_prefix("name:") {
        Ok(ProcessSelector::Name(name.to_string()))
    } else {
        anyhow::bail!("selector must be 'pid:<n>' or 'name:<stem>'")
    }
}

pub async fn clipboard_get_text(pipe_path: &str) -> anyhow::Result<()> {
    let req = Request::ClipboardGet(ClipboardGet { format: Some(ClipFormat::Text) });
    match one_call(pipe_path, req).await? {
        Response::ClipboardGet(ClipboardGetResp::Text { text }) => {
            println!("{}", json!({"ok": true, "present": true, "text": text.into_inner()}));
            Ok(())
        }
        Response::ClipboardGet(_) => {
            println!("{}", json!({"ok": true, "present": false}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn clipboard_set_text(pipe_path: &str, text: String) -> anyhow::Result<()> {
    let req = Request::ClipboardSet { req: ClipboardSet::Text(Redact::new(text)), opts: None };
    match one_call(pipe_path, req).await? {
        Response::ClipboardSet => {
            println!("{}", json!({"ok": true}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn clipboard_set_files(
    pipe_path: &str,
    paths: Vec<String>,
    paste: bool,
    hwnd: Option<u64>,
) -> anyhow::Result<()> {
    let req = Request::ClipboardSet {
        req: ClipboardSet::Files { paths: Redact::new(paths), paste, hwnd },
        opts: None,
    };
    match one_call(pipe_path, req).await? {
        Response::ClipboardSet => {
            println!("{}", json!({"ok": true}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn file_dialog_set(
    pipe_path: &str,
    paths: Vec<String>,
    hwnd: Option<u64>,
    wait_for_dialog_ms: u32,
    wait_for_close_ms: u32,
    submit: bool,
    allow_new: bool,
) -> anyhow::Result<()> {
    let req = Request::FileDialogSet {
        paths: Redact::new(paths),
        hwnd,
        wait_for_dialog_ms,
        wait_for_close_ms,
        submit,
        allow_new,
        opts: None,
    };
    match one_call(pipe_path, req).await? {
        Response::FileDialog(r) => {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "dialog_hwnd": r.dialog_hwnd,
                    "closed": r.closed,
                    "fill_method": r.fill_method,
                    "follow_up_dialogs": r.follow_up_dialogs.len(),
                })
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn drag_files(
    pipe_path: &str,
    paths: Vec<String>,
    x: i32,
    y: i32,
    start_x: Option<i32>,
    start_y: Option<i32>,
) -> anyhow::Result<()> {
    let req = Request::DragFiles {
        paths: Redact::new(paths), x, y, start_x, start_y,
        coordinates_native: true,
        opts: None,
    };
    match one_call(pipe_path, req).await? {
        Response::Drag(r) => {
            println!("{}", json!({"ok": true, "dropped": r.dropped, "effect": r.effect}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn shell_exec(
    pipe_path: &str,
    command: String,
    shell: Option<&str>,
    cwd: Option<String>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let req = Request::ShellExec(ShellExec {
        command: Redact::new(command),
        shell: Some(parse_shell(shell)?),
        env: None,
        cwd,
        timeout_ms,
        stream_chunk_size: None,
    });
    match one_call(pipe_path, req).await? {
        Response::ShellExec(r) => {
            use base64::Engine;
            println!(
                "{}",
                json!({
                    "ok": true,
                    "status": r.status,
                    "truncated": r.truncated,
                    "duration_ms": r.duration_ms,
                    "stdout_b64": base64::engine::general_purpose::STANDARD.encode(r.stdout.into_inner()),
                    "stderr_b64": base64::engine::general_purpose::STANDARD.encode(r.stderr.into_inner()),
                })
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn launch_app(
    pipe_path: &str,
    query: String,
    focus: bool,
    capture_output: bool,
) -> anyhow::Result<()> {
    let req = Request::LaunchApp { req: LaunchApp { query, capture_output }, opts: None };
    let resp = match one_call(pipe_path, req).await? {
        Response::LaunchApp(r) => r,
        Response::Error(e) => return print_err(e),
        other => return Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    };

    if !focus || resp.pid == 0 {
        println!(
            "{}",
            json!({"ok": true, "pid": resp.pid, "hwnd": resp.hwnd, "title": resp.title, "class": resp.class, "log_path": resp.log_path})
        );
        return Ok(());
    }

    // --focus: snapshot the current foreground HWND, then poll until it
    // changes (meaning the launched app stole focus). Timeout 4s. Works for
    // UWP stubs (calc.exe) which take 1-2s to hand off to the real process.
    let pre_hwnd = {
        let mut pipe = connect_or_spawn(pipe_path).await?;
        handshake(&mut pipe).await?;
        write_request(&mut pipe, &Request::ForegroundWindow).await?;
        match read_response(&mut pipe).await? {
            Response::Window(w) => w.hwnd,
            _ => 0,
        }
    };

    let deadline = tokio::time::Instant::now() + Duration::from_millis(4000);
    let focused_window = loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut pipe = connect_or_spawn(pipe_path).await?;
        handshake(&mut pipe).await?;
        write_request(&mut pipe, &Request::ForegroundWindow).await?;
        match read_response(&mut pipe).await? {
            Response::Window(w) if w.hwnd != pre_hwnd && w.hwnd != 0 => break Some(w),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            break None;
        }
    };

    match focused_window {
        Some(w) => {
            // Re-assert focus so the model can immediately start typing.
            let mut pipe = connect_or_spawn(pipe_path).await?;
            handshake(&mut pipe).await?;
            write_request(&mut pipe, &Request::FocusWindow { hwnd: w.hwnd, opts: None }).await?;
            let _ = read_response(&mut pipe).await;
            println!(
                "{}",
                json!({"ok": true, "pid": resp.pid, "hwnd": w.hwnd, "title": w.title, "class": w.class, "focused": true, "log_path": resp.log_path})
            );
        }
        None => {
            println!(
                "{}",
                json!({"ok": true, "pid": resp.pid, "hwnd": resp.hwnd, "title": resp.title, "class": resp.class, "log_path": resp.log_path})
            );
        }
    }
    Ok(())
}

pub async fn list_processes(
    pipe_path: &str,
    name_contains: Option<String>,
    visible_only: Option<bool>,
) -> anyhow::Result<()> {
    let filter = (name_contains.is_some() || visible_only.is_some()).then(|| ProcFilter {
        name_contains,
        visible_only,
    });
    let req = Request::ListProcesses(ListProcesses { filter });
    match one_call(pipe_path, req).await? {
        Response::ListProcesses(v) => {
            println!("{}", json!({"ok": true, "processes": v}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn kill_process(
    pipe_path: &str,
    selector: String,
    force: Option<bool>,
    process_tree: Option<bool>,
) -> anyhow::Result<()> {
    let req = Request::KillProcess(KillProcess {
        selector: parse_proc_selector(&selector)?,
        force,
        process_tree,
    });
    match one_call(pipe_path, req).await? {
        Response::KillProcess { terminated } => {
            println!("{}", json!({"ok": true, "terminated": terminated}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

fn print_err(e: fastuse_proto::Error) -> anyhow::Result<()> {
    let body = json!({
        "ok": false,
        "error": { "code": e.code.as_str(), "message": e.message, "hint": e.hint }
    });
    eprintln!("{body}");
    std::process::exit(1);
}
