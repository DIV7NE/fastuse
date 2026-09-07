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

// No console window. The daemon logs to stderr *and* a rolling file; when it
// owns a console, a stray click puts that console in QuickEdit mark mode,
// which blocks every stderr write and freezes the daemon while it still holds
// the pipe name. Debug builds keep the console for development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod action_opts;
mod dispatch;
mod drag_helper;
mod idle;
mod sd;
mod sentinel;
mod server;
mod session;
mod singleton;
mod tracing_init;
mod warmup;

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
    /// Comma-separated list of Confirmed-tier tools to grant (or `*` for all).
    /// Falls back to the `FASTUSE_ALLOW` env var. Set ONCE at daemon
    /// start; immutable for the lifetime of every session served.
    #[arg(long, default_value = "")]
    allow: String,
}

fn parse_allow(arg: &str, env_fallback: Option<String>) -> Vec<String> {
    let raw = if arg.is_empty() {
        env_fallback.unwrap_or_default()
    } else {
        arg.to_string()
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn main() {
    set_per_monitor_v2_first_call();

    // Medium-integrity drag-source mode. This must precede Args::parse (clap
    // rejects the flag), the singleton, the sentinel and the pipe: the helper
    // is the same binary re-executed, and it must never try to be the daemon.
    if std::env::args().any(|a| a == "--drag-helper") {
        return drag_helper::run();
    }

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

    // WR-09: install the panic hook BEFORE any thread that could touch HELD
    // exists. The hook reads from a shared slot that we populate after
    // spawn_input_thread succeeds. A panic during input-thread startup runs
    // through this hook even though the slot is still None — the hook then
    // simply skips the flush (registry is empty by definition).
    //
    // CR-04: hook also detects "panic on the input thread itself" via the
    // recorded thread id — calling InputThreadHandle::send from inside the
    // input thread's own panic would deadlock (recv waits for a reply the
    // panicking thread will never produce). When the panicking thread IS
    // the input thread we flush HELD directly (it's a thread_local on this
    // very thread, so the inline call drains it correctly).
    type InputSlot =
        std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<fastuse_win::input_thread::InputThreadHandle>>>>;
    let input_slot: InputSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    {
        let input_for_panic: InputSlot = std::sync::Arc::clone(&input_slot);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Snapshot the handle (and its thread id) under the mutex.
            let snapshot: Option<std::sync::Arc<fastuse_win::input_thread::InputThreadHandle>> =
                input_for_panic.lock().ok().and_then(|g| g.as_ref().cloned());
            if let Some(h) = snapshot {
                // SAFETY: GetCurrentThreadId is always safe.
                let here = unsafe {
                    windows::Win32::System::Threading::GetCurrentThreadId()
                };
                if here == h.thread_id() {
                    // We ARE the input thread mid-panic. Flush its
                    // thread_local registry directly (CR-04).
                    fastuse_win::input::handlers::flush_held_modifiers();
                } else {
                    // Different thread panicked; channel-send is safe.
                    let _ = h.send(fastuse_win::input_thread::InputJob::FlushHeldModifiers);
                }
            }
            prev(info);
        }));
    }

    // Spawn fastuse-win worker threads. Held for daemon lifetime.
    let input = match fastuse_win::input_thread::spawn_input_thread() {
        Ok(h) => {
            let arc = std::sync::Arc::new(h);
            if let Ok(mut slot) = input_slot.lock() {
                *slot = Some(std::sync::Arc::clone(&arc));
            }
            Some(arc)
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn input thread");
            None
        }
    };
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
    // Best-effort warmup runs in a background thread once the pipe server is
    // up — running it on the spawn path can deadlock first-call DXGI duplication
    // before the pipe is bound, so the CLI never sees the daemon.
    {
        let uia_w = uia.clone();
        let capture_w = capture.clone();
        std::thread::spawn(move || {
            let _ = warmup::run(
                uia_w.as_ref().map(|a| &**a),
                capture_w.as_ref().map(|a| &**a),
            );
        });
    }

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

    let allow = parse_allow(&args.allow, std::env::var("FASTUSE_ALLOW").ok());
    if !allow.is_empty() {
        tracing::info!(?allow, "permission allow-list active");
    }

    // Load config and build the safe-mode Permissions policy.
    let cfg_path = fastuse_core::config::default_path();
    let config = match cfg_path.as_ref() {
        Some(p) => fastuse_core::config::load_or_default(p),
        None => fastuse_core::config::Config::default(),
    };
    let permissions = std::sync::Arc::new(
        fastuse_win::permissions::Permissions::from_env_and_config(
            config.permissions.safe_mode,
            config.permissions.gated_tools.clone(),
        ),
    );
    tracing::info!(
        safe_mode = permissions.safe_mode,
        gated_count = permissions.gated.len(),
        "permissions resolved",
    );

    let server_result = rt.block_on(async {
        server::serve(
            identity.path.clone(),
            input.clone(),
            uia.clone(),
            capture.clone(),
            args.idle_timeout,
            identity.session_id,
            allow,
            permissions,
        )
        .await
    });

    if let Err(e) = server_result {
        tracing::error!(error = %e, "server exited with error");
    }
    tracing::info!("daemon shutting down");

    // SH-01: explicit Request::Shutdown path leaves an in-flight I/O on the
    // pipe that tokio's Runtime::Drop can't cancel — Drop hangs forever,
    // process never exits, singleton mutex stays held, next CLI call blocks.
    // shutdown_background() returns immediately and lets the OS reclaim
    // worker threads. Idle-timeout path was unaffected (no live connection
    // when shutdown fires) which is why the bug only surfaced on `stop`.
    rt.shutdown_background();
    drop(capture);
    drop(uia);
    drop(input);
}
