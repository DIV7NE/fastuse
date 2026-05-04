//! Phase 2 input module — SendInput wrapper, modifier guard, UIPI check,
//! cursor helpers, per-tool handlers.
//!
//! All work on this side of the crate runs on the dedicated input STA thread
//! from `input_thread.rs` (Phase 1 D-26). Tokio workers MUST NOT call into
//! these helpers directly.

pub mod cursor;
pub mod handlers;
pub mod modifier_guard;
pub mod sendinput;
pub mod uipi;

pub use sendinput::{InputErr, MockSink};
