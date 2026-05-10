//! Length-prefixed postcard framing per D-19.
//!
//! Wire format on the named pipe: `[u32 LE length][postcard payload]`.
//! Frames are capped at 16 MiB to bound `decode_frame` memory.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Read, Write};

use crate::coords::{MonitorInfo, MouseButton, ScrollDirection, WindowInfo};
use crate::error::Error;
use crate::redact::Redact;
use crate::selector::Selector;
use crate::uia_node::{ImageFormat, TreeView, UIANode};

/// Region specifier for `ScreenshotOpts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionSpec {
    /// Capture the foreground window's client rect at the moment the
    /// post-action snapshot fires.
    Auto,
    /// Explicit rectangle in physical-pixel virtual-desktop coordinates.
    Rect {
        /// Top-left x.
        x: i32,
        /// Top-left y.
        y: i32,
        /// Width in physical pixels.
        w: u32,
        /// Height in physical pixels.
        h: u32,
    },
}

/// Post-action screenshot options bundled into `ActionOpts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotOpts {
    /// Region to capture; `None` = whole primary monitor.
    pub region: Option<RegionSpec>,
    /// Output format; `None` = JPEG (default).
    pub format: Option<ImageFormat>,
    /// JPEG quality (0..=100); ignored for PNG. `None` = 85.
    pub quality: Option<u8>,
}

/// Optional post-action perception bundle — collapses perceive→act→perceive
/// into a single tool call. Absent on legacy clients (decoded as `None`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionOpts {
    /// Non-failing wait: poll UIA after the action until this selector matches
    /// or `wait_timeout_ms` elapses.
    pub wait_for: Option<Selector>,
    /// Capture a screenshot after the action (and after `wait_for`).
    pub screenshot_after: Option<ScreenshotOpts>,
    /// Timeout shared by `wait_for` and postcondition polling.
    /// Default 2000ms when unset.
    pub wait_timeout_ms: Option<u32>,
}

/// Screenshot payload re-used by `ActionResult` and `Response::Screenshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotPayload {
    /// Encoded bytes (JPEG or PNG). MCP edge re-encodes to base64.
    pub bytes: Redact<Vec<u8>>,
    /// MIME type (`image/jpeg` or `image/png`).
    pub mime: String,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
}

