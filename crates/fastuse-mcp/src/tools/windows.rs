//! Windows-specific helper tools registered as MCP tools.
//!
//! All implementations live on [`crate::handler::Fastuse`] via the
//! `#[tool_router(server_handler)]` macro, which requires a single impl
//! block for tool registration.  This module documents which handler methods
//! belong to the Windows-helpers surface.
//!
//! # Registered tools
//!
//! | MCP tool name         | handler method            | Request variant                 |
//! |-----------------------|---------------------------|---------------------------------|
//! | `list_windows`        | `Fastuse::list_windows`   | `Request::ListWindows`          |
//! | `focus_window`        | `Fastuse::focus_window`   | `Request::FocusWindow`          |
//! | `foreground_window`   | `Fastuse::foreground_window` | `Request::ForegroundWindow`  |
//! | `wait_for_window`     | `Fastuse::wait_for_window` | `Request::WaitForWindowV2`     |
//! | `list_processes`      | `Fastuse::list_processes` | `Request::ListProcesses`        |
//! | `kill_process`        | `Fastuse::kill_process`   | `Request::KillProcess`          |
//! | `launch_app`          | `Fastuse::launch_app`     | `Request::LaunchApp`            |
//! | `shell_exec`          | `Fastuse::shell_exec`     | `Request::ShellExec`            |
//! | `clipboard_get_text`  | `Fastuse::clipboard_get_text` | `Request::ClipboardGet`     |
//! | `clipboard_set_text`  | `Fastuse::clipboard_set_text` | `Request::ClipboardSet`     |
