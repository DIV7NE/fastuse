//! `fastuse-cli stop` — send Shutdown to the running daemon.

use fastuse_proto::Request;
use serde_json::json;
use tokio::net::windows::named_pipe::ClientOptions;

use crate::proto_io::{read_response, write_request};

pub async fn run(pipe_path: &str) -> anyhow::Result<()> {
    let mut pipe = match ClientOptions::new().open(pipe_path) {
        Ok(p) => p,
        Err(_) => {
            println!("{}", json!({"ok": true, "stopped": false, "reason": "no daemon running"}));
            return Ok(());
        }
    };
    // Hello, then Shutdown.
    write_request(
        &mut pipe,
        &Request::Hello {
            client_kind: "cli".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            requested_idle_timeout_secs: None,
        },
    )
    .await?;
    let _welcome = read_response(&mut pipe).await?;
    write_request(&mut pipe, &Request::Shutdown).await?;
    let _ack = read_response(&mut pipe).await.ok();
    println!("{}", json!({"ok": true, "stopped": true}));
    Ok(())
}