/// Hard cap on a single frame payload. Per threat T-01-02, an oversized
/// length-prefix declared by an attacker MUST NOT cause unbounded allocation.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// All requests sent client → daemon.
///
/// Phase 1 ships only `Hello`, `Ping`, `Shutdown`. Later phases add tool
/// variants (Click, Type, Screenshot, etc.) by appending — NEVER renumber.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// First-connect handshake (D-22).
    Hello {
        /// Short identifier of the connecting client (e.g. "cli", "mcp").
        client_kind: String,
        /// Semver-style version of the connecting client binary.
        client_version: String,
        /// Optional override for the daemon's idle-timeout (most-permissive wins).
        requested_idle_timeout_secs: Option<u32>,
    },
    /// Liveness probe; `ts_us` is the client's wall-clock send time in micros.
    Ping {
        /// Client send timestamp (microseconds since UNIX epoch).
        ts_us: u64,
    },
    /// Cooperative shutdown (used by `fastuse-cli stop`, D-04).
    Shutdown,

    // --- Phase 2: input primitives ---
    /// Click at `(x, y)` with `button` `count` times, optionally with held
    /// modifiers around the click. `skip_set_cursor_pos` opts out of the
    /// `SetCursorPos` pre-pass used to defeat games / Electron.
    Click {
        /// Target x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Target y in physical pixels (virtual-desktop origin).
        y: i32,
        /// Mouse button to actuate.
        button: MouseButton,
        /// Number of full DOWN/UP cycles to perform.
        count: u8,
        /// Modifier chord tokens (e.g. `["ctrl", "shift"]`).
        modifiers: Vec<String>,
        /// If true, skip the implicit `SetCursorPos` before injecting clicks.
        skip_set_cursor_pos: bool,
        /// Optional post-action perception bundle (Phase 5 agent-loop wins).
        opts: Option<ActionOpts>,
    },
    /// Move the cursor to `(x, y)` (no click).
    MouseMove {
        /// Target x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Target y in physical pixels (virtual-desktop origin).
        y: i32,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Press (DOWN) the given mouse button at the current cursor position.
    MouseDown {
        /// Button to press.
        button: MouseButton,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Release (UP) the given mouse button at the current cursor position.
    MouseUp {
        /// Button to release.
        button: MouseButton,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Click-drag from `(start_x, start_y)` to `(end_x, end_y)` with `button`
    /// held. Modifiers are held for the entire drag.
    Drag {
        /// Drag start x in physical pixels.
        start_x: i32,
        /// Drag start y in physical pixels.
        start_y: i32,
        /// Drag end x in physical pixels.
        end_x: i32,
        /// Drag end y in physical pixels.
        end_y: i32,
        /// Mouse button held during the drag.
        button: MouseButton,
        /// Modifier chord tokens to hold during the drag.
        modifiers: Vec<String>,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Mouse-wheel scroll at `(x, y)` in `direction` for `amount` notches
    /// (one notch = `WHEEL_DELTA` = 120).
    Scroll {
        /// Target x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Target y in physical pixels (virtual-desktop origin).
        y: i32,
        /// Direction of scroll.
        direction: ScrollDirection,
        /// Number of wheel notches.
        amount: i32,
        /// Modifier chord tokens to hold during the scroll.
        modifiers: Vec<String>,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Type literal Unicode text via `KEYEVENTF_UNICODE`. Payload is wrapped
    /// in `Redact<T>` so logs never expose it.
    Type {
        /// Unicode payload to type, redacted from `Display`/`Debug` (D-10).
        text: Redact<String>,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Press a chord (e.g. `"ctrl+s"`, `"alt+f4"`, `"win+d"`) `repeat` times.
    Key {
        /// Chord string parsed by `fastuse_proto::chord::parse_chord`.
        chord: String,
        /// Number of full DOWN/UP cycles for the primary key.
        repeat: u32,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Press a chord and hold for `duration_ms` milliseconds before release.
    HoldKey {
        /// Chord string parsed by `fastuse_proto::chord::parse_chord`.
        chord: String,
        /// Hold duration in milliseconds.
        duration_ms: u32,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Sleep `duration_ms` server-side (does NOT occupy the input thread).
    Wait {
        /// Sleep duration in milliseconds.
        duration_ms: u32,
    },

    // --- Phase 2: window/monitor primitives ---
    /// Enumerate display monitors (cached + invalidated on `WM_DISPLAYCHANGE`).
    ListMonitors,
    /// Read the current cursor position and the monitor it lies on.
    CursorPosition,
    /// Return `WindowInfo` for the foreground window.
    ForegroundWindow,
    /// Enumerate top-level windows, filtered by optional process name and
    /// title substring.
    ListWindows {
        /// Substring filter on `process_name` (case-insensitive).
        process_name: Option<String>,
        /// Substring filter on `title` (case-insensitive).
        title_substring: Option<String>,
        /// If true (default), only `IsWindowVisible` windows are returned.
        visible_only: bool,
    },
    /// Bring the given HWND to the foreground (AttachThreadInput dance).
    FocusWindow {
        /// HWND cast to `u64`.
        hwnd: u64,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Move + resize the window in physical-pixel virtual-desktop space.
    ResizeMoveWindow {
        /// HWND cast to `u64`.
        hwnd: u64,
        /// New top-left x in physical pixels.
        x: i32,
        /// New top-left y in physical pixels.
        y: i32,
        /// New width in physical pixels.
        w: i32,
        /// New height in physical pixels.
        h: i32,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },

    // --- Phase 3: capture (CAP-01..06) ---
    /// Full-monitor screenshot. `None` monitor = primary; `None` format = JPEG.
    Screenshot {
        /// Monitor index (0 = primary, then per `ListMonitors` order).
        monitor: Option<u32>,
        /// Output image format (default JPEG q=85).
        format: Option<ImageFormat>,
    },
    /// Sub-rectangle screenshot — reuses the cached duplication object
    /// (CAP-02: NEVER reacquires for region capture).
    ScreenshotRegion {
        /// Top-left x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Top-left y in physical pixels (virtual-desktop origin).
        y: i32,
        /// Width in physical pixels.
        w: u32,
        /// Height in physical pixels.
        h: u32,
        /// Monitor index (default primary).
        monitor: Option<u32>,
        /// Output image format (default JPEG q=85).
        format: Option<ImageFormat>,
    },

    // --- Phase 3: UIA (UIA-02..08) ---
    /// Walk a UIA subtree from `hwnd`'s root (None = foreground).
    UiaTree {
        /// HWND cast to `u64` (None = foreground).
        hwnd: Option<u64>,
        /// Walk depth bound (None = unlimited within timeout).
        depth: Option<u32>,
        /// Tree walker view (default Content).
        view: Option<TreeView>,
    },
    /// Selector-driven query against a UIA root (None = foreground).
    UiaQuery {
        /// Selector expression.
        selector: Selector,
        /// Optional explicit root HWND (default foreground).
        root_hwnd: Option<u64>,
    },
    /// Single-element inspection at a screen point.
    InspectAtPoint {
        /// Probe x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Probe y in physical pixels (virtual-desktop origin).
        y: i32,
    },
    /// Scroll the matched element into view via `IUIAutomationScrollItemPattern`
    /// (or fallback `IUIAutomationScrollPattern` on the parent).
    ScrollIntoView {
        /// Selector expression.
        selector: Selector,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },

    // --- Phase 4: system surface ---
    /// Read clipboard contents (Phase 4).
    ClipboardGet(ClipboardGet),
    /// Write clipboard contents (Phase 4).
    ClipboardSet {
        /// Clipboard write payload.
        req: ClipboardSet,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Spawn a shell command and stream output (Phase 4). The wire layer
    /// returns a single `ShellExecResult` summary; streaming chunks are
    /// emitted out-of-band by the MCP server (see `ShellChunk`).
    ShellExec(ShellExec),
    /// Launch an application by query (Phase 4).
    LaunchApp {
        /// Launch request payload.
        req: LaunchApp,
        /// Optional post-action perception bundle.
        opts: Option<ActionOpts>,
    },
    /// Enumerate running processes (Phase 4).
    ListProcesses(ListProcesses),
    /// Terminate a process by PID or name (Phase 4).
    KillProcess(KillProcess),

    // --- Warmup ---
    /// Warm every cold path: D3D11 device, DXGI duplication, UIA root,
    /// foreground HWND cache, monitor enum, COM apartments. No side effects.
    Warmup,

    // --- v2: computer action dispatch (Task 9) ---
    /// Dispatch a `computer` action (Anthropic computer_20251124 schema).
    Computer(ComputerRequest),
    /// Wait for a window matching title/process to appear (v2 shape).
    WaitForWindowV2(WaitForWindowRequest),

    // --- v2.0.1: window-targeted capture ---
    /// Screenshot a specific window's client area in virtual-desktop coords.
    /// Returns the image plus the monitor offset and DPI scale so callers can
    /// translate window-local pixel coordinates back to monitor-absolute coords
    /// without an extra `list-windows`/`foreground-window` round-trip.
    ScreenshotWindow {
        /// Target window handle.
        hwnd: u64,
        /// Output image format (default JPEG q=85).
        format: Option<ImageFormat>,
    },
}

/// Clipboard format selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipFormat {
    /// CF_UNICODETEXT path.
    Text,
    /// CF_DIBV5 → PNG path.
    Image,
}

/// Read clipboard request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardGet {
    /// If `Some`, only return that format; if `None`, prefer text then image.
    pub format: Option<ClipFormat>,
}

/// Read clipboard response.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardGetResp {
    /// Clipboard is empty (or didn't have the requested format).
    None,
    /// Text payload — UTF-8.
    Text {
        /// Plain text contents (Redact-wrapped).
        text: Redact<String>,
    },
    /// Image payload — PNG bytes (encoded from CF_DIBV5 by the daemon).
    Image {
        /// MIME type, currently always `image/png`.
        mime: String,
        /// Base64-encoded PNG bytes (Redact-wrapped).
        base64: Redact<String>,
        /// Image width in pixels.
        w: u32,
        /// Image height in pixels.
        h: u32,
    },
}

impl core::fmt::Debug for ClipboardGetResp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::None => write!(f, "ClipboardGetResp::None"),
            Self::Text { text } => f
                .debug_struct("ClipboardGetResp::Text")
                .field("text", text)
                .finish(),
            Self::Image { mime, base64, w, h } => f
                .debug_struct("ClipboardGetResp::Image")
                .field("mime", mime)
                .field("base64", base64)
                .field("w", w)
                .field("h", h)
                .finish(),
        }
    }
}

/// Write clipboard request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardSet {
    /// Plain text payload.
    Text(Redact<String>),
    /// Image payload (PNG / JPEG bytes).
    Image {
        /// MIME type of the input bytes.
        mime: String,
        /// Encoded image bytes (Redact-wrapped).
        bytes: Redact<Vec<u8>>,
        /// Image width in pixels.
        w: u32,
        /// Image height in pixels.
        h: u32,
    },
}

impl core::fmt::Debug for ClipboardSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Text(t) => f.debug_tuple("ClipboardSet::Text").field(t).finish(),
            Self::Image { mime, bytes, w, h } => f
                .debug_struct("ClipboardSet::Image")
                .field("mime", mime)
                .field("bytes", bytes)
                .field("w", w)
                .field("h", h)
                .finish(),
        }
    }
}

