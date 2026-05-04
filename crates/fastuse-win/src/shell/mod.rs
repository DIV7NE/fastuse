//! `shell_exec` runner — tokio::process::Command with streaming, timeout,
//! and 1 MiB combined-output cap.
//!
//! **Forbidden in this module**: `std::process::Command` — blocks the tokio
//! runtime (PITFALLS anti-pattern). Enforced by `xtask check-redact` lint.
//!
//! Output payloads are always wrapped in `Redact<Vec<u8>>` and never logged
//! verbatim; tracing spans only ever record byte counts and exit status.

pub mod cap;
pub mod runner;

pub use runner::{shell_exec, OUTPUT_CAP_BYTES};
