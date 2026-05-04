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
    /// Encoded image payload. Bytes are base64 (STANDARD) at the MCP edge;
    /// over the postcard pipe they remain raw `Vec<u8>` to avoid double
    /// encoding cost.
    Screenshot {
        /// Raw encoded bytes (JPEG or PNG). MCP edge re-encodes to base64.
        bytes: Vec<u8>,
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
                bytes: vec![1, 2, 3],
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