/// Shell choice mirrored from `fastuse_core::shell::quoting::Shell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellKind {
    /// `cmd.exe`.
    Cmd,
    /// `powershell.exe`.
    Powershell,
    /// `pwsh.exe`.
    Pwsh,
    /// `bash.exe`.
    Bash,
}

/// `shell_exec` request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellExec {
    /// User-supplied command line, passed verbatim to the chosen shell.
    pub command: Redact<String>,
    /// Shell to use; default `Cmd`.
    pub shell: Option<ShellKind>,
    /// Extra environment variables.
    pub env: Option<Vec<(String, Redact<String>)>>,
    /// Working directory for the child process.
    pub cwd: Option<String>,
    /// Timeout in milliseconds; default 30_000 (30s).
    pub timeout_ms: Option<u64>,
    /// Streaming chunk size in bytes; default 4096.
    pub stream_chunk_size: Option<u32>,
}

impl core::fmt::Debug for ShellExec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ShellExec")
            .field("command", &self.command)
            .field("shell", &self.shell)
            .field("env_count", &self.env.as_ref().map(|e| e.len()).unwrap_or(0))
            .field("cwd", &self.cwd)
            .field("timeout_ms", &self.timeout_ms)
            .field("stream_chunk_size", &self.stream_chunk_size)
            .finish()
    }
}

/// One chunk of streamed shell output (delivered out-of-band by the MCP
/// streaming surface; the request/response wire returns the `Done` summary).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellChunk {
    /// stdout slice.
    Stdout(Redact<Vec<u8>>),
    /// stderr slice.
    Stderr(Redact<Vec<u8>>),
    /// Process exit summary.
    Done {
        /// Exit status; `-1` if killed by timeout / drop.
        status: i32,
        /// `true` if output was capped at the 1 MiB ring-buffer.
        truncated: bool,
    },
}

impl core::fmt::Debug for ShellChunk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Stdout(b) => f.debug_tuple("ShellChunk::Stdout").field(b).finish(),
            Self::Stderr(b) => f.debug_tuple("ShellChunk::Stderr").field(b).finish(),
            Self::Done { status, truncated } => f
                .debug_struct("ShellChunk::Done")
                .field("status", status)
                .field("truncated", truncated)
                .finish(),
        }
    }
}

/// Aggregate result returned over the request/response wire (output bytes
/// stripped — they were streamed earlier as `ShellChunk` events).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellExecResult {
    /// Captured stdout bytes (Redact-wrapped). May be empty if streamed.
    pub stdout: Redact<Vec<u8>>,
    /// Captured stderr bytes (Redact-wrapped). May be empty if streamed.
    pub stderr: Redact<Vec<u8>>,
    /// Exit status; `-1` if killed by timeout / drop.
    pub status: i32,
    /// True if output cap fired.
    pub truncated: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

impl core::fmt::Debug for ShellExecResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ShellExecResult")
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .field("status", &self.status)
            .field("truncated", &self.truncated)
            .field("duration_ms", &self.duration_ms)
            .finish()
    }
}

/// `launch_app` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchApp {
    /// User-supplied query: absolute path, .lnk display name, AUMID, or PATH binary.
    pub query: String,
}

/// `launch_app` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAppResp {
    /// Spawned process PID.
    pub pid: u32,
    /// Main window handle (if a visible top-level window appeared in 3s).
    pub hwnd: Option<isize>,
    /// Window title at the moment of detection.
    pub title: Option<String>,
    /// Window class.
    pub class: Option<String>,
}

/// `list_processes` filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcFilter {
    /// Case-insensitive substring filter on the process name.
    pub name_contains: Option<String>,
    /// If true, exclude processes that have no visible main window.
    pub visible_only: Option<bool>,
}

/// `list_processes` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListProcesses {
    /// Optional filter.
    pub filter: Option<ProcFilter>,
}

/// One entry in the process list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// Process ID.
    pub pid: u32,
    /// Executable file stem (e.g. `notepad`).
    pub name: String,
    /// Full executable path, when readable (PROCESS_QUERY_LIMITED_INFORMATION).
    pub exe_path: Option<String>,
    /// First visible top-level main window for this PID, if any.
    pub main_hwnd: Option<isize>,
}

/// Process selector for `kill_process`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessSelector {
    /// Match by PID.
    Pid(u32),
    /// Match by name (case-insensitive exact stem match).
    Name(String),
}

