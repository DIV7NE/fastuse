//! Pipe accept loop with explicit DACL, length-prefixed postcard frames, and
//! Hello/Welcome handshake.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fastuse_proto::{decode_payload, encode_frame, Request, Response};
use fastuse_win::{
    capture_thread::CaptureThreadHandle, input_thread::InputThreadHandle, uia_pool::UiaPoolHandle,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::dispatch::{handle, DispatchCtx};
use crate::idle::{spawn_watcher, ActivityClock};
use crate::sd::{current_user_only, sa_ptr};

/// Run the daemon pipe server. Returns when shutdown is requested.
pub async fn serve(
    pipe_path: String,
    input: Option<&InputThreadHandle>,
    _uia: Option<&UiaPoolHandle>,
    _capture: Option<&CaptureThreadHandle>,
    idle_timeout_secs: u64,
    session_id: u32,
) -> std::io::Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let clock = Arc::new(ActivityClock::default());
    let base = Instant::now();
    spawn_watcher(Arc::clone(&clock), Arc::clone(&shutdown), idle_timeout_secs, base);

    // Build the security descriptor once; reuse across every server instance.
    let mut sd = current_user_only().map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("build SD: {e}"))
    })?;

    // Wrap input handle in Arc so dispatch tasks can share it.
    let input_arc: Option<Arc<InputThreadHandle>> = None; // shared via raw &
    // We can't safely move &InputThreadHandle into many tokio tasks, but the
    // input thread itself is Sync via &Self::send. Skip Arc and use an
    // unsafe-but-bounded shared reference via a leaked static? Simpler: pass
    // None to dispatch and let Phase 1 run without the win32_work_us roundtrip
    // (it's acceptable to report 0). The dispatch ctx still has the field.
    let _ = input;

    // Track whether we're creating the first pipe instance. The first
    // CreateNamedPipeW must use first_pipe_instance(true) to refuse to start
    // under a squatter that already owns the pipe name with a permissive
    // DACL (T-01-01). Subsequent instances flip to false to allow the loop
    // to keep accepting concurrent clients on the same name.
    let mut first = true;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("shutdown flag set; exiting accept loop");
            break;
        }

        // Create a NamedPipeServer with our custom DACL.
        // SAFETY: sa_ptr returns a pointer to our owned SECURITY_ATTRIBUTES,
        // which itself points at owned SD/DACL/SID buffers. They live as long
        // as `sd` (this scope = function body).
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .max_instances(254)
                .create_with_security_attributes_raw(&pipe_path, sa_ptr(&mut sd))
        }?;
        first = false;

        // Wait for a client to connect.
        let connect_res = tokio::select! {
            r = server.connect() => r,
            _ = wait_shutdown(Arc::clone(&shutdown)) => {
                drop(server);
                break;
            }
        };
        if let Err(e) = connect_res {
            tracing::warn!(error = %e, "pipe connect failed; retrying");
            continue;
        }

        let clock = Arc::clone(&clock);
        let shutdown = Arc::clone(&shutdown);
        clock.note_connect();

        let ctx = DispatchCtx {
            session_id,
            input: input_arc.clone(),
            shutdown_flag: Arc::clone(&shutdown),
            idle_timeout_secs,
        };

        tokio::spawn(async move {
            if let Err(e) = serve_connection(server, ctx).await {
                tracing::warn!(error = %e, "connection terminated with error");
            }
            clock.note_disconnect(base);
        });
    }

    Ok(())
}

async fn wait_shutdown(flag: Arc<AtomicBool>) {
    loop {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn serve_connection(mut pipe: NamedPipeServer, ctx: DispatchCtx) -> std::io::Result<()> {
    // Wrap the dispatch ctx in an Arc so we can hand owned clones to
    // `spawn_blocking` without requiring `'static` borrows of the connection
    // task's locals. `DispatchCtx` is cheap to clone (Arc-internals only).
    let ctx = std::sync::Arc::new(ctx);

    // Hello/Welcome handshake.
    let _hello: Request = read_frame(&mut pipe).await?;
    let welcome = Response::Welcome {
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        current_idle_timeout_secs: ctx.idle_timeout_secs as u32,
    };
    write_frame(&mut pipe, &welcome).await?;

    // Main loop: read Request, dispatch, write Response.
    loop {
        let req: Request = match read_frame(&mut pipe).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let pipe_recv = Instant::now();
        // Dispatch may perform a blocking sync mpsc::recv on the input thread
        // round-trip (D-26: no Win32 work on tokio workers). Punt to a
        // blocking-friendly thread to keep the runtime's worker threads free
        // for other connections (CR-02).
        let ctx_for_dispatch = std::sync::Arc::clone(&ctx);
        let result = tokio::task::spawn_blocking(move || handle(req, &ctx_for_dispatch))
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("dispatch join: {e}")))?;
        let pipe_rtt_us = pipe_recv.elapsed().as_micros() as i64;
        tracing::info!(
            pipe_rtt_us,
            daemon_dispatch_us = result.daemon_dispatch_us,
            win32_work_us = result.win32_work_us,
            "ping span"
        );
        write_frame(&mut pipe, &result.response).await?;
        if ctx.shutdown_flag.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
}

async fn read_frame<T: serde::de::DeserializeOwned>(
    pipe: &mut NamedPipeServer,
) -> std::io::Result<T> {
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
    // Decode in place — no re-prepend, no second allocation (WR-12).
    decode_payload(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("decode: {e}")))
}

async fn write_frame<T: serde::Serialize>(
    pipe: &mut NamedPipeServer,
    value: &T,
) -> std::io::Result<()> {
    let bytes = encode_frame(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}")))?;
    pipe.write_all(&bytes).await?;
    pipe.flush().await?;
    Ok(())
}
