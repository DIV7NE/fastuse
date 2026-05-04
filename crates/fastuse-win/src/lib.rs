//! fastuse-win: the ONLY crate that links `windows` / `uiautomation`.
//!
//! Phase 1 ships:
//! - `dpi::set_per_monitor_v2_first_call()` — must be the first stmt in main()
//! - `com::increment_mta_once()` — process-wide MTA bump (D-25)
//! - `input_thread`/`uia_pool`/`capture_thread` — dedicated COM threads, no
//!   tokio worker ever calls into Windows API surface (D-26).

#![deny(missing_docs)]
#![warn(unsafe_op_in_unsafe_fn)]

pub mod com;
pub mod dpi;
pub mod input;
pub mod input_thread;
pub mod uia_pool;
pub mod capture_thread;
pub mod window;

pub use com::increment_mta_once;
pub use dpi::set_per_monitor_v2_first_call;
