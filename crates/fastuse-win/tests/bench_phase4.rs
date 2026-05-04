//! Quick smoke benchmarks for Phase 4 primitives — not criterion-grade,
//! just enough to show the SUMMARY's headline p50 numbers are real.
//! Run with `cargo test --release -p fastuse-win --test bench_phase4 -- --nocapture --test-threads=1`.

use std::time::Instant;

use fastuse_proto::{ClipFormat, ClipboardGet, ClipboardSet, ListProcesses, Redact, ShellExec, ShellKind};

fn p50(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
fn bench_list_processes_p50() {
    fastuse_win::process::enum_proc::invalidate_cache();
    let mut samples = Vec::new();
    for _ in 0..20 {
        fastuse_win::process::enum_proc::invalidate_cache();
        let t = Instant::now();
        let _ = fastuse_win::process::list_processes(None);
        samples.push(t.elapsed().as_micros());
    }
    eprintln!("list_processes (cold) p50 = {} us", p50(samples));
}

#[test]
fn bench_clipboard_get_text_p50() {
    let _ = fastuse_win::clipboard::clipboard_set(ClipboardSet::Text(Redact::new(
        "fastuse-bench-marker".into(),
    )));
    let mut samples = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        let _ = fastuse_win::clipboard::clipboard_get(ClipboardGet {
            format: Some(ClipFormat::Text),
        });
        samples.push(t.elapsed().as_micros());
    }
    eprintln!("clipboard_get_text p50 = {} us", p50(samples));
}

#[tokio::test]
async fn bench_shell_exec_p50() {
    let mut samples = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        let _ = fastuse_win::shell::shell_exec(ShellExec {
            command: Redact::new("exit 0".into()),
            shell: Some(ShellKind::Cmd),
            env: None,
            cwd: None,
            timeout_ms: Some(5_000),
            stream_chunk_size: None,
        })
        .await;
        samples.push(t.elapsed().as_millis());
    }
    eprintln!("shell_exec(cmd /D /C exit 0) p50 = {} ms", p50(samples));
}
