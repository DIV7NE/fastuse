//! fastuse-win: the ONLY crate that links `windows` / `uiautomation`.
//!
//! Phase 1 ships:
//! - `dpi::set_per_monitor_v2_first_call()` — must be the first stmt in main()
//! - `com::increment_mta_once()` — process-wide MTA bump (D-25)
//! - `input_thread`/`uia_pool`/`capture_thread` — dedicated COM threads, no
//!   tokio worker ever calls into Windows API surface (D-26).

#![deny(missing_docs)]
#![warn(unsafe_op_in_unsafe_fn)]

// WR-06: this crate casts `u64` HWND wire values to `*mut _` for
// SetForegroundWindow / SetWindowPos. On a 32-bit Windows target the cast
// truncates the high 32 bits silently. The project is x64-only by stated
// constraint; enforce that at build time.
#[cfg(all(windows, not(target_pointer_width = "64")))]
compile_error!("fastuse-win requires a 64-bit Windows target");

pub mod capture;
pub mod capture_thread;
pub mod clipboard;
pub mod com;
pub mod dpi;
pub mod input;
pub mod input_thread;
pub mod launch;
pub mod process;
pub mod shell;
pub mod uia;
pub mod uia_pool;
pub mod window;

pub use com::increment_mta_once;
pub use dpi::set_per_monitor_v2_first_call;
