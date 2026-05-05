//! fastuse-mcp — rmcp 1.6 stdio MCP server exposing the fastuse tool surface.

mod client;
mod handler;
mod proto_io;
mod server;
mod spawn;
pub mod tools;

fn main() -> anyhow::Result<()> {
    fastuse_win::set_per_monitor_v2_first_call();

    // JSON structured logs to stderr so stdout remains clean for MCP framing.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    rt.block_on(server::run())
}
