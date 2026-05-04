//! Shell-related platform-neutral helpers.
//!
//! Currently exposes per-shell argument quoting and argv construction.
//! Win32 process spawning lives in `fastuse-win::shell`.

pub mod quoting;

pub use quoting::{build_argv, quote, Shell};
