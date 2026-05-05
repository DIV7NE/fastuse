//! MCP tool modules — each is a thin shim: parse args → build Request →
//! daemon.send → format Response into MCP tool result.
//!
//! Current state (Task 14 scaffold): tool implementations live in
//! [`crate::handler`] (v1 monolith preserved). These sub-modules will absorb
//! them in Tasks 15 and 16.

pub mod computer;
pub mod inspection;
pub mod meta;
pub mod windows;
