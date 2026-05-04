//! fastuse-proto: wire types only. No tokio, no windows, no uiautomation deps.
//!
//! Both clients (`fastuse-cli`, `fastuse-mcp`) and the daemon link this crate.
//! Per D-11 / D-19 / D-20, this is the canonical home of:
//!   - `Request` / `Response` enums on the named-pipe wire
//!   - `Error` / `ErrorCode` shared error vocabulary
//!   - `Redact<T>` payload-leak-prevention newtype
//!   - Length-prefixed postcard framing helpers
//!
//! Per D-21, this crate exports only the pipe path *pattern* (a constant
//! string with placeholders). Actual session-ID + SID resolution lives in
//! `fastuse-core` (the only crate outside `fastuse-win` allowed a narrow
//! `windows` dep).

#![deny(missing_docs)]
#![deny(unsafe_code)]

pub mod chord;
pub mod coords;
pub mod error;
pub mod redact;
pub mod selector;
pub mod uia_node;
pub mod wire;

pub use chord::{parse_chord, Chord, ChordError, ChordKey, ModKey};
pub use coords::{MonitorInfo, MouseButton, Rect, ScrollDirection, WindowInfo};
pub use error::{Error, ErrorCode};
pub use redact::{Redact, RedactLen};
pub use selector::Selector;
pub use uia_node::{ControlType, ImageFormat, TreeView, UIANode};
pub use wire::{
    decode_frame, decode_payload, encode_frame, pipe_path_pattern, write_frame, ClipFormat,
    ClipboardGet, ClipboardGetResp, ClipboardSet, FrameError, KillProcess, LaunchApp,
    LaunchAppResp, ListProcesses, ProcFilter, ProcessInfo, ProcessSelector, Request, Response,
    ShellChunk, ShellExec, ShellExecResult, ShellKind, MAX_FRAME_BYTES,
};
