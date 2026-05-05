//! MCP server bootstrap: connect to daemon, register tool modules, serve stdio.

use std::sync::Arc;

use anyhow::Result;
use fastuse_core::pipe_path_resolve;
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

use crate::handler::Fastuse;

/// Boot the rmcp stdio MCP server.
///
/// Resolves the daemon pipe path, connects-or-spawns the daemon (lazily on
/// first tool call), registers all tool modules (currently wired through the
/// monolithic [`Fastuse`] handler — per-module split in Tasks 15/16), and
/// blocks until the MCP client disconnects.
///
/// # Preconditions
/// Caller must have invoked [`fastuse_win::set_per_monitor_v2_first_call`]
/// before this function (i.e. as the first statement in `main`).
pub async fn run() -> Result<()> {
    let identity = pipe_path_resolve()?;
    let server = Fastuse::new(identity.path);
    let (read, write) = stdio();
    let running = server.serve((read, write)).await?;
    running.waiting().await?;
    Ok(())
}
