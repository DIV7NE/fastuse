//! rmcp 1.6 ServerHandler exposing all fastuse v2 MCP tools.
//!
//! All `#[tool]` methods live on this single `Fastuse` impl because
//! `#[tool_router(server_handler)]` requires a single type as the registration
//! target.  The sub-modules in `crate::tools` (`windows`, `inspection`,
//! `meta`) are documentation hubs that cross-reference the methods here — see
//! Task 16 commit for the architecture rationale.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fastuse_proto::{
    coords::{MonitorInfo, MouseButton, ScrollDirection, WindowInfo},
    wire::{ComputerAction, ComputerRequest, WaitForWindowRequest},
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
#[derive(Serialize)]
#[serde(untagged)]
pub enum ActionOrAck {
    Ack(AckOutput),
    Element(ElementMatchOutput),
    Screenshot(ScreenshotOutput),
}

// Hand-written because the derive emits a bare `anyOf` with no root `type`,
// which rmcp rejects: MCP requires every outputSchema to have root type
// "object". Keep the union in `anyOf` and declare the root type alongside it.
impl schemars::JsonSchema for ActionOrAck {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ActionOrAck".into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "anyOf": [
                g.subschema_for::<AckOutput>(),
                g.subschema_for::<ElementMatchOutput>(),
                g.subschema_for::<ScreenshotOutput>(),
            ],
        })
    }
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct CursorOutput {
    pub x: i32,
    pub y: i32,
    pub monitor_id: u64,
}

