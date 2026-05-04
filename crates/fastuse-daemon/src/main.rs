//! fastuse-daemon entry point.
//!
//! Order is non-negotiable (D-24, D-25, D-03):
//!   1. set_per_monitor_v2_first_call() — DPI, MUST be first
//!   2. tracing_init::init()
//!   3. acquire_singleton(session_id) — exit 0 if already running
//!   4. write_sentinel()
//!   5. increment_mta_once()
//!   6. spawn input/uia/capture threads
//!   7. build tokio runtime
//!   8. server::serve() (Task 7)

mod dispatch;
mod idle;
mod sd;
mod sentinel;
mod server;
mod singleton;
mod tracing_init;

use fastuse_core::pipe_path_resolve;
use fastuse_win::{increment_mta_once, set_per_monitor_v2_first_call};

use crate::singleton::{acquire_singleton, AcquireOutcome};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "fastuse-daemon", version)]
struct Args {
    /// Idle timeout in seconds (0 disables; default 300 = 5 min).
    #[arg(long, default_value_t = 300)]
    idle_timeout: u64,
}

fn main() {
    set_per_monitor_v2_first_call();
    let args = Args::parse();

    // Tracing — keep guard alive for the rest of main.
    let _trace_guard = match tracing_init::init() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("fastuse-daemon: tracing init failed: {e}");
            None
        }
    };

    let identity = match pipe_path_resolve() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fastuse-daemon: pipe path resolve failed: {e}");
            std::process::exit(2);
        }
    };
    tracing::info!(
        session_id = identity.session_id,
        pipe = %identity.path,
        "daemon starting"
    );

    let _singleton = match acquire_singleton(identity.session_id) {
        Ok(AcquireOutcome::Acquired(g)) => g,
        Ok(AcquireOutcome::AlreadyRunning) => {
            eprintln!(
                "fastuse-daemon: another daemon is already running for session {}",
                identity.session_id
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("fastuse-daemon: singleton acquire failed: {e}");
            std::process::exit(2);
        }
    };

    if let Err(e) = sentinel::write_sentinel(std::process::id(), &identity.path, env!("CARGO_PKG_VERSION")) {
        tracing::warn!(error = %e, "failed to write sentinel");
    }
    let _sentinel_cleanup = scopeguard::guard((), |_| {
        sentinel::remove_sentinel();
    });

    increment_mta_once();

    // Initialize UIPI integrity-level cache before any input dispatches.
    fastuse_win::input::uipi::init_our_integrity_level();

    // Spawn fastuse-win worker threads. Held for daemon lifetime.
    let input = match fastuse_win::input_thread::spawn_input_thread() {
        Ok(h) => Some(std::sync::Arc::new(h)),
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn input thread");
            None
        }
    };

    // Phase 2: panic hook flushes held modifiers via the input thread channel.
    {
        let input_for_panic = input.clone();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(h) = input_for_panic.as_ref() {
                let _ = h.send(fastuse_win::input_thread::InputJob::FlushHeldModifiers);
            }
            prev(info);
        }));
    }
    let uia = match fastuse_win::uia_pool::spawn_uia_pool(3) {
        Ok(h) => Some(std::sync::Arc::new(h)),
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn uia pool");
            None
        }
    };
    let capture = match fastuse_win::capture_thread::spawn_capture_thread() {
        Ok(h) => Some(std::sync::Arc::new(h)),
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn capture thread");
            None
        }
    };

    // Build tokio runtime and run the pipe server (Task 7 wires server::serve).
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to build tokio runtime");
            std::process::exit(2);
        }
    };

    let server_result = rt.block_on(async {
        server::serve(
            identity.path.clone(),
            input.clone(),
            uia.clone(),
            capture.clone(),
            args.idle_timeout,
            identity.session_id,
        )
        .await
    });

    if let Err(e) = server_result {
        tracing::error!(error = %e, "server exited with error");
    }
    tracing::info!("daemon shutting down");
}
