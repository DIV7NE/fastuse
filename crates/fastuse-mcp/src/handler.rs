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
