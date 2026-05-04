//! `%LOCALAPPDATA%\fastuse\daemon.pid` sentinel file (D-06).
//!
//! Format:
//! ```text
//! pid=1234
//! started_at=2026-05-04T12:00:00Z
//! version=0.1.0
//! pipe=\\.\pipe\fastuse-1-1001
//! ```

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fastuse_core::local_app_data;

/// Resolve the sentinel path: `%LOCALAPPDATA%\fastuse\daemon.pid`.
pub fn sentinel_path() -> std::io::Result<PathBuf> {
    Ok(local_app_data()?.join("daemon.pid"))
}

/// Write the sentinel file. Overwrites any existing file.
pub fn write_sentinel(pid: u32, pipe: &str, version: &str) -> std::io::Result<()> {
    let path = sentinel_path()?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut f = fs::File::create(&path)?;
    writeln!(f, "pid={}", pid)?;
    writeln!(f, "started_at_unix={}", started)?;
    writeln!(f, "version={}", version)?;
    writeln!(f, "pipe={}", pipe)?;
    Ok(())
}

/// Remove the sentinel file (idempotent).
pub fn remove_sentinel() {
    if let Ok(p) = sentinel_path() {
        let _ = fs::remove_file(p);
    }
}
