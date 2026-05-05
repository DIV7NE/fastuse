//! Thin client: connect-or-spawn the fastuse-daemon over named pipe, send
//! Request, await Response. Spawn logic is duplicated from fastuse-cli
//! (Phase 2 plan: lift to fastuse-core if a third caller appears).

use fastuse_proto::{decode_payload, encode_frame, Error as ProtoError, ErrorCode, Request, Response};
use rmcp::ErrorData as McpError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::Mutex;

use crate::spawn::connect_or_spawn;

/// Shared named-pipe connection to the fastuse-daemon.
///
/// Lazily connects on first [`DaemonClient::call`]; reconnects are **not**
/// attempted — the MCP server process is short-lived (one Claude Code
/// session) so reconnect logic is deferred.
pub struct DaemonClient {
    pipe_path: String,
    pipe: Mutex<Option<NamedPipeClient>>,
}

impl DaemonClient {
    /// Create a client that will connect-or-spawn the daemon the first time
    /// [`call`] is invoked.
    pub fn new(pipe_path: String) -> Self {
        Self {
            pipe_path,
            pipe: Mutex::new(None),
        }
    }

    /// Send a Request and wait for the Response.
    ///
    /// On the first call the daemon is connected (and spawned if not running).
    /// A Hello handshake is performed once before any tool request.
    pub async fn call(&self, req: Request) -> Result<Response, McpError> {
        let mut guard = self.pipe.lock().await;
        if guard.is_none() {
            let mut pipe = connect_or_spawn(&self.pipe_path)
                .await
                .map_err(io_to_mcp)?;
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

    /// Convert a [`fastuse_proto::Error`] into an MCP error.
    pub fn err_from_proto(e: ProtoError) -> McpError {
        McpError::internal_error(format!("[{}] {}", e.code.as_str(), e.message), None)
    }
}

// ---------- wire helpers ----------

async fn write_request(pipe: &mut NamedPipeClient, req: &Request) -> std::io::Result<()> {
    let bytes = encode_frame(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}")))?;
    pipe.write_all(&bytes).await?;
    pipe.flush().await?;
    Ok(())
}

async fn read_response(pipe: &mut NamedPipeClient) -> std::io::Result<Response> {
    let mut len_bytes = [0u8; 4];
    pipe.read_exact(&mut len_bytes).await?;
    let len = u32::from_le_bytes(len_bytes);
    if len > fastuse_proto::MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {len}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    pipe.read_exact(&mut buf).await?;
    decode_payload(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("decode: {e}")))
}

fn io_to_mcp(e: std::io::Error) -> McpError {
    McpError::internal_error(format!("io: {e}"), None)
}
