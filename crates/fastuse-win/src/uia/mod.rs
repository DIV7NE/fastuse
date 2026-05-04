//! Phase 3 UIA module: walk_subtree, selector resolution, cache, degraded
//! heuristic, and the seven UIA convenience tools (uia_tree, uia_query,
//! inspect_at_point, click_element, type_into_element, wait_for_element,
//! scroll_into_view).
//!
//! All UIA work runs on the MTA UIA pool via `uia_pool::run`. Project rule
//! UIA-12 (enforced by `cargo xtask check-cacherequest`): no `Current[A-Z]*`
//! accessor calls; every read sources from a `IUIAutomationCacheRequest` +
//! `BuildUpdatedCache` pass.

pub mod automation;
pub mod cache;
pub mod degraded;
pub mod element_actions;
pub mod find;
pub mod inspect;
pub mod query;
pub mod tree;
pub mod walk;
pub mod wait_for_element;

pub use automation::foreground_hwnd;
pub use degraded::{detect_degraded, KNOWN_DEGRADED_PROCESSES};
pub use element_actions::{handle_click_element, handle_scroll_into_view, handle_type_into_element};
pub use inspect::handle_inspect_at_point;
pub use query::handle_uia_query;
pub use tree::handle_uia_tree;
pub use wait_for_element::handle_wait_for_element;
