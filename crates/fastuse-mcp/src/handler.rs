//! rmcp 1.6 ServerHandler exposing the Phase 1 `ping` + Phase 2 input/window
//! tools.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fastuse_proto::{
    coords::{MonitorInfo, MouseButton, ScrollDirection, WindowInfo},
    wire::{ComputerAction, ComputerRequest},
    Redact, Request, Response,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_router, ErrorData as McpError};

use crate::tools::computer as computer_tool;
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

/// Untagged union returned by every action tool.
///
/// - `Ack`     — action completed, no post-action opts triggered.
/// - `Element` — element-targeted action (matched=true/false).
/// - `Screenshot` — action completed with screenshot_after (v2 transition).
#[derive(Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ActionOrAck {
    Ack(AckOutput),
    Element(ElementMatchOutput),
    Screenshot(ScreenshotOutput),
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct CursorOutput {
    pub x: i32,
    pub y: i32,
    pub monitor_id: u64,
}

// ---------- shared action opts input fragment ----------

/// Optional post-action perception arguments that can be flattened into any
/// action input schema via `#[serde(flatten, default)]`.
#[derive(Default, Deserialize, schemars::JsonSchema)]
pub struct ActionOptsArgs {
    /// UIA selector JSON to poll after the action until it matches.
    /// Default timeout 2000ms; override via `wait_timeout_ms`.
    pub wait_for: Option<serde_json::Value>,
    /// Capture a screenshot after the action (and after `wait_for` if set).
    /// Pass `true` for full-primary-monitor JPEG. Pass `"png"` or
    /// `{ "region": "auto", "format": "png" }` for more control.
    pub screenshot_after: Option<serde_json::Value>,
    /// Timeout shared by `wait_for` in milliseconds.
    pub wait_timeout_ms: Option<u32>,
}

