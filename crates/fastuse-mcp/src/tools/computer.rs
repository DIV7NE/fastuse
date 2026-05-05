//! The single `computer` MCP tool exposing Anthropic computer_20251124's action enum.
//!
//! Returns `ImageContent` inline for `Screenshot` / `Zoom` actions so Claude
//! sees the captured frame in the same turn without a separate read step.
//! All other actions return a single `TextContent` block with a JSON summary.

use fastuse_proto::wire::{ComputerAction, ComputerResult, ScaleInfo};
use rmcp::model::{CallToolResult, Content};
use serde_json::json;

/// Build a [`CallToolResult`] from the daemon's [`ComputerResult`].
///
/// Visual actions (`Screenshot`, `Zoom`) push an `ImageContent` block first,
/// followed by a `TextContent` block carrying metadata (action name, scale
/// snapshot, cursor position).  Non-visual actions emit only `TextContent`.
pub fn format_result(action: &ComputerAction, result: ComputerResult) -> CallToolResult {
    let mut contents: Vec<Content> = Vec::new();

    // --- image block (screenshot / zoom only) ---
    if let Some(img) = &result.image {
        let mime = match img.format.as_str() {
            "jpeg" | "jpg" => "image/jpeg",
            "png" => "image/png",
            _ => "application/octet-stream",
        };
        contents.push(Content::image(&img.data_base64, mime));
    }

    // --- text metadata block ---
    let scale_json = result.scale.as_ref().map(scale_to_json);
    let body = json!({
        "ok": result.ok,
        "action": action_name(action),
        "cursor": result.cursor,
        "scale": scale_json,
    });
    contents.push(Content::text(serde_json::to_string(&body).unwrap_or_default()));

    if result.ok {
        CallToolResult::success(contents)
    } else {
        CallToolResult::error(contents)
    }
}

/// Serialize a [`ScaleInfo`] into a compact JSON value for the metadata block.
fn scale_to_json(s: &ScaleInfo) -> serde_json::Value {
    json!({
        "ratio": s.ratio,
        "monitor_origin": s.monitor_origin,
        "native": [s.native_w, s.native_h],
        "scaled": [s.scaled_w, s.scaled_h],
    })
}

/// Return the snake_case action name string for the given [`ComputerAction`].
pub fn action_name(action: &ComputerAction) -> &'static str {
    match action {
        ComputerAction::Screenshot { .. } => "screenshot",
        ComputerAction::LeftClick { .. } => "left_click",
        ComputerAction::RightClick { .. } => "right_click",
        ComputerAction::MiddleClick { .. } => "middle_click",
        ComputerAction::DoubleClick { .. } => "double_click",
        ComputerAction::TripleClick { .. } => "triple_click",
        ComputerAction::LeftClickDrag { .. } => "left_click_drag",
        ComputerAction::LeftMouseDown { .. } => "left_mouse_down",
        ComputerAction::LeftMouseUp { .. } => "left_mouse_up",
        ComputerAction::MouseMove { .. } => "mouse_move",
        ComputerAction::CursorPosition => "cursor_position",
        ComputerAction::Type { .. } => "type",
        ComputerAction::Key { .. } => "key",
        ComputerAction::HoldKey { .. } => "hold_key",
        ComputerAction::Scroll { .. } => "scroll",
        ComputerAction::Wait { .. } => "wait",
        ComputerAction::Zoom { .. } => "zoom",
    }
}
