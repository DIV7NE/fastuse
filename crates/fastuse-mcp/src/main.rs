//! fastuse-mcp — rmcp 1.6 stdio MCP server exposing the fastuse `ping` tool.

mod handler;
mod proto_io;
mod spawn;

use fastuse_core::pipe_path_resolve;
use fastuse_win::set_per_monitor_v2_first_call;
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

use crate::handler::Fastuse;

fn main() {
    set_per_monitor_v2_first_call();

    let identity = match pipe_path_resolve() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fastuse-mcp: pipe path resolve failed: {e}");
            std::process::exit(2);
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime build");

    if let Err(e) = rt.block_on(run(identity.path)) {
        eprintln!("fastuse-mcp: {e}");
        std::process::exit(1);
    }
}

async fn run(pipe_path: String) -> anyhow::Result<()> {
    let (read, write) = stdio();
    let server = Fastuse::new(pipe_path);
    let running = server.serve((read, write)).await?;
    running.waiting().await?;
    Ok(())
}
