//! `fastuse-cli status` — read sentinel + 50ms ping probe.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fastuse_core::local_app_data;
use fastuse_proto::{Request, Response};
use serde_json::json;
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::time::timeout;

use crate::proto_io::{read_response, write_request};

pub async fn run(pipe_path: &str) -> anyhow::Result<()> {
    // Read sentinel.
    let sentinel = local_app_data()?.join("daemon.pid");
    let sentinel_contents = std::fs::read_to_string(&sentinel).unwrap_or_default();

    // 50ms ping probe.
    let probe = timeout(Duration::from_millis(50), async {
        let mut pipe = ClientOptions::new().open(pipe_path).ok()?;
        let _ = write_request(
            &mut pipe,
            &Request::Hello {
                client_kind: "cli".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                requested_idle_timeout_secs: None,
            },
        )
        .await;
        let _ = read_response(&mut pipe).await;
        let now_us = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_micros() as u64;
        let send = Instant::now();
        write_request(&mut pipe, &Request::Ping { ts_us: now_us }).await.ok()?;
        let res = read_response(&mut pipe).await.ok()?;
        let rtt_us = send.elapsed().as_micros() as u64;
        match res {
            Response::Pong { daemon_pid, session_id, .. } => Some((rtt_us, daemon_pid, session_id)),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten();

    let running = probe.is_some();
    let mut report = json!({
        "ok": true,
        "running": running,
        "pipe": pipe_path,
        "sentinel_path": sentinel.display().to_string(),
        "sentinel_present": sentinel.exists(),
        "sentinel_contents": sentinel_contents,
    });
    if let Some((rtt_us, pid, session)) = probe {
        report["last_ping_rtt_us"] = json!(rtt_us);
        report["daemon_pid"] = json!(pid);
        report["session_id"] = json!(session);
    }
    println!("{}", report);
    Ok(())
}