fn build_action_opts(a: ActionOptsArgs) -> Result<Option<fastuse_proto::ActionOpts>, McpError> {
    use fastuse_proto::{ActionOpts, RegionSpec, ScreenshotOpts, ImageFormat};

    // If no opts fields are set at all, propagate None so legacy dispatch
    // returns Response::Ack / Response::Element unchanged.
    if a.wait_for.is_none()
        && a.screenshot_after.is_none()
        && a.wait_timeout_ms.is_none()
    {
        return Ok(None);
    }

    let wait_for = a.wait_for.map(parse_selector).transpose()?;

    // `screenshot_after` accepts three shapes:
    //  - `true`           → full primary monitor JPEG (most common agent use case)
    //  - `"png"` / `"jpeg"` → full monitor with explicit format
    //  - `{ "region": "auto"|{x,y,w,h}, "format": "jpeg"|"png", "quality": N }`
    let screenshot_after = match a.screenshot_after {
        None => None,
        Some(v) => {
            let opts = if v == serde_json::Value::Bool(true) {
                ScreenshotOpts { region: None, format: Some(ImageFormat::Jpeg), quality: None }
            } else if let Some(fmt) = v.as_str() {
                let format = Some(parse_image_format(Some(fmt)));
                ScreenshotOpts { region: None, format, quality: None }
            } else if v.is_object() {
                #[derive(Deserialize)]
                struct SSOpts {
                    region: Option<serde_json::Value>,
                    format: Option<String>,
                    quality: Option<u8>,
                }
                let so: SSOpts = serde_json::from_value(v)
                    .map_err(|e| McpError::invalid_params(format!("screenshot_after: {e}"), None))?;
                let region = match so.region {
                    None => None,
                    Some(rv) if rv.as_str().map_or(false, |s| s == "auto") => Some(RegionSpec::Auto),
                    Some(rv) => {
                        #[derive(Deserialize)]
                        struct R { x: i32, y: i32, w: u32, h: u32 }
                        let r: R = serde_json::from_value(rv)
                            .map_err(|e| McpError::invalid_params(format!("screenshot_after.region: {e}"), None))?;
                        Some(RegionSpec::Rect { x: r.x, y: r.y, w: r.w, h: r.h })
                    }
                };
                let format = Some(parse_image_format(so.format.as_deref()));
                ScreenshotOpts { region, format, quality: so.quality }
            } else {
                return Err(McpError::invalid_params(
                    "screenshot_after: expected true, \"jpeg\", \"png\", or an object".to_string(),
                    None,
                ));
            };
            Some(opts)
        }
    };

    Ok(Some(ActionOpts {
        wait_for,
        screenshot_after,
        wait_timeout_ms: a.wait_timeout_ms,
    }))
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
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

fn left_button() -> String { "left".to_string() }
fn one_u8() -> u8 { 1 }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct XYArgs {
    pub x: i32,
    pub y: i32,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ButtonArgs {
    #[serde(default = "left_button")]
    pub button: String,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
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
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ScrollArgs {
    pub x: i32,
    pub y: i32,
    pub direction: String,
    pub amount: i32,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TypeArgs {
    pub text: String,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct KeyArgs {
    pub chord: String,
    #[serde(default = "one_u32")]
    pub repeat: u32,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

fn one_u32() -> u32 { 1 }

#[derive(Deserialize, schemars::JsonSchema)]
pub struct HoldKeyArgs {
    pub chord: String,
    pub duration_ms: u32,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
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
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ResizeMoveArgs {
    pub hwnd: u64,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
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
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
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

// ---------- computer_20251124 input schema ----------

/// Raw input for the `computer` MCP tool.
///
/// The entire JSON object is forwarded to `serde_json::from_value::<ComputerAction>()`
/// which uses the internal `action` tag to discriminate variants.  The
/// `JsonSchema` impl returns the full `computer_20251124`-compatible schema.
#[derive(Deserialize)]
pub struct ComputerArgs {
    #[serde(flatten)]
    pub raw: serde_json::Value,
}

impl schemars::JsonSchema for ComputerArgs {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ComputerArgs".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "screenshot", "left_click", "right_click", "middle_click",
                        "double_click", "triple_click", "left_click_drag",
                        "left_mouse_down", "left_mouse_up", "mouse_move",
                        "cursor_position", "type", "key", "hold_key", "scroll",
                        "wait", "zoom"
                    ],
                    "description": "The desktop action to perform."
                },
                "coordinate": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "minItems": 2,
                    "maxItems": 2,
                    "description": "Target [x, y] in scaled image-pixel space."
                },
                "start_coordinate": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "minItems": 2,
                    "maxItems": 2,
                    "description": "Drag start [x, y] for left_click_drag."
                },
                "text": {
                    "type": "string",
                    "description": "Text for type/key/hold_key, or modifier chord for click actions."
                },
                "duration": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Hold/wait duration in milliseconds."
                },
                "scroll_direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"]
                },
                "scroll_amount": {
                    "type": "integer",
                    "description": "Number of wheel ticks."
                },
                "humanize": {
                    "type": "boolean",
                    "description": "Apply human-like motion and timing jitter (default true)."
                },
                "monitor": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Monitor index for screenshot (default: foreground monitor)."
                },
                "zoom_factor": {
                    "type": "number",
                    "minimum": 1.0,
                    "description": "Multiplicative zoom factor for zoom action (e.g. 2.5)."
                }
            },
            "required": ["action"]
        })
    }
}

