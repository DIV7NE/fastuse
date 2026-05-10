//! Phase 3 capture + UIA CLI subcommands.
//!
//! Same dispatch shape as `cmd_phase2.rs`. Image responses can be written to
//! a path via `--out` for `screenshot` / `screenshot-region`.

use std::path::Path;

use fastuse_proto::{ActionOpts, ImageFormat, Request, Response, Selector, TreeView};
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

pub async fn screenshot_window(
    pipe_path: &str,
    hwnd: u64,
    format: Option<&str>,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    let req = Request::ScreenshotWindow {
        hwnd,
        format: Some(parse_format(format)),
    };
    match one_call(pipe_path, req).await? {
        Response::ScreenshotWindow {
            bytes,
            mime,
            client_w,
            client_h,
            monitor_offset_x,
            monitor_offset_y,
            dpi_scale,
        } => {
            let bytes = bytes.into_inner();
            let mut envelope = json!({
                "ok": true,
                "mime": mime,
                "window_size": {"w": client_w, "h": client_h},
                "monitor_offset": {"x": monitor_offset_x, "y": monitor_offset_y},
                "dpi_scale": dpi_scale,
                "bytes": bytes.len(),
            });
            if let Some(p) = out {
                std::fs::write(p, &bytes)?;
                envelope["path"] = json!(p.to_string_lossy());
            } else {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                envelope["data_b64"] = json!(b64);
            }
            println!("{}", envelope);
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
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

pub async fn scroll_into_view(pipe_path: &str, selector_json: &str, opts: Option<ActionOpts>) -> anyhow::Result<()> {
    let selector = parse_selector(selector_json)?;
    let req = Request::ScrollIntoView { selector, opts };
    print_action_or_ack(one_call(pipe_path, req).await?)
}

/// Resolve a UIA selector to an element, focus its containing window if
/// possible, then click the element's bounding-rect centre with native
/// virtual-desktop pixels. v2 thin convenience over `uia-query` +
/// `focus-window` + `computer left-click`.
///
/// Strategy:
/// 1. UIA query → match list
/// 2. Pick first match (caller can pre-filter by selector)
/// 3. If `--focus-window <hwnd>` was passed, focus it first
/// 4. Click bounding_rect centroid via `Request::Click` (count=1, button=Left)
///
/// Note: this does NOT do v1's "ladder" of fallback strategies. UIA
/// degraded → no match → error. For adversarial apps, use vision
/// (screenshot + computer left-click) instead.
pub async fn click_element(
    pipe_path: &str,
    selector_json: &str,
    root_hwnd: Option<u64>,
    focus_first: Option<u64>,
    button: &str,
    count: u8,
    pick_first: bool,
) -> anyhow::Result<()> {
    use fastuse_proto::{ErrorCode, MouseButton};
    let selector = parse_selector(selector_json)?;

    // Step 1: UIA query
    let q_req = Request::UiaQuery { selector, root_hwnd };
    let matches = match one_call(pipe_path, q_req).await? {
        Response::UiaQuery { matches, degraded } => {
            if degraded && matches.is_empty() {
                // Distinct from NoElementMatched — UIA itself couldn't see the
                // tree. Emit a typed error so callers can branch on it.
                println!(
                    "{}",
                    json!({
                        "ok": false,
                        "error": ErrorCode::UiaDegraded.as_str(),
                        "hint": "UIA returned degraded with no matches — use vision (screenshot + computer left-click) on this app",
                    })
                );
                anyhow::bail!("UIA_DEGRADED");
            }
            matches
        }
        Response::Error(e) => {
            return print_err(e);
        }
        other => {
            anyhow::bail!("unexpected uia-query response: {other:?}");
        }
    };
    // Zero matches → NoElementMatched (typed).
    if matches.is_empty() {
        println!(
            "{}",
            json!({
                "ok": false,
                "error": ErrorCode::NoElementMatched.as_str(),
                "hint": "selector resolved to zero elements — consider vision (screenshot + computer left-click)",
            })
        );
        anyhow::bail!("NO_ELEMENT_MATCHED");
    }
    // Multiple matches without `--first` → AmbiguousMatch (typed).
    if matches.len() > 1 && !pick_first {
        let n = matches.len();
        println!(
            "{}",
            json!({
                "ok": false,
                "error": ErrorCode::AmbiguousMatch.as_str(),
                "matches": n,
                "hint": "pass --first to click the first match, or tighten the selector",
            })
        );
        anyhow::bail!("AMBIGUOUS_MATCH");
    }
    let first = matches.into_iter().next().expect("non-empty checked above");
    let r = first.bounding_rect;
    if r.w <= 0 || r.h <= 0 {
        anyhow::bail!(
            "matched element has zero bounding rect (likely off-screen or collapsed) — name={:?} automation_id={:?}",
            first.name,
            first.automation_id
        );
    }
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;

    // Step 2: focus window first if requested (lifts the target from behind
    // any covering window so the click actually lands on it)
    if let Some(hwnd) = focus_first {
        let f_req = Request::FocusWindow { hwnd, opts: None };
        match one_call(pipe_path, f_req).await? {
            Response::Ack { .. } | Response::Window { .. } => {}
            Response::Error(e) => {
                eprintln!(
                    "warning: focus-window failed: [{}] {} — proceeding to click anyway",
                    e.code.as_str(),
                    e.message
                );
            }
            _ => {}
        }
        // Brief settle so the focused window paints before we click.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }

    // Step 3: click the centroid
    let mb = match button.to_lowercase().as_str() {
        "left" | "l" => MouseButton::Left,
        "right" | "r" => MouseButton::Right,
        "middle" | "m" => MouseButton::Middle,
        other => anyhow::bail!("unknown button {other:?}; use left|right|middle"),
    };
    let c_req = Request::Click {
        x: cx,
        y: cy,
        button: mb,
        count,
        modifiers: vec![],
        skip_set_cursor_pos: false,
        opts: None,
    };
    match one_call(pipe_path, c_req).await? {
        Response::Ack { slept_us } => {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "clicked_at": [cx, cy],
                    "element": {
                        "name": first.name,
                        "automation_id": first.automation_id,
                        "control_type": format!("{:?}", first.control_type),
                        "bounds": [r.x, r.y, r.w, r.h],
                    },
                    "slept_us": slept_us,
                })
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
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