/// `kill_process` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillProcess {
    /// Selector.
    pub selector: ProcessSelector,
    /// Best-effort hard-kill (NtSuspendProcess + TerminateProcess).
    pub force: Option<bool>,
    /// Kill the entire process tree via Job object (Win10+).
    pub process_tree: Option<bool>,
}

/// All responses sent daemon → client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// Handshake reply (D-22).
    Welcome {
        /// Semver-style version of the daemon binary.
        daemon_version: String,
        /// Idle timeout (seconds) the daemon committed to for this session.
        current_idle_timeout_secs: u32,
    },
    /// Ping reply.
    Pong {
        /// Echo of the client's send timestamp in microseconds.
        ts_us: u64,
        /// OS-level process id of the responding daemon.
        daemon_pid: u32,
        /// WTS console session id the daemon belongs to.
        session_id: u32,
    },
    /// Structured error.
    Error(Error),

    // --- Phase 2 ---
    /// Generic acknowledgement — used by `Wait`, `Click`, `Type`, etc. when
    /// no payload is needed. `slept_us` is populated by `Wait` only.
    Ack {
        /// Microseconds actually slept (only set by `Wait`).
        slept_us: Option<u64>,
    },
    /// Result of `ListMonitors`.
    Monitors(Vec<MonitorInfo>),
    /// Result of `CursorPosition`.
    CursorPos {
        /// Cursor x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Cursor y in physical pixels (virtual-desktop origin).
        y: i32,
        /// Monitor id (HMONITOR cast to `u64`) the point lies on
        /// (`MonitorFromPoint`, `MONITOR_DEFAULTTONEAREST`).
        monitor_id: u64,
    },
    /// Result of `ForegroundWindow`.
    Window(WindowInfo),
    /// Result of `ListWindows`.
    Windows(Vec<WindowInfo>),

    // --- Phase 3: capture ---
    /// Encoded image payload. Wrapped in `Redact<Vec<u8>>` per T-03-03 so
    /// raw pixel bytes never appear in tracing/Debug; the MCP edge unwraps
    /// and base64-encodes for the JSON content block.
    Screenshot {
        /// Encoded bytes (JPEG or PNG). MCP edge re-encodes to base64.
        bytes: Redact<Vec<u8>>,
        /// MIME type (`image/jpeg` or `image/png`).
        mime: String,
        /// Width in physical pixels.
        width: u32,
        /// Height in physical pixels.
        height: u32,
    },

    // --- Phase 3: UIA ---
    /// Result of `UiaTree`.
    UiaTree {
        /// Walked subtree root with cache-fetched properties.
        root: UIANode,
        /// True if heuristic flagged the tree as degraded (UIA-10).
        degraded: bool,
    },
    /// Result of `UiaQuery`.
    UiaQuery {
        /// Matched nodes (cache-fetched).
        matches: Vec<UIANode>,
        /// True if the underlying tree was degraded.
        degraded: bool,
    },
    /// Result of `InspectAtPoint`.
    Inspect {
        /// Single element under the probe point.
        node: UIANode,
        /// True if the owning window's tree was degraded.
        degraded: bool,
    },
    /// Result of UIA element actions.
    Element {
        /// True if the selector resolved (or, for waits, found a match before
        /// timeout).
        matched: bool,
    },

    // --- Phase 4 ---
    /// `clipboard_get` reply.
    ClipboardGet(ClipboardGetResp),
    /// `clipboard_set` ack.
    ClipboardSet,
    /// `shell_exec` summary (streamed chunks emitted out-of-band).
    ShellExec(ShellExecResult),
    /// `launch_app` reply.
    LaunchApp(LaunchAppResp),
    /// `list_processes` reply.
    ListProcesses(Vec<ProcessInfo>),
    /// `kill_process` ack — number of PIDs that were terminated.
    KillProcess {
        /// Number of process handles terminated.
        terminated: u32,
    },

    /// `Warmup` reply with per-subsystem timings.
    Warmup {
        /// Time spent warming the capture path (microseconds).
        capture_us: u64,
        /// Time spent warming UIA root + foreground (microseconds).
        uia_us: u64,
        /// Time spent warming monitor enumeration (microseconds).
        monitors_us: u64,
        /// Total wall-clock (microseconds).
        total_us: u64,
    },

    // --- v2: computer action result (Task 9) ---
    /// Result of a `Computer` action dispatch.
    Computer(ComputerResult),

    // --- v2.0.1: window-targeted capture ---
    /// Result of `ScreenshotWindow`. Image bytes are the captured client-area
    /// rectangle in virtual-desktop pixel coordinates. `monitor_offset_x` /
    /// `monitor_offset_y` is the top-left of the captured rect in
    /// virtual-desktop coords — add window-local pixel offsets back to it to
    /// recover monitor-absolute click coordinates.
    ScreenshotWindow {
        /// Encoded JPEG or PNG bytes; MCP edge re-encodes to base64.
        bytes: Redact<Vec<u8>>,
        /// MIME type (`image/jpeg` or `image/png`).
        mime: String,
        /// Client-area width in physical pixels.
        client_w: u32,
        /// Client-area height in physical pixels.
        client_h: u32,
        /// Top-left x of the captured client rect in virtual-desktop coords.
        monitor_offset_x: i32,
        /// Top-left y of the captured client rect in virtual-desktop coords.
        monitor_offset_y: i32,
        /// Effective DPI scale of the target window (`GetDpiForWindow / 96`).
        dpi_scale: f32,
    },
}

/// Pipe-name pattern. The actual session_id and user_sid_short are filled in
/// by `fastuse-core::pipe_path::resolve()` (which links `windows`).
/// `fastuse-proto` keeps platform-agnostic and only exports the literal pattern.
pub fn pipe_path_pattern() -> &'static str {
    r"\\.\pipe\fastuse-{session_id}-{user_sid_short}"
}

