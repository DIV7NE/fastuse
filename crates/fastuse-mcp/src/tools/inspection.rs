//! UIA inspection tools registered as MCP tools.
//!
//! All implementations live on [`crate::handler::Fastuse`] via the
//! `#[tool_router(server_handler)]` macro.  These tools are intentionally
//! read-only (DevTools-style structure inspection) — they return data, never
//! click or mutate state.
//!
//! # Registered tools
//!
//! | MCP tool name   | handler method                | Request variant              |
//! |-----------------|-------------------------------|------------------------------|
//! | `inspect_at`    | `Fastuse::inspect_at_point`   | `Request::InspectAtPoint`    |
//! | `uia_query`     | `Fastuse::uia_query`          | `Request::UiaQuery`          |
//! | `uia_tree`      | `Fastuse::uia_tree`           | `Request::UiaTree`           |
