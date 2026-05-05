//! fastuse-core: platform-agnostic helpers + a narrow `windows` exception
//! for resolving the session-scoped pipe path (per D-21).
//!
//! The `windows` crate dependency here is intentionally narrow: only the
//! handful of Win32 calls needed to materialize the runtime pipe path
//! (`WTSGetActiveConsoleSessionId`, `ProcessIdToSessionId`, `OpenProcessToken`,
//! `GetTokenInformation(TokenUser)`) and to locate `%LOCALAPPDATA%`.
//! All other Windows API surface lives in `fastuse-win`.

#![deny(missing_docs)]

pub mod config;
pub mod error;
pub mod hot_state;
pub mod local_app_data;
pub mod perm;
pub mod pipe_path;
pub mod shell;

pub use error::FastuseError;
pub use hot_state::HotState;
pub use local_app_data::local_app_data;
pub use perm::{resolve as resolve_perm, hint as perm_hint, SessionAllow, Tier};
pub use pipe_path::{pipe_path_resolve, PipeIdentity};
