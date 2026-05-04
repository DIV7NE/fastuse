//! `fastuse-cli start` — explicit daemon start (idempotent).

use serde_json::json;

use crate::spawn::connect_or_spawn;

pub async fn run(pipe_path: &str) -> anyhow::Result<()> {
    // connect_or_spawn is the same primitive — if the daemon is already up,
    // it just connects; otherwise it spawns and connects.
    let _ = connect_or_spawn(pipe_path).await?;
    println!("{}", json!({"ok": true, "running": true}));
    Ok(())
}