/// Encode `value` as a length-prefixed postcard frame.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let payload = postcard::to_allocvec(value).map_err(FrameError::Serialize)?;
    if payload.len() as u64 > MAX_FRAME_BYTES as u64 {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Async-friendly variant: write a length-prefixed frame to any `Write`.
pub fn write_frame<T: Serialize, W: Write>(w: &mut W, value: &T) -> Result<(), FrameError> {
    let bytes = encode_frame(value)?;
    w.write_all(&bytes).map_err(FrameError::Io)?;
    Ok(())
}

/// Decode a postcard frame whose 4-byte length prefix has *already* been
/// stripped by the caller. Use this when you've already read the prefix off
/// the wire (e.g. to validate `MAX_FRAME_BYTES` before allocating) and you
/// don't want to reallocate + memcpy just to satisfy [`decode_frame`]'s
/// self-prefixed shape (WR-12).
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    if payload.len() as u64 > MAX_FRAME_BYTES as u64 {
        return Err(FrameError::TooLarge(payload.len()));
    }
    postcard::from_bytes(payload).map_err(FrameError::Deserialize)
}

/// Decode a single length-prefixed postcard frame from `r`.
///
/// Refuses any declared length above [`MAX_FRAME_BYTES`] without allocating.
pub fn decode_frame<T: DeserializeOwned, R: Read>(r: &mut R) -> Result<T, FrameError> {
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes).map_err(FrameError::Io)?;
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len as usize));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(FrameError::Io)?;
    postcard::from_bytes(&buf).map_err(FrameError::Deserialize)
}

