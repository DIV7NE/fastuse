//! Aimbot-grade element targeting.
//!
//! Layered as: `profile` (fingerprint window+element) → `candidate` (rank) →
//! `strategy` (pick) → `hit_test` (gate) → `verify` (postcondition) →
//! `execute` (orchestrate). The module owns no `windows::*` calls itself —
//! it routes work through existing `uia_pool`, `capture_thread`, and the new
//! `ocr_thread`.
//!
//! See `docs/superpowers/specs/2026-05-05-fastuse-targeting-aimbot-design.md`.

pub mod candidate;
pub mod execute;
pub mod hit_test;
pub mod profile;
pub mod strategy;
pub mod verify;

pub use candidate::{GeometrySource, PatternSet, TargetCandidate};
pub use execute::{execute_targeted, TargetedRequest};
pub use profile::{
    invalidate_profile_cache, profile_window, IntegrityLevel, ProfileCacheKey, TargetProfile,
    TreeQuality, WindowSignals,
};
pub use strategy::pick_strategy;
pub use verify::{poll_until, VerifyOutcome};
