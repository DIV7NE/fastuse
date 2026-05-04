//! fastuse-cli — local helper CLI for the daemon.

mod cmd_ping;
mod cmd_start;
mod cmd_status;
mod cmd_stop;
mod proto_io;
mod spawn;

use clap::{Parser, Subcommand};
use fastuse_core::pipe_path_resolve;
use fastuse_win::set_per_monitor_v2_first_call;

#[derive(Parser, Debug)]
#[command(name = "fastuse-cli", version, about = "fastuse local helper CLI")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Round-trip a ping through the daemon.
    Ping {
        /// Run N iterations and report cold/warm separately.
        #[arg(long, default_value_t = 1)]
        bench: u32,
        /// Pretty human-readable output.
        #[arg(long, default_value_t = false)]
        pretty: bool,
    },
    /// Start the daemon (idempotent).
    Start,
    /// Stop the running daemon.
    Stop,
    /// Print daemon status (sentinel + 50ms ping probe).
    Status,
}

fn main() {
    set_per_monitor_v2_first_call();
    let cli = Cli::parse();

    let identity = match pipe_path_resolve() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fastuse-cli: pipe path resolve failed: {e}");
            std::process::exit(2);
        }
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime build");

    let result: anyhow::Result<()> = rt.block_on(async {
        match cli.command {
            Cmd::Ping { bench, pretty } => cmd_ping::run(&identity.path, bench, pretty).await,
            Cmd::Start => cmd_start::run(&identity.path).await,
            Cmd::Stop => cmd_stop::run(&identity.path).await,
            Cmd::Status => cmd_status::run(&identity.path).await,
        }
    });

    if let Err(e) = result {
        let err = serde_json::json!({"ok": false, "error": e.to_string()});
        eprintln!("{}", err);
        std::process::exit(1);
    }
}