/// Error returned by [`encode_frame`] / [`decode_frame`] / [`write_frame`].
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Underlying byte stream I/O failure.
    #[error("io: {0}")]
    Io(#[source] io::Error),
    /// Postcard serialization failure on the encode path.
    #[error("serialize: {0}")]
    Serialize(#[source] postcard::Error),
    /// Postcard deserialization failure on the decode path.
    #[error("deserialize: {0}")]
    Deserialize(#[source] postcard::Error),
    /// Frame payload exceeds [`MAX_FRAME_BYTES`].
    #[error("frame too large: {0} bytes (cap is {})", MAX_FRAME_BYTES)]
    TooLarge(usize),
}

// ---------------------------------------------------------------------------
// v2 — computer action enum (Task 9)
// Mirrors Anthropic computer_20251124. Coordinates are in scaled image-pixel
// space (Section 4 of the spec).
// ---------------------------------------------------------------------------

/// Anthropic `computer_20251124` action enum. Coordinates are in scaled
/// image-pixel space (per the per-session `ScaleStack` in `fastuse-win`).
///
/// **Wire encoding:** externally-tagged (`{"left_click": {...}}`) for
/// postcard compatibility. `serde(tag = "action")` would be friendlier to
/// Anthropic's `{"action": "left_click", ...}` JSON shape but postcard does
/// not support internally-tagged enums (requires `deserialize_any`). The MCP
/// boundary uses [`reshape_anthropic_action`] to convert Claude's
/// internally-tagged JSON into this externally-tagged shape before
/// deserializing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComputerAction {
    /// Capture a screenshot.
    Screenshot {
        /// Optional monitor index; defaults to the foreground monitor.
        #[serde(default)]
        monitor: Option<u32>,
    },
    /// Single primary-button click at `coordinate`.
    LeftClick {
        /// Click position in scaled image-pixel space.
        coordinate: [i32; 2],
        /// Modifier chord like `"ctrl+shift"`. Wrapped in `Redact` per D-10.
        #[serde(default)]
        text: Option<Redact<String>>,
        /// When true, apply Bezier-curve motion + timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Single secondary-button click at `coordinate`.
    RightClick {
        /// Click position in scaled image-pixel space.
        coordinate: [i32; 2],
        /// When true, apply humanized motion + timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Single middle-button click at `coordinate`.
    MiddleClick {
        /// Click position in scaled image-pixel space.
        coordinate: [i32; 2],
        /// When true, apply humanized motion + timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Two primary-button clicks at `coordinate`.
    DoubleClick {
        /// Click position in scaled image-pixel space.
        coordinate: [i32; 2],
        /// When true, apply humanized motion + timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Three primary-button clicks at `coordinate`.
    TripleClick {
        /// Click position in scaled image-pixel space.
        coordinate: [i32; 2],
        /// When true, apply humanized motion + timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Press primary button, drag from `start_coordinate` to `coordinate`,
    /// release. Atomic — covers ~95% of drag use cases.
    LeftClickDrag {
        /// Drag start in scaled image-pixel space.
        start_coordinate: [i32; 2],
        /// Drag end in scaled image-pixel space.
        coordinate: [i32; 2],
        /// When true, apply humanized motion + timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
        /// Modifier chord held during drag (e.g. `"ctrl"` for copy-drag).
        /// Wrapped in `Redact` per D-10.
        #[serde(default)]
        text: Option<Redact<String>>,
    },
    /// Press primary button without releasing. Composable for modifier drags.
    LeftMouseDown {
        /// Position to press at; defaults to current cursor when absent.
        #[serde(default)]
        coordinate: Option<[i32; 2]>,
    },
    /// Release primary button. Composable for modifier drags.
    LeftMouseUp {
        /// Position to release at; defaults to current cursor when absent.
        #[serde(default)]
        coordinate: Option<[i32; 2]>,
    },
    /// Move the cursor without clicking.
    MouseMove {
        /// Target position in scaled image-pixel space.
        coordinate: [i32; 2],
        /// When true, apply humanized Bezier motion.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Read the current cursor position. Returned in `ComputerResult.cursor`.
    CursorPosition,
    /// Type a string of literal Unicode text.
    Type {
        /// Text to type. Wrapped in `Redact` per D-10 — typed payloads are
        /// the highest-leak surface.
        text: Redact<String>,
        /// When true, apply per-keystroke timing jitter.
        #[serde(default = "yes")]
        humanize: bool,
    },
    /// Press a chord like `"ctrl+l"`, `"enter"`, `"alt+f4"`.
    Key {
        /// Chord syntax (xdotool-style). Wrapped in `Redact` per D-10.
        text: Redact<String>,
    },
    /// Press a chord and hold it for `duration` milliseconds before release.
    HoldKey {
        /// Chord syntax (xdotool-style). Wrapped in `Redact` per D-10.
        text: Redact<String>,
        /// Hold duration in milliseconds.
        duration: u32,
    },
    /// Mouse-wheel scroll at `coordinate`.
    Scroll {
        /// Scroll origin in scaled image-pixel space.
        coordinate: [i32; 2],
        /// Direction (up / down / left / right).
        scroll_direction: ScrollDir,
        /// Number of wheel ticks.
        scroll_amount: i32,
    },
    /// Sleep for `duration` milliseconds. Useful inside multi-step flows.
    Wait {
        /// Sleep duration in milliseconds.
        duration: u32,
    },
    /// Crop a region around `coordinate` and upscale to standard target
    /// dimensions. Pushes a new `ScaleSnapshot` onto the daemon's
    /// `ScaleStack`; subsequent click coords are interpreted in the zoomed
    /// frame until the next `Screenshot` resets the stack.
    Zoom {
        /// Center of the zoom region in scaled image-pixel space.
        coordinate: [i32; 2],
        /// Multiplicative zoom factor (e.g. 2.5 = 2.5× zoom).
        zoom_factor: f32,
    },
}

fn yes() -> bool { true }

/// Convert Anthropic `computer_20251124`'s internally-tagged JSON shape
/// (`{"action": "left_click", "coordinate": [..], ...}`) into the
/// externally-tagged shape that [`ComputerAction`] expects on the wire
/// (`{"left_click": {"coordinate": [..], ...}}`).
///
/// Used at the MCP boundary where Claude sends Anthropic-shape JSON. The CLI
/// constructs `ComputerAction` variants directly and does not need this.
pub fn reshape_anthropic_action(
    mut v: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "expected JSON object".to_string())?;
    let action = obj
        .remove("action")
        .ok_or_else(|| "missing 'action' field".to_string())?;
    let action_str = action
        .as_str()
        .ok_or_else(|| "'action' must be a string".to_string())?
        .to_string();
    let mut wrapped = serde_json::Map::new();
    wrapped.insert(
        action_str,
        serde_json::Value::Object(std::mem::take(obj)),
    );
    Ok(serde_json::Value::Object(wrapped))
}

/// Scroll wheel direction for `ComputerAction::Scroll`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDir {
    /// Scroll up (away from user).
    Up,
    /// Scroll down (toward user).
    Down,
    /// Scroll left.
    Left,
    /// Scroll right.
    Right,
}

/// Result of a `computer` action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerResult {
    /// True if the daemon successfully dispatched. False = error.
    pub ok: bool,
    /// Set for `Screenshot` / `Zoom` actions.
    #[serde(default)]
    pub image: Option<ImagePayload>,
    /// Set for `CursorPosition`.
    #[serde(default)]
    pub cursor: Option<[i32; 2]>,
    /// Scale snapshot the image was captured under (for `Screenshot` / `Zoom`).
    #[serde(default)]
    pub scale: Option<ScaleInfo>,
}

/// Encoded image returned by `Screenshot` / `Zoom`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImagePayload {
    /// `"jpeg"` or `"png"`.
    pub format: String,
    /// Image width in pixels (matches `ScaleInfo.scaled_w`).
    pub width: u32,
    /// Image height in pixels (matches `ScaleInfo.scaled_h`).
    pub height: u32,
    /// Base64 of the encoded image bytes (no `data:` prefix). The MCP layer
    /// wraps as `ImageContent`; CLI may emit to file when `--out` provided.
    pub data_base64: String,
}

/// Snapshot of one screenshot's scaling state. Mirrors the daemon's
/// `fastuse_win::scaling::ScaleSnapshot` over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScaleInfo {
    /// Native-to-scaled ratio (`native / scaled`).
    pub ratio: f64,
    /// Captured monitor's top-left in virtual-desktop coords.
    pub monitor_origin: [i32; 2],
    /// Native monitor width in physical pixels.
    pub native_w: u32,
    /// Native monitor height in physical pixels.
    pub native_h: u32,
    /// Scaled image width.
    pub scaled_w: u32,
    /// Scaled image height.
    pub scaled_h: u32,
}

// ---------------------------------------------------------------------------
// v2 — Windows helper request structs (Task 9)
// ---------------------------------------------------------------------------

/// Top-level `Request::Computer` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerRequest {
    /// The action to dispatch.
    pub action: ComputerAction,
    /// When `true`, coordinates in `action` are native virtual-desktop pixels;
    /// the daemon skips `ScaleStack` translation entirely. CLI sets this to
    /// `true`; MCP leaves it `false` (default) so scale-translate runs as
    /// normal.
    #[serde(default)]
    pub coordinates_native: bool,
}

/// Filter for the v2 `list_windows` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListWindowsRequest {
    /// Substring match against window title.
    pub title_substr: Option<String>,
    /// Substring match against owning process name.
    pub process_name: Option<String>,
}

/// Argument for the v2 `focus_window` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FocusWindowRequest {
    /// Target HWND.
    pub hwnd: u64,
}

/// Argument for the v2 `wait_for_window` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WaitForWindowRequest {
    /// Substring match against window title.
    pub title_substr: Option<String>,
    /// Substring match against owning process name.
    pub process_name: Option<String>,
    /// Maximum time to poll, in milliseconds.
    pub timeout_ms: u32,
}

/// Filter for the v2 `list_processes` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListProcessesRequest {
    /// Substring match against process name.
    pub name_substr: Option<String>,
    /// When true, exclude background-only processes.
    pub visible_only: bool,
}

/// Argument for the v2 `kill_process` MCP tool. Externally-tagged for
/// postcard compatibility (see `ComputerAction` doc comment).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum KillProcessRequest {
    /// Kill by process ID.
    Pid {
        /// Target PID.
        pid: u32,
    },
    /// Kill by process executable stem (no extension).
    Name {
        /// Target process stem (e.g. `"notepad"`).
        stem: String,
    },
}

/// Argument for the v2 `launch_app` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaunchAppRequest {
    /// App name, full path, or URI scheme to launch.
    pub target: String,
}

