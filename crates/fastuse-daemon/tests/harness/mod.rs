//! Shared test harness for Phase 2 integration tests.
//!
//! Connects to the running fastuse daemon via the named pipe. If the daemon
//! is not running, the helper spawns the release binary in the background
//! (best-effort). All public functions are `pub(super)` to silence
//! per-test-file dead-code warnings — each integration test is a separate
//! compilation unit and only consumes a subset of the helpers.

#![allow(dead_code)]

use std::time::Duration;

use fastuse_core::pipe_path_resolve;
use fastuse_proto::{decode_payload, encode_frame, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

pub struct TestClient {
    pipe: NamedPipeClient,
}

impl TestClient {
    pub async fn call(&mut self, req: Request) -> std::io::Result<Response> {
        write_request(&mut self.pipe, &req).await?;
        read_response(&mut self.pipe).await
    }
}

/// Connect to the running daemon, spawning it if necessary. Skips the test
/// (returns `None` semantics — actually `panic!` is wrong; we use
/// `std::process::exit(0)` is also wrong for cargo test. We just panic with
/// a clear "skipped" message — Cargo will surface it via the test name).
pub async fn connect_or_skip() -> TestClient {
    let identity = pipe_path_resolve().expect("resolve pipe path");

    // Try connect first; if it fails, spawn the daemon and retry.
    match try_connect(&identity.path).await {
        Ok(pipe) => return handshake(pipe).await,
        Err(_) => {
            spawn_daemon();
            // Backoff loop.
            let mut delay = Duration::from_millis(50);
            for _ in 0..6 {
                tokio::time::sleep(delay).await;
                if let Ok(pipe) = try_connect(&identity.path).await {
                    return handshake(pipe).await;
                }
                delay = std::cmp::min(delay * 2, Duration::from_millis(800));
            }
            panic!("could not connect to daemon at {} after spawn", identity.path);
        }
    }
}

async fn try_connect(path: &str) -> std::io::Result<NamedPipeClient> {
    ClientOptions::new().open(path)
}

async fn handshake(mut pipe: NamedPipeClient) -> TestClient {
    let hello = Request::Hello {
        client_kind: "test".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        requested_idle_timeout_secs: None,
    };
    write_request(&mut pipe, &hello).await.expect("hello");
    let _welcome = read_response(&mut pipe).await.expect("welcome");
    TestClient { pipe }
}

fn spawn_daemon() {
    // Locate fastuse-daemon.exe beside the current test binary.
    let exe = std::env::current_exe().expect("current_exe");
    let dir = exe.parent().and_then(|d| d.parent()).expect("parent");
    let daemon = dir.join("fastuse-daemon.exe");
    if !daemon.exists() {
        panic!(
            "fastuse-daemon.exe not found at {} — run `cargo build --release` first",
            daemon.display()
        );
    }
    // Detach stdio so the daemon's tracing output does not interleave with
    // the test runner's stdout/stderr. We can't easily get DETACHED_PROCESS
    // semantics from std::process; piping to /NUL is good enough for tests.
    let _ = std::process::Command::new(&daemon)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn();
}

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
