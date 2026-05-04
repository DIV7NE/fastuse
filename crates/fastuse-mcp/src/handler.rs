//! rmcp 1.6 ServerHandler exposing the Phase 1 `ping` + Phase 2 input/window
//! tools.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fastuse_proto::{
    coords::{MonitorInfo, MouseButton, ScrollDirection, WindowInfo},
    Redact, Request, Response,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::proto_io::{read_response, write_request};
use crate::spawn::connect_or_spawn;

/// fastuse MCP server.
pub struct Fastuse {
    pipe_path: String,
    pipe: Mutex<Option<tokio::net::windows::named_pipe::NamedPipeClient>>,
    tool_router: rmcp::handler::server::tool::ToolRouter<Self>,
}

impl Fastuse {
    pub fn new(pipe_path: String) -> Self {
        Self {
            pipe_path,
            pipe: Mutex::new(None),
            tool_router: Self::tool_router(),
        }
    }

    /// Submit a Request and decode the Response on the shared pipe (lazy
    /// connect + handshake on first call).
    async fn call(&self, req: Request) -> Result<Response, McpError> {
        let mut guard = self.pipe.lock().await;
        if guard.is_none() {
            let mut pipe = connect_or_spawn(&self.pipe_path).await.map_err(io_to_mcp)?;
            write_request(
                &mut pipe,
                &Request::Hello {
                    client_kind: "mcp".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                    requested_idle_timeout_secs: None,
                },
            )
            .await
            .map_err(io_to_mcp)?;
            let _welcome = read_response(&mut pipe).await.map_err(io_to_mcp)?;
            *guard = Some(pipe);
        }
        let pipe = guard.as_mut().expect("just inserted");
        write_request(pipe, &req).await.map_err(io_to_mcp)?;
        read_response(pipe).await.map_err(io_to_mcp)
    }

    fn err_from_proto(e: fastuse_proto::Error) -> McpError {
        McpError::internal_error(format!("[{}] {}", e.code.as_str(), e.message), None)
    }
}

// ---------- output schemas ----------

#[derive(Serialize, schemars::JsonSchema)]
pub struct PingOutput {
    pub ok: bool,
    pub rtt_us: u64,
    pub daemon_pid: u32,
    pub session_id: u32,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct AckOutput {
    pub ok: bool,
    pub slept_us: Option<u64>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct CursorOutput {
    pub x: i32,
    pub y: i32,
    pub monitor_id: u64,
}

// ---------- input schemas ----------

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClickArgs {
    pub x: i32,
    pub y: i32,
    #[serde(default = "left_button")]
    pub button: String,
    #[serde(default = "one_u8")]
    pub count: u8,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default)]
    pub skip_set_cursor_pos: bool,
}

