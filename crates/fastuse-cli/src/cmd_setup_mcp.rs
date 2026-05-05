//! `fastuse-cli setup-mcp [--user | --project]` — register fastuse-mcp.exe in
//! Claude Code's settings.json idempotently. Reads existing JSON, adds
//! mcp.servers.fastuse, writes back. Prints next-step instruction.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn run(user: bool, project: bool) -> Result<()> {
    let target_path = if user || !project {
        let home = std::env::var("USERPROFILE").context("USERPROFILE not set")?;
        PathBuf::from(home).join(".claude").join("settings.json")
    } else {
        std::env::current_dir()?.join(".claude").join("settings.json")
    };

    let mcp_exe = locate_mcp_exe()?;

    let mut root: Value = if target_path.exists() {
        let s = std::fs::read_to_string(&target_path)
            .with_context(|| format!("read {target_path:?}"))?;
        if s.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&s)
                .with_context(|| format!("parse {target_path:?}"))?
        }
    } else {
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        json!({})
    };

    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings root is not an object"))?;
    let mcp = root_obj.entry("mcp").or_insert_with(|| json!({}));
    let mcp_obj = mcp
        .as_object_mut()
        .ok_or_else(|| anyhow!("mcp is not an object"))?;
    let servers = mcp_obj.entry("servers").or_insert_with(|| json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| anyhow!("mcp.servers is not an object"))?;

    let entry = json!({
        "command": mcp_exe.to_string_lossy(),
        "args": [],
        "env": {}
    });

    let updated = match servers_obj.get("fastuse") {
        Some(existing) if existing == &entry => false,
        _ => {
            servers_obj.insert("fastuse".to_string(), entry);
            true
        }
    };

    if updated {
        let pretty = serde_json::to_string_pretty(&root)?;
        std::fs::write(&target_path, pretty)?;
        println!("Wrote {target_path:?}. Restart Claude Code to pick up the change.");
    } else {
        println!("fastuse already registered in {target_path:?} (no change).");
    }

    Ok(())
}

fn locate_mcp_exe() -> Result<PathBuf> {
    let cli = std::env::current_exe()?;
    let dir = cli
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent"))?;
    let candidate = dir.join("fastuse-mcp.exe");
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(anyhow!(
            "fastuse-mcp.exe not found next to fastuse-cli.exe; expected at {candidate:?}"
        ))
    }
}
