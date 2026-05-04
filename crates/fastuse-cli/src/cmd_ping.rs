//! `fastuse-cli ping` — connect, handshake, send N pings, report.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fastuse_proto::{Request, Response};
use serde_json::json;
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::proto_io::{read_response, write_request};
use crate::spawn::connect_or_spawn;

pub async fn run(pipe_path: &str, bench: u32, pretty: bool) -> anyhow::Result<()> {
    if bench <= 1 {
        let mut pipe = connect_or_spawn(pipe_path).await?;
        handshake(&mut pipe).await?;
        let (rtt_us, daemon_pid, session_id) = single_ping(&mut pipe).await?;
        emit(rtt_us, daemon_pid, session_id, pretty);
        return Ok(());
    }

    // --bench N: cold = first iteration, warm = remaining.
    let cold_start = Instant::now();
    let mut pipe = connect_or_spawn(pipe_path).await?;
    handshake(&mut pipe).await?;
    let cold_us = cold_start.elapsed().as_micros() as u64;

    let mut samples: Vec<u64> = Vec::with_capacity(bench as usize);
    let mut last_pid: u32 = 0;
    let mut last_session: u32 = 0;
    for _ in 0..bench {
        let (rtt_us, pid, session) = single_ping(&mut pipe).await?;
        samples.push(rtt_us);
        last_pid = pid;
        last_session = session;
    }

    samples.sort_unstable();
    let warm_p50 = samples[samples.len() / 2];
    let warm_p99 = samples[((samples.len() as f64) * 0.99).floor() as usize];
    let warm_min = samples.first().copied().unwrap_or(0);
    let warm_max = samples.last().copied().unwrap_or(0);

    let report = json!({
        "ok": true,
        "iterations": bench,
        "cold_total_us": cold_us,
        "warm_p50_us": warm_p50,
        "warm_p99_us": warm_p99,
        "warm_min_us": warm_min,
        "warm_max_us": warm_max,
        "daemon_pid": last_pid,
        "session_id": last_session,
    });
    if pretty {
        println!("ping --bench {bench}");
        println!("  cold first-connect+handshake: {} us", cold_us);
        println!("  warm  p50: {} us  p99: {} us  min: {} us  max: {} us",
                 warm_p50, warm_p99, warm_min, warm_max);
        println!("  daemon_pid={}, session_id={}", last_pid, last_session);
    } else {
        println!("{}", serde_json::to_string(&report)?);
    }
    Ok(())
}

async fn handshake(pipe: &mut NamedPipeClient) -> anyhow::Result<()> {
    let hello = Request::Hello {
        client_kind: "cli".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        requested_idle_timeout_secs: None,
    };
    write_request(pipe, &hello).await?;
    let _welcome = read_response(pipe).await?;
    Ok(())
}

async fn single_ping(pipe: &mut NamedPipeClient) -> anyhow::Result<(u64, u32, u32)> {
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_micros() as u64;
    let send_start = Instant::now();
    write_request(pipe, &Request::Ping { ts_us: now_us }).await?;
    let res = read_response(pipe).await?;
    let rtt_us = send_start.elapsed().as_micros() as u64;
    match res {
        Response::Pong { daemon_pid, session_id, .. } => Ok((rtt_us, daemon_pid, session_id)),
        Response::Error(e) => anyhow::bail!("daemon error: {e}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

fn emit(rtt_us: u64, daemon_pid: u32, session_id: u32, pretty: bool) {
    if pretty {
        println!("ok=true rtt_us={} daemon_pid={} session_id={}",
                 rtt_us, daemon_pid, session_id);
    } else {
        let v = json!({
            "ok": true,
            "rtt_us": rtt_us,
            "daemon_pid": daemon_pid,
            "session_id": session_id,
        });
        println!("{}", v);
    }
}