// ---------- Phase 4 input schemas ----------

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardGetTextArgs {}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardSetTextArgs {
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ShellExecArgs {
    pub command: String,
    /// "cmd" (default) | "powershell" | "pwsh" | "bash"
    pub shell: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Timeout in milliseconds (default 30000).
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct LaunchAppArgs {
    pub query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListProcessesArgs {
    pub name_contains: Option<String>,
    pub visible_only: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct KillProcessArgs {
    /// Either "pid:1234" or "name:notepad".
    pub selector: String,
    pub force: Option<bool>,
    pub process_tree: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardSetImageArgs {
    /// MIME type: "image/png" or "image/jpeg".
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded image bytes (PNG or JPEG).
    pub data_b64: String,
}

// ---------- Phase 4 output schemas ----------

#[derive(Serialize, schemars::JsonSchema)]
pub struct ClipboardTextOutput {
    pub present: bool,
    pub text: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ShellExecOutput {
    pub status: i32,
    pub truncated: bool,
    pub duration_ms: u64,
    pub stdout_b64: String,
    pub stderr_b64: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct LaunchAppOutput {
    pub pid: u32,
    pub hwnd: Option<isize>,
    pub title: Option<String>,
    pub class: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct KillProcessOutput {
    pub terminated: u32,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct WarmupOutput {
    pub capture_us: u64,
    pub uia_us: u64,
    pub monitors_us: u64,
    pub total_us: u64,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ProcessInfoOutput {
    pub pid: u32,
    pub name: String,
    pub exe_path: Option<String>,
    pub main_hwnd: Option<isize>,
}

fn parse_shell_kind(s: Option<&str>) -> Option<fastuse_proto::ShellKind> {
    use fastuse_proto::ShellKind;
    Some(match s.map(str::to_lowercase).as_deref() {
        Some("powershell") => ShellKind::Powershell,
        Some("pwsh") => ShellKind::Pwsh,
        Some("bash") => ShellKind::Bash,
        Some("cmd") | None => ShellKind::Cmd,
        _ => return None,
    })
}

fn parse_proc_selector(s: &str) -> Result<fastuse_proto::ProcessSelector, McpError> {
    use fastuse_proto::ProcessSelector;
    if let Some(pid) = s.strip_prefix("pid:") {
        let pid: u32 = pid.parse().map_err(|e| {
            McpError::invalid_params(format!("pid: {e}"), None)
        })?;
        Ok(ProcessSelector::Pid(pid))
    } else if let Some(name) = s.strip_prefix("name:") {
        Ok(ProcessSelector::Name(name.to_string()))
    } else {
        Err(McpError::invalid_params(
            "selector must be 'pid:<n>' or 'name:<stem>'".to_string(),
            None,
        ))
    }
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
    async fn click(&self, Parameters(args): Parameters<ClickArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        let req = Request::Click {
            x: args.x,
            y: args.y,
            button: parse_button(&args.button)?,
            count: args.count,
            modifiers: args.modifiers,
            skip_set_cursor_pos: args.skip_set_cursor_pos,
            opts,
        };
        action_or_ack_response(self.call(req).await?)
    }

    #[tool(name = "mouse_move", description = "Move the system cursor to physical-pixel coordinates. No buttons.")]
    async fn mouse_move(&self, Parameters(args): Parameters<XYArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        action_or_ack_response(self.call(Request::MouseMove { x: args.x, y: args.y, opts }).await?)
    }

    #[tool(name = "mouse_down", description = "Press a mouse button at the current cursor position.")]
    async fn mouse_down(&self, Parameters(args): Parameters<ButtonArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        action_or_ack_response(self.call(Request::MouseDown { button: parse_button(&args.button)?, opts }).await?)
    }

    #[tool(name = "mouse_up", description = "Release a mouse button at the current cursor position.")]
    async fn mouse_up(&self, Parameters(args): Parameters<ButtonArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        action_or_ack_response(self.call(Request::MouseUp { button: parse_button(&args.button)?, opts }).await?)
    }

    #[tool(name = "drag", description = "Click-drag from start to end with optional modifiers held throughout.")]
    async fn drag(&self, Parameters(args): Parameters<DragArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        let req = Request::Drag {
            start_x: args.start_x,
            start_y: args.start_y,
            end_x: args.end_x,
            end_y: args.end_y,
            button: parse_button(&args.button)?,
            modifiers: args.modifiers,
            opts,
        };
        action_or_ack_response(self.call(req).await?)
    }

    #[tool(name = "scroll", description = "Mouse-wheel scroll at the given coordinates. direction = up|down|left|right; amount = wheel notches.")]
    async fn scroll(&self, Parameters(args): Parameters<ScrollArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        let req = Request::Scroll {
            x: args.x,
            y: args.y,
            direction: parse_dir(&args.direction)?,
            amount: args.amount,
            modifiers: args.modifiers,
            opts,
        };
        action_or_ack_response(self.call(req).await?)
    }

    #[tool(name = "type", description = "Type literal Unicode text into the foreground window. Payload is wrapped in Redact<> end-to-end and never logged.")]
    async fn type_text(&self, Parameters(args): Parameters<TypeArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        let req = Request::Type { text: Redact::new(args.text), opts };
        action_or_ack_response(self.call(req).await?)
    }

    #[tool(name = "key", description = "Press a chord like ctrl+s, alt+f4, win+d. repeat defaults to 1.")]
    async fn key(&self, Parameters(args): Parameters<KeyArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let _ = fastuse_proto::parse_chord(&args.chord)
            .map_err(|e| McpError::invalid_params(format!("invalid chord {:?}: {e}", args.chord), None))?;
        let opts = build_action_opts(args.opts)?;
        action_or_ack_response(self.call(Request::Key { chord: args.chord, repeat: args.repeat, opts }).await?)
    }

    #[tool(name = "hold_key", description = "Press a chord, hold for duration_ms, release. Modifiers and primary key are flushed on panic / disconnect.")]
    async fn hold_key(&self, Parameters(args): Parameters<HoldKeyArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let _ = fastuse_proto::parse_chord(&args.chord)
            .map_err(|e| McpError::invalid_params(format!("invalid chord {:?}: {e}", args.chord), None))?;
        let opts = build_action_opts(args.opts)?;
        action_or_ack_response(self.call(Request::HoldKey { chord: args.chord, duration_ms: args.duration_ms, opts }).await?)
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
    async fn focus_window(&self, Parameters(args): Parameters<HwndArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        action_or_ack_response(self.call(Request::FocusWindow { hwnd: args.hwnd, opts }).await?)
    }

    #[tool(name = "resize_move_window", description = "Move + resize an HWND in physical-pixel virtual-desktop space. Validates that the centre lies on a known monitor.")]
    async fn resize_move_window(&self, Parameters(args): Parameters<ResizeMoveArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        let req = Request::ResizeMoveWindow { hwnd: args.hwnd, x: args.x, y: args.y, w: args.w, h: args.h, opts };
        action_or_ack_response(self.call(req).await?)
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

    #[tool(name = "scroll_into_view", description = "Scroll the matched element into view via UIA ScrollItemPattern.")]
    async fn scroll_into_view(&self, Parameters(args): Parameters<SelectorArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let selector = parse_selector(args.selector)?;
        let opts = build_action_opts(args.opts)?;
        let req = Request::ScrollIntoView { selector, opts };
        action_or_ack_response(self.call(req).await?)
    }

    // ---- Phase 4: clipboard ----
    #[tool(name = "clipboard_get_text", description = "Read text from the clipboard. Returns present=false if empty or non-text.")]
    async fn clipboard_get_text(&self, Parameters(_): Parameters<ClipboardGetTextArgs>) -> Result<Json<ClipboardTextOutput>, McpError> {
        let req = Request::ClipboardGet(fastuse_proto::ClipboardGet {
            format: Some(fastuse_proto::ClipFormat::Text),
        });
        match self.call(req).await? {
            Response::ClipboardGet(fastuse_proto::ClipboardGetResp::Text { text }) => {
                Ok(Json(ClipboardTextOutput { present: true, text: Some(text.into_inner()) }))
            }
            Response::ClipboardGet(_) => Ok(Json(ClipboardTextOutput { present: false, text: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "clipboard_set_text", description = "Write text to the clipboard. Payload is wrapped in Redact<> end-to-end.")]
    async fn clipboard_set_text(&self, Parameters(args): Parameters<ClipboardSetTextArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::ClipboardSet { req: fastuse_proto::ClipboardSet::Text(Redact::new(args.text)), opts: None };
        match self.call(req).await? {
            Response::ClipboardSet => Ok(Json(AckOutput { ok: true, slept_us: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "clipboard_get_image", description = "Read an image from the clipboard. Decodes CF_DIBV5 and returns base64-encoded PNG. Returns an error if no image is present.")]
    async fn clipboard_get_image(&self) -> Result<Json<ScreenshotOutput>, McpError> {
        let req = Request::ClipboardGet(fastuse_proto::ClipboardGet {
            format: Some(fastuse_proto::ClipFormat::Image),
        });
        match self.call(req).await? {
            Response::ClipboardGet(fastuse_proto::ClipboardGetResp::Image { mime, base64, w, h }) => {
                Ok(Json(ScreenshotOutput {
                    mime,
                    width: w,
                    height: h,
                    data_b64: base64.into_inner(),
                }))
            }
            Response::ClipboardGet(_) => {
                Err(McpError::internal_error("no image on clipboard".to_string(), None))
            }
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "clipboard_set_image", description = "Write a base64-encoded PNG or JPEG image to the clipboard as CF_DIBV5.")]
    async fn clipboard_set_image(&self, Parameters(args): Parameters<ClipboardSetImageArgs>) -> Result<Json<AckOutput>, McpError> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&args.data_b64)
            .map_err(|e| McpError::invalid_params(format!("data_b64: {e}"), None))?;
        let req = Request::ClipboardSet {
            req: fastuse_proto::ClipboardSet::Image {
                mime: args.mime,
                bytes: Redact::new(bytes),
                w: args.width,
                h: args.height,
            },
            opts: None,
        };
        match self.call(req).await? {
            Response::ClipboardSet => Ok(Json(AckOutput { ok: true, slept_us: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- Phase 4: shell ----
    #[tool(name = "shell_exec", description = "Run a shell command. Permission-gated (Confirmed tier). stdout/stderr returned base64.")]
    async fn shell_exec(&self, Parameters(args): Parameters<ShellExecArgs>) -> Result<Json<ShellExecOutput>, McpError> {
        let shell = parse_shell_kind(args.shell.as_deref())
            .ok_or_else(|| McpError::invalid_params("unknown shell".to_string(), None))?;
        let req = Request::ShellExec(fastuse_proto::ShellExec {
            command: Redact::new(args.command),
            shell: Some(shell),
            env: None,
            cwd: args.cwd,
            timeout_ms: args.timeout_ms,
            stream_chunk_size: None,
        });
        match self.call(req).await? {
            Response::ShellExec(r) => {
                use base64::Engine;
                Ok(Json(ShellExecOutput {
                    status: r.status,
                    truncated: r.truncated,
                    duration_ms: r.duration_ms,
                    stdout_b64: base64::engine::general_purpose::STANDARD.encode(r.stdout.into_inner()),
                    stderr_b64: base64::engine::general_purpose::STANDARD.encode(r.stderr.into_inner()),
                }))
            }
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- Phase 4: launch / process ----
    #[tool(name = "launch_app", description = "Launch an application by PATH binary, absolute path, or known URI scheme. Returns spawned PID and main HWND if visible within 3s.")]
    async fn launch_app(&self, Parameters(args): Parameters<LaunchAppArgs>) -> Result<Json<LaunchAppOutput>, McpError> {
        let req = Request::LaunchApp { req: fastuse_proto::LaunchApp { query: args.query }, opts: None };
        match self.call(req).await? {
            Response::LaunchApp(r) => Ok(Json(LaunchAppOutput {
                pid: r.pid,
                hwnd: r.hwnd,
                title: r.title,
                class: r.class,
            })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "list_processes", description = "Enumerate running processes with optional name substring filter and visible-window filter.")]
    async fn list_processes(&self, Parameters(args): Parameters<ListProcessesArgs>) -> Result<Json<Vec<ProcessInfoOutput>>, McpError> {
        let filter = if args.name_contains.is_some() || args.visible_only.is_some() {
            Some(fastuse_proto::ProcFilter {
                name_contains: args.name_contains,
                visible_only: args.visible_only,
            })
        } else {
            None
        };
        let req = Request::ListProcesses(fastuse_proto::ListProcesses { filter });
        match self.call(req).await? {
            Response::ListProcesses(v) => Ok(Json(v.into_iter().map(|p| ProcessInfoOutput {
                pid: p.pid,
                name: p.name,
                exe_path: p.exe_path,
                main_hwnd: p.main_hwnd,
            }).collect())),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "kill_process", description = "Terminate a process by 'pid:<n>' or 'name:<stem>'. Permission-gated (Confirmed tier).")]
    async fn kill_process(&self, Parameters(args): Parameters<KillProcessArgs>) -> Result<Json<KillProcessOutput>, McpError> {
        let selector = parse_proc_selector(&args.selector)?;
        let req = Request::KillProcess(fastuse_proto::KillProcess {
            selector,
            force: args.force,
            process_tree: args.process_tree,
        });
        match self.call(req).await? {
            Response::KillProcess { terminated } => Ok(Json(KillProcessOutput { terminated })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- Warmup ----
    #[tool(name = "warmup", description = "Warm every cold path (D3D11, DXGI, UIA root, monitors). Idempotent. Daemon also auto-warms on spawn. Returns per-subsystem timings in microseconds.")]
    async fn warmup(&self) -> Result<Json<WarmupOutput>, McpError> {
        match self.call(Request::Warmup).await? {
            Response::Warmup { capture_us, uia_us, monitors_us, total_us } => {
                Ok(Json(WarmupOutput { capture_us, uia_us, monitors_us, total_us }))
            }
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- Vision-first computer tool (Task 15) ----

    /// Drive the Windows desktop. Vision-first: take screenshots, then click
    /// coordinates returned by analyzing the image. Matches Anthropic
    /// `computer_20251124` schema.
    ///
    /// Returns `ImageContent` inline for `screenshot` and `zoom` actions so
    /// Claude sees the captured frame in the same turn. All other actions
    /// return `TextContent` with a JSON summary (ok, action, cursor, scale).
    #[tool(
        name = "computer",
        description = "Drive the Windows desktop. Vision-first: take screenshots, then click coordinates returned by analyzing the image. Matches Anthropic computer_20251124 schema."
    )]
    async fn computer(&self, Parameters(args): Parameters<ComputerArgs>) -> Result<CallToolResult, McpError> {
        let action: ComputerAction = serde_json::from_value(args.raw)
            .map_err(|e| McpError::invalid_params(format!("invalid computer action: {e}"), None))?;
        let resp = self.call(Request::Computer(ComputerRequest { action: action.clone() })).await?;
        match resp {
            Response::Computer(result) => Ok(computer_tool::format_result(&action, result)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
}

fn action_or_ack_response(res: Response) -> Result<Json<ActionOrAck>, McpError> {
    use base64::Engine;
    match res {
        Response::Ack { slept_us } => Ok(Json(ActionOrAck::Ack(AckOutput { ok: true, slept_us }))),
        Response::Element { matched } => Ok(Json(ActionOrAck::Element(ElementMatchOutput { matched }))),
        // screenshot_after now returns a Screenshot response (v2 transition).
        Response::Screenshot { bytes, mime, width, height } => {
            let data_b64 = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
            Ok(Json(ActionOrAck::Screenshot(ScreenshotOutput { mime, width, height, data_b64 })))
        }
        Response::Error(e) => Err(Fastuse::err_from_proto(e)),
        other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
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