fn left_button() -> String { "left".to_string() }
fn one_u8() -> u8 { 1 }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct XYArgs {
    pub x: i32,
    pub y: i32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ButtonArgs {
    #[serde(default = "left_button")]
    pub button: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DragArgs {
    pub start_x: i32,
    pub start_y: i32,
    pub end_x: i32,
    pub end_y: i32,
    #[serde(default = "left_button")]
    pub button: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ScrollArgs {
    pub x: i32,
    pub y: i32,
    pub direction: String,
    pub amount: i32,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TypeArgs {
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct KeyArgs {
    pub chord: String,
    #[serde(default = "one_u32")]
    pub repeat: u32,
}

fn one_u32() -> u32 { 1 }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct HoldKeyArgs {
    pub chord: String,
    pub duration_ms: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WaitArgs {
    pub duration_ms: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListWindowsArgs {
    pub process_name: Option<String>,
    pub title_substring: Option<String>,
    #[serde(default = "true_b")]
    pub visible_only: bool,
}

fn true_b() -> bool { true }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct HwndArgs {
    pub hwnd: u64,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ResizeMoveArgs {
    pub hwnd: u64,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

// ---------- Phase 3 input schemas ----------

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ScreenshotArgs {
    pub monitor: Option<u32>,
    /// "jpeg" (default, q=85) or "png".
    pub format: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ScreenshotRegionArgs {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub monitor: Option<u32>,
    pub format: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UiaTreeArgs {
    pub hwnd: Option<u64>,
    pub depth: Option<u32>,
    /// "content" (default) or "raw".
    pub view: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SelectorArgs {
    /// Selector JSON: { "ByName": "OK" } / { "ByControlType": "Button" } /
    /// { "And": [...] } etc. -- matches fastuse_proto::Selector serde shape.
    pub selector: serde_json::Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UiaQueryArgs {
    pub selector: serde_json::Value,
    pub root_hwnd: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InspectAtPointArgs {
    pub x: i32,
    pub y: i32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClickElementArgs {
    pub selector: serde_json::Value,
    #[serde(default)]
    pub modifiers: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TypeIntoElementArgs {
    pub selector: serde_json::Value,
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WaitForElementArgs {
    pub selector: serde_json::Value,
    #[serde(default)]
    pub timeout_ms: u32,
}

// ---------- Phase 3 output schemas ----------

#[derive(Serialize, schemars::JsonSchema)]
pub struct ScreenshotOutput {
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// base64-encoded image bytes (T-03-03: never logged in tracing).
    pub data_b64: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct UiaTreeOutput {
    pub root: fastuse_proto::UIANode,
    pub degraded: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct UiaQueryOutput {
    pub matches: Vec<fastuse_proto::UIANode>,
    pub degraded: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct InspectOutput {
    pub node: fastuse_proto::UIANode,
    pub degraded: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ElementMatchOutput {
    pub matched: bool,
}

fn parse_image_format(s: Option<&str>) -> fastuse_proto::ImageFormat {
    match s.map(|t| t.to_lowercase()) {
        Some(t) if t == "png" => fastuse_proto::ImageFormat::Png,
        _ => fastuse_proto::ImageFormat::Jpeg,
    }
}

fn parse_tree_view(s: Option<&str>) -> fastuse_proto::TreeView {
    match s.map(|t| t.to_lowercase()) {
        Some(t) if t == "raw" => fastuse_proto::TreeView::Raw,
        _ => fastuse_proto::TreeView::Content,
    }
}

fn parse_selector(v: serde_json::Value) -> Result<fastuse_proto::Selector, McpError> {
    serde_json::from_value(v)
        .map_err(|e| McpError::invalid_params(format!("selector: {e}"), None))
}

fn parse_button(s: &str) -> Result<MouseButton, McpError> {
    match s.to_lowercase().as_str() {
        "left" | "l" => Ok(MouseButton::Left),
        "right" | "r" => Ok(MouseButton::Right),
        "middle" | "m" => Ok(MouseButton::Middle),
        other => Err(McpError::invalid_params(format!("unknown button: {other}"), None)),
    }
}

fn parse_dir(s: &str) -> Result<ScrollDirection, McpError> {
    match s.to_lowercase().as_str() {
        "up" => Ok(ScrollDirection::Up),
        "down" => Ok(ScrollDirection::Down),
        "left" => Ok(ScrollDirection::Left),
        "right" => Ok(ScrollDirection::Right),
        other => Err(McpError::invalid_params(format!("unknown direction: {other}"), None)),
    }
}

#[tool_router(server_handler)]
impl Fastuse {
    #[tool(name = "ping", description = "Round-trip a ping through the fastuse daemon and return the RTT in microseconds.")]
    async fn ping(&self) -> Result<Json<PingOutput>, McpError> {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| McpError::internal_error(format!("clock: {e}"), None))?
            .as_micros() as u64;
        let send = Instant::now();
        let res = self.call(Request::Ping { ts_us: now_us }).await?;
        let rtt_us = send.elapsed().as_micros() as u64;
        match res {
            Response::Pong { daemon_pid, session_id, .. } => Ok(Json(PingOutput { ok: true, rtt_us, daemon_pid, session_id })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- input ----
    #[tool(name = "click", description = "Click at physical-pixel coordinates with optional modifiers; sets cursor position implicitly unless skip_set_cursor_pos is true.")]
    async fn click(&self, Parameters(args): Parameters<ClickArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::Click {
            x: args.x,
            y: args.y,
            button: parse_button(&args.button)?,
            count: args.count,
            modifiers: args.modifiers,
            skip_set_cursor_pos: args.skip_set_cursor_pos,
        };
        ack(self.call(req).await?)
    }

    #[tool(name = "mouse_move", description = "Move the system cursor to physical-pixel coordinates. No buttons.")]
    async fn mouse_move(&self, Parameters(args): Parameters<XYArgs>) -> Result<Json<AckOutput>, McpError> {
        ack(self.call(Request::MouseMove { x: args.x, y: args.y }).await?)
    }

    #[tool(name = "mouse_down", description = "Press a mouse button at the current cursor position.")]
    async fn mouse_down(&self, Parameters(args): Parameters<ButtonArgs>) -> Result<Json<AckOutput>, McpError> {
        ack(self.call(Request::MouseDown { button: parse_button(&args.button)? }).await?)
    }

    #[tool(name = "mouse_up", description = "Release a mouse button at the current cursor position.")]
    async fn mouse_up(&self, Parameters(args): Parameters<ButtonArgs>) -> Result<Json<AckOutput>, McpError> {
        ack(self.call(Request::MouseUp { button: parse_button(&args.button)? }).await?)
    }

    #[tool(name = "drag", description = "Click-drag from start to end with optional modifiers held throughout.")]
    async fn drag(&self, Parameters(args): Parameters<DragArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::Drag {
            start_x: args.start_x,
            start_y: args.start_y,
            end_x: args.end_x,
            end_y: args.end_y,
            button: parse_button(&args.button)?,
            modifiers: args.modifiers,
        };
        ack(self.call(req).await?)
    }

    #[tool(name = "scroll", description = "Mouse-wheel scroll at the given coordinates. direction = up|down|left|right; amount = wheel notches.")]
    async fn scroll(&self, Parameters(args): Parameters<ScrollArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::Scroll {
            x: args.x,
            y: args.y,
            direction: parse_dir(&args.direction)?,
            amount: args.amount,
            modifiers: args.modifiers,
        };
        ack(self.call(req).await?)
    }

    #[tool(name = "type", description = "Type literal Unicode text into the foreground window. Payload is wrapped in Redact<> end-to-end and never logged.")]
    async fn type_text(&self, Parameters(args): Parameters<TypeArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::Type { text: Redact::new(args.text) };
        ack(self.call(req).await?)
    }

    #[tool(name = "key", description = "Press a chord like ctrl+s, alt+f4, win+d. repeat defaults to 1.")]
    async fn key(&self, Parameters(args): Parameters<KeyArgs>) -> Result<Json<AckOutput>, McpError> {
        let _ = fastuse_proto::parse_chord(&args.chord)
            .map_err(|e| McpError::invalid_params(format!("invalid chord {:?}: {e}", args.chord), None))?;
        ack(self.call(Request::Key { chord: args.chord, repeat: args.repeat }).await?)
    }

    #[tool(name = "hold_key", description = "Press a chord, hold for duration_ms, release. Modifiers and primary key are flushed on panic / disconnect.")]
    async fn hold_key(&self, Parameters(args): Parameters<HoldKeyArgs>) -> Result<Json<AckOutput>, McpError> {
        let _ = fastuse_proto::parse_chord(&args.chord)
            .map_err(|e| McpError::invalid_params(format!("invalid chord {:?}: {e}", args.chord), None))?;
        ack(self.call(Request::HoldKey { chord: args.chord, duration_ms: args.duration_ms }).await?)
    }

    #[tool(name = "wait", description = "Server-side sleep. Returns slept_us. Does not occupy the input thread.")]
    async fn wait(&self, Parameters(args): Parameters<WaitArgs>) -> Result<Json<AckOutput>, McpError> {
        ack(self.call(Request::Wait { duration_ms: args.duration_ms }).await?)
    }

    // ---- window/monitor ----
    #[tool(name = "list_monitors", description = "Enumerate display monitors with bounds (physical pixels, virtual-desktop origin), DPI scale, and primary flag.")]
    async fn list_monitors(&self) -> Result<Json<Vec<MonitorInfo>>, McpError> {
        match self.call(Request::ListMonitors).await? {
            Response::Monitors(v) => Ok(Json(v)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "cursor_position", description = "Read the cursor position and the monitor it lies on.")]
    async fn cursor_position(&self) -> Result<Json<CursorOutput>, McpError> {
        match self.call(Request::CursorPosition).await? {
            Response::CursorPos { x, y, monitor_id } => Ok(Json(CursorOutput { x, y, monitor_id })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "foreground_window", description = "Return WindowInfo for the foreground window (HWND, title, class, process, pid, bounds).")]
    async fn foreground_window(&self) -> Result<Json<WindowInfo>, McpError> {
        match self.call(Request::ForegroundWindow).await? {
            Response::Window(w) => Ok(Json(w)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "list_windows", description = "Enumerate top-level windows with optional process_name and title_substring filters. 250ms TTL cache.")]
    async fn list_windows(&self, Parameters(args): Parameters<ListWindowsArgs>) -> Result<Json<Vec<WindowInfo>>, McpError> {
        let req = Request::ListWindows {
            process_name: args.process_name,
            title_substring: args.title_substring,
            visible_only: args.visible_only,
        };
        match self.call(req).await? {
            Response::Windows(v) => Ok(Json(v)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "focus_window", description = "Bring an HWND to the foreground using AttachThreadInput to bypass the SetForegroundWindow lockout.")]
    async fn focus_window(&self, Parameters(args): Parameters<HwndArgs>) -> Result<Json<AckOutput>, McpError> {
        ack(self.call(Request::FocusWindow { hwnd: args.hwnd }).await?)
    }

    #[tool(name = "resize_move_window", description = "Move + resize an HWND in physical-pixel virtual-desktop space. Validates that the centre lies on a known monitor.")]
    async fn resize_move_window(&self, Parameters(args): Parameters<ResizeMoveArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::ResizeMoveWindow { hwnd: args.hwnd, x: args.x, y: args.y, w: args.w, h: args.h };
        ack(self.call(req).await?)
    }

    // ---- Phase 3: capture ----
    #[tool(name = "screenshot", description = "Full-monitor screenshot. Returns base64-encoded JPEG (q=85, default) or PNG.")]
    async fn screenshot(&self, Parameters(args): Parameters<ScreenshotArgs>) -> Result<Json<ScreenshotOutput>, McpError> {
        let req = Request::Screenshot {
            monitor: args.monitor,
            format: Some(parse_image_format(args.format.as_deref())),
        };
        screenshot_response(self.call(req).await?)
    }

    #[tool(name = "screenshot_region", description = "Sub-rectangle screenshot. Reuses cached duplication object (no reacquire).")]
    async fn screenshot_region(&self, Parameters(args): Parameters<ScreenshotRegionArgs>) -> Result<Json<ScreenshotOutput>, McpError> {
        let req = Request::ScreenshotRegion {
            x: args.x,
            y: args.y,
            w: args.w,
            h: args.h,
            monitor: args.monitor,
            format: Some(parse_image_format(args.format.as_deref())),
        };
        screenshot_response(self.call(req).await?)
    }

    // ---- Phase 3: UIA ----
    #[tool(name = "uia_tree", description = "Walk the UIA subtree of an HWND (foreground if None) using a single CacheRequest pass.")]
    async fn uia_tree(&self, Parameters(args): Parameters<UiaTreeArgs>) -> Result<Json<UiaTreeOutput>, McpError> {
        let req = Request::UiaTree {
            hwnd: args.hwnd,
            depth: args.depth,
            view: Some(parse_tree_view(args.view.as_deref())),
        };
        match self.call(req).await? {
            Response::UiaTree { root, degraded } => Ok(Json(UiaTreeOutput { root, degraded })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "uia_query", description = "Selector-driven query against a UIA root (foreground if None). Selector grammar: ByName / ByAutomationId / ByControlType / ByClass + And/Or/Not.")]
    async fn uia_query(&self, Parameters(args): Parameters<UiaQueryArgs>) -> Result<Json<UiaQueryOutput>, McpError> {
        let selector = parse_selector(args.selector)?;
        let req = Request::UiaQuery {
            selector,
            root_hwnd: args.root_hwnd,
        };
        match self.call(req).await? {
            Response::UiaQuery { matches, degraded } => Ok(Json(UiaQueryOutput { matches, degraded })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "inspect_at_point", description = "Inspect the single UIA element under a screen point.")]
    async fn inspect_at_point(&self, Parameters(args): Parameters<InspectAtPointArgs>) -> Result<Json<InspectOutput>, McpError> {
        let req = Request::InspectAtPoint { x: args.x, y: args.y };
        match self.call(req).await? {
            Response::Inspect { node, degraded } => Ok(Json(InspectOutput { node, degraded })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "click_element", description = "Find an element via selector and click its centroid. Delegates to phase2 click after centroid resolution.")]
    async fn click_element(&self, Parameters(args): Parameters<ClickElementArgs>) -> Result<Json<ElementMatchOutput>, McpError> {
        let selector = parse_selector(args.selector)?;
        let req = Request::ClickElement { selector, modifiers: args.modifiers };
        element_response(self.call(req).await?)
    }

    #[tool(name = "type_into_element", description = "Find an element via selector, focus it, type text. Payload is redacted from logs.")]
    async fn type_into_element(&self, Parameters(args): Parameters<TypeIntoElementArgs>) -> Result<Json<ElementMatchOutput>, McpError> {
        let selector = parse_selector(args.selector)?;
        let req = Request::TypeIntoElement { selector, text: Redact::new(args.text) };
        element_response(self.call(req).await?)
    }

    #[tool(name = "wait_for_element", description = "Poll for an element until it appears or timeout (default 5000ms). Returns matched=true/false.")]
    async fn wait_for_element(&self, Parameters(args): Parameters<WaitForElementArgs>) -> Result<Json<ElementMatchOutput>, McpError> {
        let selector = parse_selector(args.selector)?;
        let req = Request::WaitForElement { selector, timeout_ms: args.timeout_ms };
        element_response(self.call(req).await?)
    }

    #[tool(name = "scroll_into_view", description = "Scroll the matched element into view via UIA ScrollItemPattern.")]
    async fn scroll_into_view(&self, Parameters(args): Parameters<SelectorArgs>) -> Result<Json<ElementMatchOutput>, McpError> {
        let selector = parse_selector(args.selector)?;
        let req = Request::ScrollIntoView { selector };
        element_response(self.call(req).await?)
    }
}

fn screenshot_response(res: Response) -> Result<Json<ScreenshotOutput>, McpError> {
    use base64::Engine;
    match res {
        Response::Screenshot { bytes, mime, width, height } => {
            let data_b64 = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
            Ok(Json(ScreenshotOutput { mime, width, height, data_b64 }))
        }
        Response::Error(e) => Err(Fastuse::err_from_proto(e)),
        other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
    }
}

fn element_response(res: Response) -> Result<Json<ElementMatchOutput>, McpError> {
    match res {
        Response::Element { matched } => Ok(Json(ElementMatchOutput { matched })),
        Response::Error(e) => Err(Fastuse::err_from_proto(e)),
        other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
    }
}

fn ack(res: Response) -> Result<Json<AckOutput>, McpError> {
    match res {
        Response::Ack { slept_us } => Ok(Json(AckOutput { ok: true, slept_us })),
        Response::Error(e) => Err(Fastuse::err_from_proto(e)),
        other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
    }
}

fn io_to_mcp(e: std::io::Error) -> McpError {
    McpError::internal_error(format!("io: {e}"), None)
}

impl Fastuse {
    pub fn server_info() -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some("fastuse: low-latency Windows computer-use control plane".into());
        let mut impl_ = rmcp::model::Implementation::default();
        impl_.name = "fastuse-mcp".into();
        impl_.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = impl_;
        info
    }
}
