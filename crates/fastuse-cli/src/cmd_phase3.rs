//! Phase 3 capture + UIA CLI subcommands.
//!
//! Same dispatch shape as `cmd_phase2.rs`. Image responses can be written to
//! a path via `--out` for `screenshot` / `screenshot-region`.

use std::path::Path;

use fastuse_proto::{ActionOpts, ImageFormat, Redact, Request, Response, Selector, TreeView};
use serde_json::json;
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::cmd_phase2::print_action_or_ack;
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

fn parse_format(s: Option<&str>) -> ImageFormat {
    match s.map(|t| t.to_lowercase()) {
        Some(t) if t == "png" => ImageFormat::Png,
        _ => ImageFormat::Jpeg,
    }
}

fn parse_view(s: Option<&str>) -> TreeView {
    match s.map(|t| t.to_lowercase()) {
        Some(t) if t == "raw" => TreeView::Raw,
        _ => TreeView::Content,
    }
}

fn parse_selector(s: &str) -> anyhow::Result<Selector> {
    serde_json::from_str(s).map_err(|e| anyhow::anyhow!("selector: {e}"))
}

pub async fn screenshot(
    pipe_path: &str,
    monitor: Option<u32>,
    format: Option<&str>,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    let req = Request::Screenshot {
        monitor,
        format: Some(parse_format(format)),
    };
    handle_screenshot(one_call(pipe_path, req).await?, out)
}

pub async fn screenshot_region(
    pipe_path: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    monitor: Option<u32>,
    format: Option<&str>,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    let req = Request::ScreenshotRegion {
        x,
        y,
        w,
        h,
        monitor,
        format: Some(parse_format(format)),
    };
    handle_screenshot(one_call(pipe_path, req).await?, out)
}

fn handle_screenshot(res: Response, out: Option<&Path>) -> anyhow::Result<()> {
    match res {
        Response::Screenshot {
            bytes,
            mime,
            width,
            height,
        } => {
            let bytes = bytes.into_inner();
            if let Some(p) = out {
                std::fs::write(p, &bytes)?;
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "mime": mime,
                        "width": width,
                        "height": height,
                        "path": p.to_string_lossy(),
                        "bytes": bytes.len(),
                    })
                );
            } else {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "mime": mime,
                        "width": width,
                        "height": height,
                        "data_b64": b64,
                    })
                );
            }
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn uia_tree(
    pipe_path: &str,
    hwnd: Option<u64>,
    depth: Option<u32>,
    view: Option<&str>,
) -> anyhow::Result<()> {
    let req = Request::UiaTree {
        hwnd,
        depth,
        view: Some(parse_view(view)),
    };
    match one_call(pipe_path, req).await? {
        Response::UiaTree { root, degraded } => {
            println!(
                "{}",
                json!({"ok": true, "degraded": degraded, "root": root})
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn uia_query(
    pipe_path: &str,
    selector_json: &str,
    root_hwnd: Option<u64>,
) -> anyhow::Result<()> {
    let selector = parse_selector(selector_json)?;
    let req = Request::UiaQuery {
        selector,
        root_hwnd,
    };
    match one_call(pipe_path, req).await? {
        Response::UiaQuery { matches, degraded } => {
            println!(
                "{}",
                json!({"ok": true, "degraded": degraded, "matches": matches})
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn inspect_at_point(pipe_path: &str, x: i32, y: i32) -> anyhow::Result<()> {
    let req = Request::InspectAtPoint { x, y };
    match one_call(pipe_path, req).await? {
        Response::Inspect { node, degraded } => {
            println!(
                "{}",
                json!({"ok": true, "degraded": degraded, "node": node})
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn click_element(
    pipe_path: &str,
    selector_json: &str,
    mods: Option<&str>,
    opts: Option<ActionOpts>,
) -> anyhow::Result<()> {
    let selector = parse_selector(selector_json)?;
    let modifiers = mods.map(|s| {
        s.split(',')
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
    });
    let req = Request::ClickElement { selector, modifiers, opts };
    print_action_or_ack(one_call(pipe_path, req).await?)
}

pub async fn type_into_element(
    pipe_path: &str,
    selector_json: &str,
    text: String,
    opts: Option<ActionOpts>,
) -> anyhow::Result<()> {
    let selector = parse_selector(selector_json)?;
    let req = Request::TypeIntoElement {
        selector,
        text: Redact::new(text),
        opts,
    };
    print_action_or_ack(one_call(pipe_path, req).await?)
}

pub async fn wait_for_element(
    pipe_path: &str,
    selector_json: &str,
    timeout_ms: u32,
) -> anyhow::Result<()> {
    let selector = parse_selector(selector_json)?;
    let req = Request::WaitForElement {
        selector,
        timeout_ms,
    };
    print_match(one_call(pipe_path, req).await?)
}

pub async fn scroll_into_view(pipe_path: &str, selector_json: &str, opts: Option<ActionOpts>) -> anyhow::Result<()> {
    let selector = parse_selector(selector_json)?;
    let req = Request::ScrollIntoView { selector, opts };
    print_action_or_ack(one_call(pipe_path, req).await?)
}

fn print_match(res: Response) -> anyhow::Result<()> {
    match res {
        Response::Element { matched } => {
            println!("{}", json!({"ok": true, "matched": matched}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

fn print_err(e: fastuse_proto::Error) -> anyhow::Result<()> {
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
