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
    },
    /// Move the cursor to `(x, y)` (no click).
    MouseMove {
        /// Target x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Target y in physical pixels (virtual-desktop origin).
        y: i32,
    },
    /// Press (DOWN) the given mouse button at the current cursor position.
    MouseDown {
        /// Button to press.
        button: MouseButton,
    },
    /// Release (UP) the given mouse button at the current cursor position.
    MouseUp {
        /// Button to release.
        button: MouseButton,
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
    },
    /// Type literal Unicode text via `KEYEVENTF_UNICODE`. Payload is wrapped
    /// in `Redact<T>` so logs never expose it.
    Type {
        /// Unicode payload to type, redacted from `Display`/`Debug` (D-10).
        text: Redact<String>,
    },
    /// Press a chord (e.g. `"ctrl+s"`, `"alt+f4"`, `"win+d"`) `repeat` times.
    Key {
        /// Chord string parsed by `fastuse_proto::chord::parse_chord`.
        chord: String,
        /// Number of full DOWN/UP cycles for the primary key.
        repeat: u32,
    },
    /// Press a chord and hold for `duration_ms` milliseconds before release.
    HoldKey {
        /// Chord string parsed by `fastuse_proto::chord::parse_chord`.
        chord: String,
        /// Hold duration in milliseconds.
        duration_ms: u32,
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
    /// Find an element via selector and click its centroid (delegates to
    /// Phase 2 `input::click` after centroid resolution).
    ClickElement {
        /// Selector expression.
        selector: Selector,
        /// Modifier chord tokens to hold during the click.
        modifiers: Option<Vec<String>>,
    },
    /// Find an element via selector, focus it, and type into it
    /// (delegates to Phase 2 `input::type_text` after `SetFocus`).
    TypeIntoElement {
        /// Selector expression.
        selector: Selector,
        /// Unicode payload to type, redacted from logs (T-03-01).
        text: Redact<String>,
    },
    /// Poll for an element until it appears or timeout.
    WaitForElement {
        /// Selector expression.
        selector: Selector,
        /// Timeout in milliseconds (default 5000 server-side).
        timeout_ms: u32,
    },
    /// Scroll the matched element into view via `IUIAutomationScrollItemPattern`
    /// (or fallback `IUIAutomationScrollPattern` on the parent).
    ScrollIntoView {
        /// Selector expression.
        selector: Selector,
    },

    // --- Phase 4: system surface ---
    /// Read clipboard contents (Phase 4).
    ClipboardGet(ClipboardGet),
    /// Write clipboard contents (Phase 4).
    ClipboardSet(ClipboardSet),
    /// Spawn a shell command and stream output (Phase 4). The wire layer
    /// returns a single `ShellExecResult` summary; streaming chunks are
    /// emitted out-of-band by the MCP server (see `ShellChunk`).
    ShellExec(ShellExec),
    /// Launch an application by query (Phase 4).
    LaunchApp(LaunchApp),
    /// Enumerate running processes (Phase 4).
    ListProcesses(ListProcesses),
    /// Terminate a process by PID or name (Phase 4).
    KillProcess(KillProcess),
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
    /// Result of `WaitForElement` and the boolean half of element actions.
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
        let req = Request::ClipboardSet(ClipboardSet::Text(Redact::new("hello".to_string())));
        let bytes = encode_frame(&req).unwrap();
        let mut cur = Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn clipboard_set_image_round_trips() {
        let req = Request::ClipboardSet(ClipboardSet::Image {
            mime: "image/png".into(),
            bytes: Redact::new(vec![1u8, 2, 3, 4]),
            w: 4,
            h: 4,
        });
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
        let req = Request::LaunchApp(LaunchApp {
            query: "notepad".into(),
        });
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
            Request::ClickElement {
                selector: Selector::ByName("OK".into()),
                modifiers: Some(vec!["ctrl".into()]),
            },
            Request::TypeIntoElement {
                selector: Selector::ByAutomationId("editor".into()),
                text: Redact::new("hello".to_string()),
            },
            Request::WaitForElement {
                selector: Selector::ByName("Loaded".into()),
                timeout_ms: 1000,
            },
            Request::ScrollIntoView {
                selector: Selector::ByClass("ListItem".into()),
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
}
