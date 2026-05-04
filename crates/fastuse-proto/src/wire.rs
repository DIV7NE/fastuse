//! Length-prefixed postcard framing per D-19.
//!
//! Wire format on the named pipe: `[u32 LE length][postcard payload]`.
//! Frames are capped at 16 MiB to bound `decode_frame` memory.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Read, Write};

use crate::error::Error;

/// Hard cap on a single frame payload. Per threat T-01-02, an oversized
/// length-prefix declared by an attacker MUST NOT cause unbounded allocation.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// All requests sent client → daemon.
///
/// Phase 1 ships only `Hello`, `Ping`, `Shutdown`. Later phases add tool
/// variants (Click, Type, Screenshot, etc.) by appending — NEVER renumber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// All responses sent daemon → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    fn pipe_path_pattern_is_literal() {
        assert_eq!(
            pipe_path_pattern(),
            r"\\.\pipe\fastuse-{session_id}-{user_sid_short}"
        );
    }
}