/// Argument for the v2 `shell_exec` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShellExecRequest {
    /// Command line to execute.
    pub command: String,
    /// `"powershell"`, `"cmd"`, or `None` for the default shell.
    pub shell: Option<String>,
}

/// Argument for the v2 `clipboard_set_text` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClipboardSetTextRequest {
    /// Text to write to the clipboard.
    pub text: String,
}

/// Argument for the v2 `inspect_at` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InspectAtRequest {
    /// X coordinate in native virtual-desktop pixels.
    pub x: i32,
    /// Y coordinate in native virtual-desktop pixels.
    pub y: i32,
}

/// Argument for the v2 `uia_query` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiaQueryRequest {
    /// Selector grammar from v1 (ByName / ByControlType / ByClass /
    /// ByAutomationId / And / Or / Not).
    pub selector: crate::selector::Selector,
    /// Root HWND to search under; `None` = foreground window.
    pub root_hwnd: Option<u64>,
    /// Maximum number of results to return.
    pub max_results: Option<u32>,
}

/// Argument for the v2 `uia_tree` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiaTreeRequest {
    /// Window to dump; `None` = foreground window.
    pub hwnd: Option<u64>,
    /// Maximum tree depth to walk.
    pub max_depth: Option<u32>,
}

// ---------------------------------------------------------------------------
// Unit tests — computer_action_tests (Task 9, Step 1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod computer_action_tests {
    use super::*;

    #[test]
    fn screenshot_action_round_trips() {
        // Externally-tagged on the wire (postcard requirement). The MCP layer
        // uses `reshape_anthropic_action` to convert from Claude's internally-
        // tagged shape; here we exercise the wire shape directly.
        let a = ComputerAction::Screenshot { monitor: Some(1) };
        let json = serde_json::to_string(&a).unwrap();
        let back: ComputerAction = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        assert!(json.contains("\"screenshot\""));
    }

    #[test]
    fn anthropic_shape_reshapes_to_wire_shape() {
        // Claude sends Anthropic's internally-tagged shape; the MCP boundary
        // calls reshape_anthropic_action before deserializing.
        let claude_json: serde_json::Value =
            serde_json::from_str(r#"{"action":"left_click","coordinate":[100,200]}"#).unwrap();
        let reshaped = reshape_anthropic_action(claude_json).unwrap();
        let a: ComputerAction = serde_json::from_value(reshaped).unwrap();
        match a {
            ComputerAction::LeftClick { coordinate, humanize, .. } => {
                assert_eq!(coordinate, [100, 200]);
                assert!(humanize, "humanize defaults to true");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn anthropic_shape_honors_explicit_humanize_false() {
        let claude_json: serde_json::Value =
            serde_json::from_str(r#"{"action":"left_click","coordinate":[1,2],"humanize":false}"#)
                .unwrap();
        let reshaped = reshape_anthropic_action(claude_json).unwrap();
        let a: ComputerAction = serde_json::from_value(reshaped).unwrap();
        match a {
            ComputerAction::LeftClick { humanize, .. } => assert!(!humanize),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn reshape_rejects_missing_action_field() {
        let bad: serde_json::Value =
            serde_json::from_str(r#"{"coordinate":[1,2]}"#).unwrap();
        assert!(reshape_anthropic_action(bad).is_err());
    }

    #[test]
    fn postcard_round_trip_through_wire() {
        // Critical: ComputerAction over postcard (the daemon pipe) must
        // encode and decode cleanly. This was broken when the enum was
        // internally-tagged.
        let a = ComputerAction::LeftClick {
            coordinate: [100, 200],
            text: None,
            humanize: true,
        };
        let bytes = postcard::to_allocvec(&a).unwrap();
        let back: ComputerAction = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn key_chord_is_a_string_field() {
        let a = ComputerAction::Key { text: Redact::new("ctrl+s".into()) };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"text\":\"ctrl+s\""));
    }

    #[test]
    fn scroll_direction_serializes_snake_case() {
        let a = ComputerAction::Scroll {
            coordinate: [10, 20],
            scroll_direction: ScrollDir::Down,
            scroll_amount: 3,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"scroll_direction\":\"down\""));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn ping_round_trips() {
        let req = Request::Ping { ts_us: 42 };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn hello_round_trips() {
        let req = Request::Hello {
            client_kind: "cli".into(),
            client_version: "0.1.0".into(),
            requested_idle_timeout_secs: Some(300),
        };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn pong_round_trips() {
        let res = Response::Pong {
            ts_us: 1234567,
            daemon_pid: 4242,
            session_id: 1,
        };
        let bytes = encode_frame(&res).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Response = decode_frame(&mut cur).unwrap();
        assert_eq!(res, decoded);
    }

    #[test]
    fn truncated_payload_is_err() {
        let req = Request::Ping { ts_us: 1 };
        let mut bytes = encode_frame(&req).unwrap();
        bytes.pop(); // truncate one byte from the payload
        let mut cur = Cursor::new(bytes);
        let res: Result<Request, _> = decode_frame(&mut cur);
        assert!(res.is_err());
    }

    #[test]
    fn missing_length_prefix_is_err() {
        let mut cur = Cursor::new(vec![0u8; 2]); // less than 4 bytes
        let res: Result<Request, _> = decode_frame(&mut cur);
        assert!(res.is_err());
    }

    #[test]
    fn oversize_declared_length_returns_err_no_alloc() {
        // Declare a frame larger than MAX_FRAME_BYTES; assert TooLarge before
        // any allocation is attempted.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        // NOTE: do NOT extend with the payload — the cap check must trip first.
        let mut cur = Cursor::new(bytes);
        let res: Result<Request, _> = decode_frame(&mut cur);
        match res {
            Err(FrameError::TooLarge(_)) => (),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn clipboard_set_text_round_trips() {
        let req = Request::ClipboardSet {
            req: ClipboardSet::Text(Redact::new("hello".to_string())),
            opts: None,
        };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn clipboard_set_image_round_trips() {
        let req = Request::ClipboardSet {
            req: ClipboardSet::Image {
                mime: "image/png".into(),
                bytes: Redact::new(vec![1u8, 2, 3, 4]),
                w: 4,
                h: 4,
            },
            opts: None,
        };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn shell_exec_round_trips() {
        let req = Request::ShellExec(ShellExec {
            command: Redact::new("dir".into()),
            shell: Some(ShellKind::Cmd),
            env: Some(vec![("FOO".into(), Redact::new("bar".into()))]),
            cwd: Some("C:\\".into()),
            timeout_ms: Some(5_000),
            stream_chunk_size: Some(2048),
        });
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn launch_app_round_trips() {
        let req = Request::LaunchApp {
            req: LaunchApp { query: "notepad".into() },
            opts: None,
        };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn list_processes_round_trips() {
        let req = Request::ListProcesses(ListProcesses {
            filter: Some(ProcFilter {
                name_contains: Some("explorer".into()),
                visible_only: Some(true),
            }),
        });
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn kill_process_round_trips() {
        let req = Request::KillProcess(KillProcess {
            selector: ProcessSelector::Pid(1234),
            force: Some(false),
            process_tree: Some(true),
        });
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn shell_exec_response_round_trips() {
        let res = Response::ShellExec(ShellExecResult {
            stdout: Redact::new(b"ok\n".to_vec()),
            stderr: Redact::new(vec![]),
            status: 0,
            truncated: false,
            duration_ms: 12,
        });
        let bytes = encode_frame(&res).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Response = decode_frame(&mut cur).unwrap();
        assert_eq!(res, decoded);
    }

    #[test]
    fn shell_exec_debug_does_not_leak_command() {
        let req = ShellExec {
            command: Redact::new("rm -rf SECRET-MARKER-XYZ".into()),
            shell: None,
            env: None,
            cwd: None,
            timeout_ms: None,
            stream_chunk_size: None,
        };
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("SECRET-MARKER-XYZ"), "Debug leaked command: {dbg}");
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn clipboard_set_image_debug_does_not_leak_bytes() {
        let cs = ClipboardSet::Image {
            mime: "image/png".into(),
            bytes: Redact::new(vec![0xAA, 0xBB, 0xCC, 0xDD]),
            w: 1,
            h: 1,
        };
        let dbg = format!("{cs:?}");
        assert!(!dbg.contains("AA"));
        assert!(!dbg.contains("BB"));
    }

    #[test]
    fn permission_required_error_carries_hint() {
        let err = Error::new(crate::error::ErrorCode::PermissionRequired, "shell_exec gated")
            .with_hint("restart with --allow=shell_exec");
        let bytes = encode_frame(&err).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Error = decode_frame(&mut cur).unwrap();
        assert_eq!(decoded.code, crate::error::ErrorCode::PermissionRequired);
        assert_eq!(decoded.hint.as_deref(), Some("restart with --allow=shell_exec"));
    }

    #[test]
    fn phase3_capture_requests_round_trip() {
        let r1 = Request::Screenshot {
            monitor: Some(1),
            format: Some(ImageFormat::Png),
        };
        let bytes = encode_frame(&r1).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(r1, decoded);

        let r2 = Request::ScreenshotRegion {
            x: 10,
            y: 20,
            w: 320,
            h: 240,
            monitor: None,
            format: None,
        };
        let bytes = encode_frame(&r2).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(r2, decoded);
    }

    #[test]
    fn phase3_uia_requests_round_trip() {
        use crate::selector::Selector;
        use crate::uia_node::ControlType;

        let cases = vec![
            Request::UiaTree {
                hwnd: Some(0xdead_beef),
                depth: Some(3),
                view: Some(TreeView::Raw),
            },
            Request::UiaQuery {
                selector: Selector::ByControlType(ControlType::Button),
                root_hwnd: None,
            },
            Request::InspectAtPoint { x: 100, y: 200 },
            Request::ScrollIntoView {
                selector: Selector::ByClass("ListItem".into()),
                opts: None,
            },
        ];
        for r in cases {
            let bytes = encode_frame(&r).unwrap();
            let mut cur = Cursor::new(bytes);
            let decoded: Request = decode_frame(&mut cur).unwrap();
            assert_eq!(r, decoded);
        }
    }

    #[test]
    fn phase3_responses_round_trip() {
        let nodes = vec![crate::uia_node::UIANode::empty()];
        let cases = vec![
            Response::Screenshot {
                bytes: Redact::new(vec![1, 2, 3]),
                mime: "image/jpeg".into(),
                width: 1920,
                height: 1080,
            },
            Response::UiaTree {
                root: crate::uia_node::UIANode::empty(),
                degraded: false,
            },
            Response::UiaQuery {
                matches: nodes,
                degraded: true,
            },
            Response::Inspect {
                node: crate::uia_node::UIANode::empty(),
                degraded: false,
            },
            Response::Element { matched: true },
        ];
        for r in cases {
            let bytes = encode_frame(&r).unwrap();
            let mut cur = Cursor::new(bytes);
            let decoded: Response = decode_frame(&mut cur).unwrap();
            assert_eq!(r, decoded);
        }
    }

    #[test]
    fn pipe_path_pattern_is_literal() {
        assert_eq!(
            pipe_path_pattern(),
            r"\\.\pipe\fastuse-{session_id}-{user_sid_short}"
        );
    }

    #[test]
    fn click_with_action_opts_round_trips() {
        use crate::selector::Selector;
        let req = Request::Click {
            x: 100,
            y: 200,
            button: MouseButton::Left,
            count: 1,
            modifiers: vec![],
            skip_set_cursor_pos: false,
            opts: Some(ActionOpts {
                wait_for: Some(Selector::ByName("Saved".into())),
                screenshot_after: Some(ScreenshotOpts {
                    region: Some(RegionSpec::Auto),
                    format: Some(ImageFormat::Jpeg),
                    quality: Some(85),
                }),
                wait_timeout_ms: Some(2000),
            }),
        };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn warmup_round_trips() {
        let r = Request::Warmup;
        let bytes = encode_frame(&r).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        assert_eq!(r, decode_frame::<Request, _>(&mut cur).unwrap());
    }
}
