//! `fastuse-cli computer <subcommand>` handlers.
//!
//! All subcommands send `ComputerRequest { coordinates_native: true }` so the
//! daemon skips `ScaleStack` translation and uses coordinates as native
//! virtual-desktop pixels directly.

use std::path::Path;

use fastuse_proto::wire::{
    ComputerAction, ComputerRequest, ComputerResult, ScrollDir,
};
use fastuse_proto::{Redact, Request, Response};
use serde_json::json;
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::proto_io::{read_response, write_request};
use crate::spawn::connect_or_spawn;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Connect, handshake, send `req`, return the response.
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

/// Wrap a `ComputerAction` into a `ComputerRequest` with `coordinates_native: true`.
fn native_req(action: ComputerAction) -> Request {
    Request::Computer(ComputerRequest {
        action,
        coordinates_native: true,
    })
}

/// Parse a scroll direction string into `ScrollDir`.
/// Returns `Err` with an exit-code-2-style message on bad input.
fn parse_scroll_dir(s: &str) -> anyhow::Result<ScrollDir> {
    match s.to_lowercase().as_str() {
        "up" => Ok(ScrollDir::Up),
        "down" => Ok(ScrollDir::Down),
        "left" => Ok(ScrollDir::Left),
        "right" => Ok(ScrollDir::Right),
        other => anyhow::bail!(
            "unknown scroll direction {:?}; expected up | down | left | right",
            other
        ),
    }
}

/// Print a `ComputerResult` as JSON, omitting `data_base64` when image bytes
/// were written to `out_path`.
fn print_result(result: &ComputerResult, out_path: Option<&Path>) {
    let scale = result.scale.as_ref().map(|s| {
        json!({
            "ratio": s.ratio,
            "monitor_origin": s.monitor_origin,
            "native_w": s.native_w,
            "native_h": s.native_h,
            "scaled_w": s.scaled_w,
            "scaled_h": s.scaled_h,
        })
    });
    if let Some(img) = &result.image {
        if out_path.is_some() {
            // Image written to disk; omit the base64 blob from stdout.
            println!(
                "{}",
                json!({
                    "ok": result.ok,
                    "format": img.format,
                    "width": img.width,
                    "height": img.height,
                    "scale": scale,
                })
            );
        } else {
            println!(
                "{}",
                json!({
                    "ok": result.ok,
                    "format": img.format,
                    "width": img.width,
                    "height": img.height,
                    "data_base64": img.data_base64,
                    "scale": scale,
                })
            );
        }
    } else if let Some(cursor) = result.cursor {
        println!("{}", json!({ "ok": result.ok, "cursor": cursor }));
    } else {
        println!("{}", json!({ "ok": result.ok }));
    }
}

/// Dispatch a computer action: send the request, handle the response.
///
/// `out_path` is used only for `Screenshot` — when `Some`, the raw image
/// bytes are decoded from `data_base64` and written to disk.
async fn dispatch(
    pipe_path: &str,
    action: ComputerAction,
    out_path: Option<&Path>,
) -> anyhow::Result<()> {
    let resp = one_call(pipe_path, native_req(action)).await?;
    match resp {
        Response::Computer(result) => {
            // Write image bytes to disk when `--out` was provided.
            if let (Some(img), Some(path)) = (&result.image, out_path) {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&img.data_base64)
                    .map_err(|e| anyhow::anyhow!("base64 decode: {e}"))?;
                std::fs::write(path, &bytes)?;
            }
            print_result(&result, out_path);
            Ok(())
        }
        Response::Error(e) => anyhow::bail!("{}", e.message),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Public subcommand handlers
// ---------------------------------------------------------------------------

/// `computer screenshot [--out PATH] [--monitor N] [--format jpeg|png]`
pub async fn screenshot(
    pipe_path: &str,
    out: Option<&Path>,
    monitor: Option<u32>,
    format: &str,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::Screenshot { monitor, format: Some(parse_image_format(format)) },
        out,
    )
    .await
}

/// Map the CLI's `--format` string onto the wire enum. Anything unrecognised
/// falls back to the JPEG default rather than failing the capture.
fn parse_image_format(s: &str) -> fastuse_proto::ImageFormat {
    match s.trim().to_ascii_lowercase().as_str() {
        "png" => fastuse_proto::ImageFormat::Png,
        _ => fastuse_proto::ImageFormat::Jpeg,
    }
}

/// `computer left-click X Y [--modifiers "ctrl+shift"] [--instant]`
pub async fn left_click(
    pipe_path: &str,
    x: i32,
    y: i32,
    modifiers: Option<String>,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::LeftClick {
            coordinate: [x, y],
            text: modifiers.map(Redact::new),
            humanize: !instant,
        },
        None,
    )
    .await
}

