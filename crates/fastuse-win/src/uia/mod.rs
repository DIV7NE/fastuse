//! Phase 3 UIA module: walk_subtree, selector resolution, win-event hook,
//! foreground HWND -> root cache, degraded heuristic, and the seven UIA
//! convenience tools (uia_tree, uia_query, inspect_at_point, click_element,
//! type_into_element, wait_for_element, scroll_into_view).
//!
//! All UIA work runs on the MTA UIA pool via `uia_pool::run`. Project rule
//! UIA-12 (enforced by `cargo xtask check-cacherequest`): no `Current[A-Z]*`
//! accessor calls; every read sources from a `IUIAutomationCacheRequest` +
//! `BuildUpdatedCache` pass.

pub mod degraded;