// MCP requires every outputSchema root to be an object, so list-returning
// tools wrap their array in a named field rather than returning a bare array.
#[derive(Serialize, schemars::JsonSchema)]
pub struct MonitorsOutput {
    pub monitors: Vec<MonitorInfo>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct WindowsOutput {
    pub windows: Vec<WindowInfo>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ProcessesOutput {
    pub processes: Vec<ProcessInfoOutput>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct IdleOutput {
    pub waited_ms: u32,
    pub paint_observed: bool,
    pub focus_settled: bool,
}


// ---------- batch ----------

/// One step in a `batch`. Deliberately a small set: the actions whose value
/// comes from being sequenced without a model turn between them. Steps carry
/// no per-step ActionOpts — end the batch with a `screenshot` step instead.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BatchStep {
    Click {
        x: i32,
        y: i32,
        #[serde(default = "left_button")]
        button: String,
        #[serde(default = "one_u8")]
        count: u8,
        #[serde(default)]
        modifiers: Vec<String>,
        #[serde(default)]
        skip_set_cursor_pos: bool,
    },
    MouseMove { x: i32, y: i32 },
    Scroll {
        x: i32,
        y: i32,
        direction: String,
        amount: i32,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Type {
        text: String,
        /// Inter-character delay in milliseconds. Defaults to 30, which is
        /// safe on RichEditD2DPT-style controls; pass 0 for the bulk path.
        #[serde(default = "default_type_rate")]
        rate_ms: u32,
    },
    Key {
        chord: String,
        #[serde(default = "one_u32")]
        repeat: u32,
    },
    Wait { duration_ms: u32 },
    WaitForIdle {
        hwnd: Option<u64>,
        #[serde(default = "default_idle_timeout")]
        timeout_ms: u32,
    },
    FocusWindow { hwnd: u64 },
    Screenshot {
        monitor: Option<u32>,
        format: Option<String>,
    },
}

impl BatchStep {
    /// Short label echoed back in the per-step result.
    fn label(&self) -> &'static str {
        match self {
            BatchStep::Click { .. } => "click",
            BatchStep::MouseMove { .. } => "mouse_move",
            BatchStep::Scroll { .. } => "scroll",
            BatchStep::Type { .. } => "type",
            BatchStep::Key { .. } => "key",
            BatchStep::Wait { .. } => "wait",
            BatchStep::WaitForIdle { .. } => "wait_for_idle",
            BatchStep::FocusWindow { .. } => "focus_window",
            BatchStep::Screenshot { .. } => "screenshot",
        }
    }

    fn into_request(self) -> Result<Request, McpError> {
        Ok(match self {
            BatchStep::Click { x, y, button, count, modifiers, skip_set_cursor_pos } => {
                Request::Click {
                    x,
                    y,
                    button: parse_button(&button)?,
                    count,
                    modifiers,
                    skip_set_cursor_pos,
                    opts: None,
                }
            }
            BatchStep::MouseMove { x, y } => Request::MouseMove { x, y, opts: None },
            BatchStep::Scroll { x, y, direction, amount, modifiers } => Request::Scroll {
                x,
                y,
                direction: parse_dir(&direction)?,
                amount,
                modifiers,
                opts: None,
            },
            BatchStep::Type { text, rate_ms } => match rate_ms {
                0 => Request::Type { text: Redact::new(text), opts: None },
                rate_ms => Request::TypeRated { text: Redact::new(text), rate_ms, opts: None },
            },
            BatchStep::Key { chord, repeat } => {
                fastuse_proto::parse_chord(&chord).map_err(|e| {
                    McpError::invalid_params(format!("invalid chord {chord:?}: {e}"), None)
                })?;
                Request::Key { chord, repeat, opts: None }
            }
            BatchStep::Wait { duration_ms } => Request::Wait { duration_ms },
            BatchStep::WaitForIdle { hwnd, timeout_ms } => {
                Request::WaitForIdle { hwnd, timeout_ms }
            }
            BatchStep::FocusWindow { hwnd } => Request::FocusWindow { hwnd, opts: None },
            BatchStep::Screenshot { monitor, format } => Request::Screenshot {
                monitor,
                format: Some(parse_image_format(format.as_deref())),
            },
        })
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BatchArgs {
    /// Steps run in order on the daemon, with no model turn between them.
    pub steps: Vec<BatchStep>,
    /// Keep going after a failing step instead of stopping there.
    #[serde(default)]
    pub continue_on_error: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct BatchStepResult {
    /// Zero-based position in `steps`.
    pub index: usize,
    /// Which action this step ran.
    pub action: String,
    pub ok: bool,
    /// Present when the step failed.
    pub error: Option<String>,
    /// Present for `screenshot` steps that succeeded.
    pub screenshot: Option<ScreenshotOutput>,
    /// Present for `wait_for_idle` steps that succeeded.
    pub idle: Option<IdleOutput>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct BatchOutput {
    /// True when every step ran without error.
    pub ok: bool,
    /// Number of steps actually run (< steps.len() when stopped early).
    pub ran: usize,
    pub steps: Vec<BatchStepResult>,
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
    /// Fixed inter-character delay in milliseconds. Unset uses the bulk
    /// "as fast as possible" path, which loses characters on Modern Notepad,
    /// WinUI and other RichEditD2DPT controls; pass 30 or higher there.
    pub rate_ms: Option<u32>,
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

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WaitForWindowArgs {
    /// Substring to match against window title. At least one of title_substr
    /// or process_name must be provided.
    pub title_substr: Option<String>,
    /// Substring to match against owning process name.
    pub process_name: Option<String>,
    /// Maximum poll time in milliseconds. Defaults to 5000.
    #[serde(default = "default_wait_timeout")]
    pub timeout_ms: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WaitForIdleArgs {
    /// Target HWND. If unset, the foreground window is used.
    pub hwnd: Option<u64>,
    /// Wait budget in milliseconds. Defaults to 1000.
    #[serde(default = "default_idle_timeout")]
    pub timeout_ms: u32,
}

fn default_wait_timeout() -> u32 { 5000 }
fn default_idle_timeout() -> u32 { 1000 }

/// Inter-character delay for `batch` type steps. 30ms is the documented
/// threshold that survives the SendInput overrun race on modern controls.
fn default_type_rate() -> u32 { 30 }

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

#[derive(Serialize, schemars::JsonSchema)]
pub struct StatusOutput {
    pub running: bool,
    pub daemon_pid: Option<u32>,
    pub session_id: Option<u32>,
    pub rtt_us: Option<u64>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct StopOutput {
    pub ok: bool,
    pub was_running: bool,
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

    #[tool(name = "type", description = "Type literal Unicode text into the foreground window. Payload is wrapped in Redact<> end-to-end and never logged. Set rate_ms (30+) for Modern Notepad / WinUI / RichEditD2DPT controls, which drop characters on the default bulk path.")]
    async fn type_text(&self, Parameters(args): Parameters<TypeArgs>) -> Result<Json<ActionOrAck>, McpError> {
        let opts = build_action_opts(args.opts)?;
        let req = match args.rate_ms {
            Some(rate_ms) => Request::TypeRated { text: Redact::new(args.text), rate_ms, opts },
            None => Request::Type { text: Redact::new(args.text), opts },
        };
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
    async fn list_monitors(&self) -> Result<Json<MonitorsOutput>, McpError> {
        match self.call(Request::ListMonitors).await? {
            Response::Monitors(v) => Ok(Json(MonitorsOutput { monitors: v })),
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

    #[tool(name = "list_windows", description = "List visible top-level Windows windows. Filter by title substring or process name. Returns hwnd, bounds, title, process name.")]
    async fn list_windows(&self, Parameters(args): Parameters<ListWindowsArgs>) -> Result<Json<WindowsOutput>, McpError> {
        let req = Request::ListWindows {
            process_name: args.process_name,
            title_substring: args.title_substring,
            visible_only: args.visible_only,
        };
        match self.call(req).await? {
            Response::Windows(v) => Ok(Json(WindowsOutput { windows: v })),
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

    #[tool(name = "inspect_at", description = "Read the UIA element under the given pixel. Read-only — does not click. Returns control_type, name, automation_id, bounds. Useful for grounding before deciding where to click.")]
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
    async fn list_processes(&self, Parameters(args): Parameters<ListProcessesArgs>) -> Result<Json<ProcessesOutput>, McpError> {
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
            Response::ListProcesses(v) => Ok(Json(ProcessesOutput {
                processes: v.into_iter().map(|p| ProcessInfoOutput {
                    pid: p.pid,
                    name: p.name,
                    exe_path: p.exe_path,
                    main_hwnd: p.main_hwnd,
                }).collect(),
            })),
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

    // ---- batch ----
    #[tool(
        name = "batch",
        description = "Run several actions in order in one call, with no model turn between them. Steps: click, mouse_move, scroll, type, key, wait, wait_for_idle, focus_window, screenshot. Stops at the first failing step unless continue_on_error is true. End with a screenshot step to see the result."
    )]
    async fn batch(&self, Parameters(args): Parameters<BatchArgs>) -> Result<Json<BatchOutput>, McpError> {
        let total = args.steps.len();
        let mut results: Vec<BatchStepResult> = Vec::with_capacity(total);
        let mut all_ok = true;

        for (index, step) in args.steps.into_iter().enumerate() {
            let action = step.label().to_string();
            let mut res = BatchStepResult {
                index,
                action,
                ok: true,
                error: None,
                screenshot: None,
                idle: None,
            };

            // An unparseable step is a step failure, not a whole-call failure,
            // so earlier steps that already ran are still reported.
            let outcome = match step.into_request() {
                Ok(req) => self.call(req).await,
                Err(e) => Err(e),
            };

            match outcome {
                Ok(Response::Error(e)) => {
                    res.ok = false;
                    res.error = Some(format!("[{}] {}", e.code.as_str(), e.message));
                }
                Ok(Response::Screenshot { bytes, mime, width, height }) => {
                    res.screenshot = Some(screenshot_output(bytes, mime, width, height));
                }
                Ok(Response::Idle { waited_ms, paint_observed, focus_settled }) => {
                    res.idle = Some(IdleOutput { waited_ms, paint_observed, focus_settled });
                }
                Ok(_) => {}
                Err(e) => {
                    res.ok = false;
                    res.error = Some(e.to_string());
                }
            }

            let failed = !res.ok;
            results.push(res);
            if failed {
                all_ok = false;
                if !args.continue_on_error {
                    break;
                }
            }
        }

        Ok(Json(BatchOutput { ok: all_ok, ran: results.len(), steps: results }))
    }

    // ---- wait_for_idle ----
    #[tool(name = "wait_for_idle", description = "Block until the target window's input queue drains (foreground if hwnd is None). Use between a click and a follow-up type when the app debounces or an ImGui-style focus shift needs a frame to settle. Returns waited_ms, paint_observed, and focus_settled.")]
    async fn wait_for_idle(&self, Parameters(args): Parameters<WaitForIdleArgs>) -> Result<Json<IdleOutput>, McpError> {
        let req = Request::WaitForIdle { hwnd: args.hwnd, timeout_ms: args.timeout_ms };
        match self.call(req).await? {
            Response::Idle { waited_ms, paint_observed, focus_settled } => {
                Ok(Json(IdleOutput { waited_ms, paint_observed, focus_settled }))
            }
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- wait_for_window ----
    #[tool(name = "wait_for_window", description = "Poll for a window matching the predicate, returns when found or after timeout_ms. At least one of title_substr or process_name is required.")]
    async fn wait_for_window(&self, Parameters(args): Parameters<WaitForWindowArgs>) -> Result<Json<WindowInfo>, McpError> {
        let req = Request::WaitForWindowV2(WaitForWindowRequest {
            title_substr: args.title_substr,
            process_name: args.process_name,
            timeout_ms: args.timeout_ms,
        });
        match self.call(req).await? {
            Response::Window(w) => Ok(Json(w)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- meta: status / stop ----

    #[tool(name = "status", description = "Return daemon health: whether it is running, its PID, session ID, and last ping RTT in microseconds.")]
    async fn status(&self) -> Result<Json<StatusOutput>, McpError> {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| McpError::internal_error(format!("clock: {e}"), None))?
            .as_micros() as u64;
        let send = Instant::now();
        match self.call(Request::Ping { ts_us: now_us }).await {
            Ok(Response::Pong { daemon_pid, session_id, .. }) => Ok(Json(StatusOutput {
                running: true,
                daemon_pid: Some(daemon_pid),
                session_id: Some(session_id),
                rtt_us: Some(send.elapsed().as_micros() as u64),
            })),
            _ => Ok(Json(StatusOutput {
                running: false,
                daemon_pid: None,
                session_id: None,
                rtt_us: None,
            })),
        }
    }

    #[tool(name = "stop", description = "Gracefully shut down the fastuse daemon. Idempotent — returns ok=true even if the daemon was not running.")]
    async fn stop(&self) -> Result<Json<StopOutput>, McpError> {
        match self.call(Request::Shutdown).await {
            Ok(_) => Ok(Json(StopOutput { ok: true, was_running: true })),
            Err(_) => Ok(Json(StopOutput { ok: true, was_running: false })),
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
        // Claude sends Anthropic's internally-tagged shape
        // (`{"action": "left_click", ...}`); convert to the externally-tagged
        // shape ComputerAction expects on the wire. See `wire::reshape_anthropic_action`.
        let reshaped = fastuse_proto::wire::reshape_anthropic_action(args.raw)
            .map_err(|e| McpError::invalid_params(format!("invalid computer action: {e}"), None))?;
        let action: ComputerAction = serde_json::from_value(reshaped)
            .map_err(|e| McpError::invalid_params(format!("invalid computer action: {e}"), None))?;
        let resp = self
            .call(Request::Computer(ComputerRequest {
                action: action.clone(),
                coordinates_native: false,
            }))
            .await?;
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

/// Base64-encode a screenshot payload for the MCP edge. Shared by the
/// `screenshot` tools and `batch` screenshot steps.
fn screenshot_output(
    bytes: Redact<Vec<u8>>,
    mime: String,
    width: u32,
    height: u32,
) -> ScreenshotOutput {
    use base64::Engine;
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
    ScreenshotOutput { mime, width, height, data_b64 }
}

fn screenshot_response(res: Response) -> Result<Json<ScreenshotOutput>, McpError> {
    match res {
        Response::Screenshot { bytes, mime, width, height } => {
            Ok(Json(screenshot_output(bytes, mime, width, height)))
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

#[cfg(test)]
mod schema_tests {
    use super::*;

    /// Every tool's outputSchema must have root `"type": "object"`. rmcp
    /// enforces this at registration time by panicking, so a violation takes
    /// the whole server down at startup rather than failing one tool.
    #[test]
    fn output_schemas_are_objects() {
        let offenders: Vec<String> = Fastuse::tool_router()
            .list_all()
            .into_iter()
            .filter_map(|t| {
                let schema = t.output_schema.as_ref()?;
                match schema.get("type").and_then(|v| v.as_str()) {
                    Some("object") => None,
                    other => Some(format!("{}: type={:?}", t.name, other)),
                }
            })
            .collect();
        assert!(offenders.is_empty(), "non-object output schemas: {offenders:?}");
    }

    #[test]
    fn batch_is_registered() {
        assert!(Fastuse::tool_router().has_route("batch"));
    }

    /// A type step must take the rated path by default: the bulk path drops
    /// characters on RichEditD2DPT controls (Modern Notepad, WinUI).
    #[test]
    fn batch_type_defaults_to_rated() {
        let step: BatchStep =
            serde_json::from_value(serde_json::json!({"action": "type", "text": "hi"})).unwrap();
        match step.into_request().unwrap() {
            Request::TypeRated { rate_ms, .. } => assert_eq!(rate_ms, 30),
            other => panic!("expected TypeRated, got {other:?}"),
        }
        let bulk: BatchStep = serde_json::from_value(
            serde_json::json!({"action": "type", "text": "hi", "rate_ms": 0}),
        )
        .unwrap();
        assert!(matches!(bulk.into_request().unwrap(), Request::Type { .. }));
    }

    #[test]
    fn batch_rejects_a_bad_chord() {
        let step: BatchStep =
            serde_json::from_value(serde_json::json!({"action": "key", "chord": "ctrl+nope"}))
                .unwrap();
        assert!(step.into_request().is_err());
    }

    #[test]
    fn wait_for_idle_is_registered() {
        assert!(Fastuse::tool_router().has_route("wait_for_idle"));
    }
}