/// `computer right-click X Y [--instant]`
pub async fn right_click(
    pipe_path: &str,
    x: i32,
    y: i32,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::RightClick { coordinate: [x, y], humanize: !instant },
        None,
    )
    .await
}

/// `computer middle-click X Y [--instant]`
pub async fn middle_click(
    pipe_path: &str,
    x: i32,
    y: i32,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::MiddleClick { coordinate: [x, y], humanize: !instant },
        None,
    )
    .await
}

/// `computer double-click X Y [--instant]`
pub async fn double_click(
    pipe_path: &str,
    x: i32,
    y: i32,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::DoubleClick { coordinate: [x, y], humanize: !instant },
        None,
    )
    .await
}

/// `computer triple-click X Y [--instant]`
pub async fn triple_click(
    pipe_path: &str,
    x: i32,
    y: i32,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::TripleClick { coordinate: [x, y], humanize: !instant },
        None,
    )
    .await
}

/// `computer drag SX SY EX EY [--modifiers "..."] [--instant]`
pub async fn drag(
    pipe_path: &str,
    sx: i32,
    sy: i32,
    ex: i32,
    ey: i32,
    modifiers: Option<String>,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::LeftClickDrag {
            start_coordinate: [sx, sy],
            coordinate: [ex, ey],
            humanize: !instant,
            text: modifiers.map(Redact::new),
        },
        None,
    )
    .await
}

/// `computer type "text" [--instant]`
pub async fn type_text(
    pipe_path: &str,
    text: String,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::Type {
            text: Redact::new(text),
            humanize: !instant,
        },
        None,
    )
    .await
}

/// `computer key CHORD`
pub async fn key(pipe_path: &str, chord: String) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::Key { text: Redact::new(chord) },
        None,
    )
    .await
}

/// `computer hold-key CHORD --ms N`
pub async fn hold_key(pipe_path: &str, chord: String, ms: u32) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::HoldKey {
            text: Redact::new(chord),
            duration: ms,
        },
        None,
    )
    .await
}

/// `computer scroll X Y --direction up|down|left|right --amount N`
///
/// Returns exit code 2 on bad direction string.
pub async fn scroll(
    pipe_path: &str,
    x: i32,
    y: i32,
    direction: &str,
    amount: i32,
) -> anyhow::Result<()> {
    let dir = parse_scroll_dir(direction).map_err(|e| {
        eprintln!("fastuse-cli: {e}");
        std::process::exit(2);
        // Unreachable — satisfies the type checker.
        #[allow(unreachable_code)]
        e
    })?;
    dispatch(
        pipe_path,
        ComputerAction::Scroll {
            coordinate: [x, y],
            scroll_direction: dir,
            scroll_amount: amount,
        },
        None,
    )
    .await
}

/// `computer mouse-move X Y [--instant]`
pub async fn mouse_move(
    pipe_path: &str,
    x: i32,
    y: i32,
    instant: bool,
) -> anyhow::Result<()> {
    dispatch(
        pipe_path,
        ComputerAction::MouseMove { coordinate: [x, y], humanize: !instant },
        None,
    )
    .await
}

/// `computer cursor-position`
pub async fn cursor_position(pipe_path: &str) -> anyhow::Result<()> {
    dispatch(pipe_path, ComputerAction::CursorPosition, None).await
}

/// `computer wait MS`
pub async fn wait(pipe_path: &str, ms: u32) -> anyhow::Result<()> {
    dispatch(pipe_path, ComputerAction::Wait { duration: ms }, None).await
}

/// `computer zoom X Y [--factor 2.0]`
pub async fn zoom(pipe_path: &str, x: i32, y: i32, factor: f32) -> anyhow::Result<()> {
    dispatch(pipe_path, ComputerAction::Zoom { coordinate: [x, y], zoom_factor: factor }, None)
        .await
}

/// `computer left-mouse-down [--x X] [--y Y]`
pub async fn left_mouse_down(
    pipe_path: &str,
    x: Option<i32>,
    y: Option<i32>,
) -> anyhow::Result<()> {
    let coordinate = match (x, y) {
        (Some(cx), Some(cy)) => Some([cx, cy]),
        _ => None,
    };
    dispatch(pipe_path, ComputerAction::LeftMouseDown { coordinate }, None).await
}

/// `computer left-mouse-up [--x X] [--y Y]`
pub async fn left_mouse_up(
    pipe_path: &str,
    x: Option<i32>,
    y: Option<i32>,
) -> anyhow::Result<()> {
    let coordinate = match (x, y) {
        (Some(cx), Some(cy)) => Some([cx, cy]),
        _ => None,
    };
    dispatch(pipe_path, ComputerAction::LeftMouseUp { coordinate }, None).await
}
