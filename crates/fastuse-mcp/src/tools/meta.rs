//! Daemon meta-tools registered as MCP tools.
//!
//! All implementations live on [`crate::handler::Fastuse`] via the
//! `#[tool_router(server_handler)]` macro.
//!
//! # Registered tools
//!
//! | MCP tool name | handler method      | Notes                                              |
//! |---------------|---------------------|----------------------------------------------------|
//! | `ping`        | `Fastuse::ping`     | Round-trip RTT probe; returns daemon PID + session |
//! | `status`      | `Fastuse::status`   | Non-fatal: returns `running=false` if daemon is down |
//! | `stop`        | `Fastuse::stop`     | Idempotent graceful shutdown via `Request::Shutdown` |
//! | `warmup`      | `Fastuse::warmup`   | Touches D3D11, DXGI, UIA root, monitors            |
