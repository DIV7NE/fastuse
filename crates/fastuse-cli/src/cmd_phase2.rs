//! Phase 2 input + window CLI subcommands.
//!
//! All subcommands follow the same shape:
//!   1. `connect_or_spawn` to bring up the daemon if needed.
//!   2. Hello/Welcome handshake.
//!   3. Send a single Phase 2 `Request`, await `Response`.
//!   4. Print JSON-on-stdout (Phase 1 D-16) and exit.

use fastuse_proto::{
    coords::{MouseButton, ScrollDirection},
    Redact, Request, Response,
};
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

fn parse_button(s: &str) -> anyhow::Result<MouseButton> {
    match s.to_lowercase().as_str() {
        "left" | "l" => Ok(MouseButton::Left),
        "right" | "r" => Ok(MouseButton::Right),
        "middle" | "m" => Ok(MouseButton::Middle),
        other => anyhow::bail!("unknown button: {other}"),
    }
}

fn parse_dir(s: &str) -> anyhow::Result<ScrollDirection> {
    match s.to_lowercase().as_str() {
        "up" => Ok(ScrollDirection::Up),
        "down" => Ok(ScrollDirection::Down),
        "left" => Ok(ScrollDirection::Left),
        "right" => Ok(ScrollDirection::Right),
        other => anyhow::bail!("unknown direction: {other}"),
    }
}

fn parse_mods(s: Option<&str>) -> Vec<String> {
    s.map(|raw| raw.split(',').filter(|t| !t.is_empty()).map(|t| t.to_string()).collect())
        .unwrap_or_default()
}

fn ok_ack(slept_us: Option<u64>) {
    println!("{}", json!({"ok": true, "slept_us": slept_us}));
}

fn print_response(res: Response) -> anyhow::Result<()> {
    match res {
        Response::Ack { slept_us } => ok_ack(slept_us),
        Response::Error(e) => {
            let body = json!({
                "ok": false,
                "error": {
                    "code": e.code.as_str(),
                    "message": e.message,
                    "hint": e.hint,
                }
            });
            eprintln!("{}", body);
            std::process::exit(1);
        }
        Response::Monitors(mons) => println!("{}", serde_json::to_string(&mons)?),
        Response::CursorPos { x, y, monitor_id } => println!(
            "{}",
            json!({"x": x, "y": y, "monitor_id": monitor_id})
        ),
        Response::Window(w) => println!("{}", serde_json::to_string(&w)?),
        Response::Windows(ws) => println!("{}", serde_json::to_string(&ws)?),
        other => println!("{}", json!({"ok": false, "unexpected": format!("{other:?}")})),
    }
    Ok(())
}

pub async fn click(
    pipe_path: &str,
    x: i32,
    y: i32,
    button: &str,
    count: u8,
    mods: Option<&str>,
    no_cursor: bool,
) -> anyhow::Result<()> {
    let req = Request::Click {
        x,
        y,
        button: parse_button(button)?,
        count,
        modifiers: parse_mods(mods),
        skip_set_cursor_pos: no_cursor,
    };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn r#type(pipe_path: &str, text: String) -> anyhow::Result<()> {
    let req = Request::Type { text: Redact::new(text) };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn key(pipe_path: &str, chord: String, repeat: u32) -> anyhow::Result<()> {
    // Early-fail on InvalidChord (CONTEXT.md: shared parser).
    let _ = fastuse_proto::parse_chord(&chord)
        .map_err(|e| anyhow::anyhow!("invalid chord {chord:?}: {e}"))?;
    let req = Request::Key { chord, repeat };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn hold_key(pipe_path: &str, chord: String, ms: u32) -> anyhow::Result<()> {
    let _ = fastuse_proto::parse_chord(&chord)
        .map_err(|e| anyhow::anyhow!("invalid chord {chord:?}: {e}"))?;
    let req = Request::HoldKey { chord, duration_ms: ms };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn mouse_move(pipe_path: &str, x: i32, y: i32) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::MouseMove { x, y }).await?)
}

pub async fn drag(
    pipe_path: &str,
    sx: i32,
    sy: i32,
    ex: i32,
    ey: i32,
    button: &str,
    mods: Option<&str>,
) -> anyhow::Result<()> {
    let req = Request::Drag {
        start_x: sx,
        start_y: sy,
        end_x: ex,
        end_y: ey,
        button: parse_button(button)?,
        modifiers: parse_mods(mods),
    };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn scroll(
    pipe_path: &str,
    x: i32,
    y: i32,
    dir: &str,
    amount: i32,
    mods: Option<&str>,
) -> anyhow::Result<()> {
    let req = Request::Scroll {
        x,
        y,
        direction: parse_dir(dir)?,
        amount,
        modifiers: parse_mods(mods),
    };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn wait(pipe_path: &str, ms: u32) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::Wait { duration_ms: ms }).await?)
}

pub async fn list_monitors(pipe_path: &str) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::ListMonitors).await?)
}

pub async fn cursor_position(pipe_path: &str) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::CursorPosition).await?)
}

pub async fn foreground_window(pipe_path: &str) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::ForegroundWindow).await?)
}

pub async fn list_windows(
    pipe_path: &str,
    process: Option<String>,
    title: Option<String>,
) -> anyhow::Result<()> {
    let req = Request::ListWindows {
        process_name: process,
        title_substring: title,
        visible_only: true,
    };
    print_response(one_call(pipe_path, req).await?)
}

pub async fn focus_window(pipe_path: &str, hwnd: u64) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::FocusWindow { hwnd }).await?)
}

pub async fn resize_move_window(
    pipe_path: &str,
    hwnd: u64,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::ResizeMoveWindow { hwnd, x, y, w, h }).await?)
}

pub async fn mouse_down(pipe_path: &str, button: &str) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::MouseDown { button: parse_button(button)? }).await?)
}

pub async fn mouse_up(pipe_path: &str, button: &str) -> anyhow::Result<()> {
    print_response(one_call(pipe_path, Request::MouseUp { button: parse_button(button)? }).await?)
}
