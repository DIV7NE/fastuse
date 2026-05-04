//! Pipe accept loop. Task 7 implements the full version; Task 6 ships a
//! placeholder that simply blocks until shutdown so the daemon binary builds.

use fastuse_win::{capture_thread::CaptureThreadHandle, input_thread::InputThreadHandle, uia_pool::UiaPoolHandle};

/// Run the daemon server. Phase 1 Task 6 placeholder; Task 7 fills the body.
pub async fn serve(
    pipe_path: String,
    _input: Option<&InputThreadHandle>,
    _uia: Option<&UiaPoolHandle>,
    _capture: Option<&CaptureThreadHandle>,
) -> std::io::Result<()> {
    tracing::info!(pipe = %pipe_path, "server placeholder running; waiting for ctrl-c");
    tokio::signal::ctrl_c().await?;
    Ok(())
}
