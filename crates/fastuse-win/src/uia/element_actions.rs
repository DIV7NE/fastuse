//! Element-targeting action handlers (Phase 3 Tasks 16, 18; UIA-05/06/08).
//!
//! click_element / type_into_element / scroll_into_view all share the same
//! shape: resolve the element on the UIA pool (one walk via `walk_subtree`,
//! filter via `find_first`), pull centroid/focus/scroll behavior off the
//! cached element, then delegate to Phase 2 `input::handlers` via the
//! input thread.
//!
//! No duplicate input logic lives here — D-26 (one COM thread per surface)
//! demands that `SendInput` stays on the input thread. UIA work stays on
//! the UIA pool. This module is the orchestrator.

use std::sync::Arc;

use fastuse_proto::{
    coords::Rect, Error as ProtoError, ErrorCode, MouseButton, Redact, Response, Selector,
};
use uiautomation::patterns::{UIPatternType, UIScrollItemPattern, UIValuePattern};
use uiautomation::{UIAutomation, UIElement};

use crate::input::handlers as ih;
use crate::input_thread::InputThreadHandle;
use crate::uia::automation::foreground_hwnd;
use crate::uia::cache::get_or_fetch;
use crate::uia::walk::{build_subtree_cache_request, map_control_type, walk_cached};
use crate::uia_pool::UiaPoolHandle;

/// What we ship back from the UIA pool to the input thread for click/type.
#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedElement {
    rect: Rect,
}

/// Locate the first element matching `selector` under the foreground (or
/// explicit) HWND, returning its cached bounding rect. Optionally calls
/// `set_focus` on the element before returning (for type_into_element).
fn resolve_element(
    pool: &UiaPoolHandle,
    selector: Selector,
    focus_first: bool,
    scroll_into_view_first: bool,
) -> Result<ResolvedElement, ProtoError> {
    let hwnd = foreground_hwnd().ok_or_else(|| {
        ProtoError::new(ErrorCode::WindowNotFound, "no foreground window".to_string())
    })?;
    pool.run(move |uia| {
        let root = get_or_fetch(uia, hwnd)?;
        // CR-02: single BuildUpdatedCache walk per call. `cached_root` is
        // the cache-populated UIElement we use both to build the UIANode
        // snapshot (`walk_cached`) AND to relocate the live element via
        // `get_cached_children()` — no second walk.
        let cached_root = build_cached_root(uia, &root)?;
        let tree = walk_cached(&cached_root, u32::MAX)?;
        let target_node = match find_first_in_tree(&tree, &selector) {
            Some(n) => n,
            None => {
                return Err(ProtoError::new(
                    ErrorCode::ElementNotFound,
                    "selector matched no element".to_string(),
                ))
            }
        };
        // Re-resolve a live UIElement from the cached_root using a property
        // filter that uniquely identifies the target. AutomationId is
        // strongest; fall back to Name+ControlType.
        let live = relocate_live_element(uia, &cached_root, &target_node)?;
        if scroll_into_view_first {
            if let Ok(p) = live.get_cached_pattern::<UIScrollItemPattern>() {
                let _ = p.scroll_into_view();
            }
        }
        if focus_first {
            // SetFocus is a method, not a Current* accessor — UIA-12 safe.
            live.set_focus().map_err(|e| {
                ProtoError::new(
                    ErrorCode::Internal,
                    format!("SetFocus on element failed: {e}"),
                )
            })?;
        }
        Ok(ResolvedElement {
            rect: target_node.bounding_rect,
        })
    })
}

/// Build a cache-populated `UIElement` rooted at `root`. Single
/// `BuildUpdatedCache` call. Both the `UIANode` snapshot (via `walk_cached`)
/// and the live-element relocation (via `get_cached_children()`) source
/// from this one cached root — CR-02.
fn build_cached_root(
    uia: &UIAutomation,
    root: &UIElement,
) -> Result<UIElement, ProtoError> {
    let req = build_subtree_cache_request(uia, fastuse_proto::TreeView::Content)?;
    root.build_updated_cache(&req).map_err(|e| {
        ProtoError::new(ErrorCode::Internal, format!("build_updated_cache: {e}"))
    })
}

/// Re-locate a live `UIElement` from the cached root that matches the
/// `target_node`'s identifying properties. Walks the cached children.
fn relocate_live_element(
    _uia: &UIAutomation,
    cached_root: &UIElement,
    target: &fastuse_proto::UIANode,
) -> Result<UIElement, ProtoError> {
    fn dfs(el: &UIElement, target: &fastuse_proto::UIANode) -> Option<UIElement> {
        if matches_node(el, target) {
            return Some(el.clone());
        }
        if let Ok(kids) = el.get_cached_children() {
            for k in &kids {
                if let Some(hit) = dfs(k, target) {
                    return Some(hit);
                }
            }
        }
        None
    }
    dfs(cached_root, target).ok_or_else(|| {
        ProtoError::new(
            ErrorCode::ElementNotFound,
            "could not relocate live element from cached snapshot".to_string(),
        )
    })
}

fn matches_node(el: &UIElement, target: &fastuse_proto::UIANode) -> bool {
    // Control type first. A file dialog's filename ComboBox and the Edit
    // inside it publish the same AutomationId, and an AutomationId-only match
    // lands on the ancestor — whose ValuePattern write goes nowhere.
    if el.get_cached_control_type().map(map_control_type).ok() != Some(target.control_type) {
        return false;
    }
    let aid = el.get_cached_automation_id().unwrap_or_default();
    if !target.automation_id.is_empty() {
        return aid == target.automation_id;
    }
    let name = el.get_cached_name().unwrap_or_default();
    name == target.name && el.get_cached_classname().unwrap_or_default() == target.class_name
}

fn find_first_in_tree(
    node: &fastuse_proto::UIANode,
    selector: &Selector,
) -> Option<fastuse_proto::UIANode> {
    if selector.matches(node) {
        return Some(node.clone());
    }
    for child in &node.children {
        if let Some(hit) = find_first_in_tree(child, selector) {
            return Some(hit);
        }
    }
    None
}

/// Compute centroid in physical pixels.
fn centroid(rect: &Rect) -> (i32, i32) {
    (rect.x + rect.w / 2, rect.y + rect.h / 2)
}

/// Try `ValuePattern::SetValue` on the elements under `root` matching
/// `selectors`, tier by tier and within a tier in DFS pre-order; `true` when
/// one accepted the write. Tiers exist because tree order alone picks the
/// wrong control in dense windows — a file dialog's list view exposes dozens
/// of column-cell `Edit`s ahead of its filename field.
///
/// Meant to be called from inside a `UiaPoolHandle::run` closure — it takes
/// the pool's live `UIAutomation`. One `BuildUpdatedCache` pass for all
/// tiers, with the live-element relocation sourcing from that same cached
/// root (CR-02).
pub fn set_value_first_match(
    uia: &UIAutomation,
    root: &UIElement,
    selectors: &[Selector],
    value: &str,
) -> Result<bool, ProtoError> {
    let cached_root = build_cached_root(uia, root)?;
    let tree = walk_cached(&cached_root, u32::MAX)?;
    for selector in selectors {
        let mut nodes = Vec::new();
        crate::uia::find::collect_matching(&tree, selector, &mut nodes);
        for node in &nodes {
            let Ok(live) = relocate_live_element(uia, &cached_root, node) else {
                continue;
            };
            let Ok(pattern) = live.get_cached_pattern::<UIValuePattern>() else {
                continue;
            };
            if pattern.set_value(value).is_ok() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Handle `Request::ClickElement`: resolve element via UIA pool, delegate to
/// Phase 2 `input::click` on the input thread.
pub fn handle_click_element(
    uia_pool: &UiaPoolHandle,
    input: &Arc<InputThreadHandle>,
    selector: Selector,
    modifiers: Option<Vec<String>>,
) -> Result<Response, ProtoError> {
    let resolved = resolve_element(uia_pool, selector, /*focus*/ false, /*scroll*/ true)?;
    let (cx, cy) = centroid(&resolved.rect);
    let mods = modifiers.unwrap_or_default();
    input.run(move || ih::click(cx, cy, MouseButton::Left, 1, &mods, false))?;
    Ok(Response::Element { matched: true })
}

/// Handle `Request::TypeIntoElement`: resolve + focus, delegate to
/// Phase 2 `input::type_text`. Payload arrives wrapped in `Redact<String>`.
pub fn handle_type_into_element(
    uia_pool: &UiaPoolHandle,
    input: &Arc<InputThreadHandle>,
    selector: Selector,
    text: Redact<String>,
) -> Result<Response, ProtoError> {
    resolve_element(uia_pool, selector, /*focus*/ true, /*scroll*/ false)?;
    // Unwrap inside this scope only — never log it.
    let payload = text.into_inner();
    tracing::debug!(text_len = payload.len(), "type_into_element dispatch");
    input.run(move || ih::type_text(&payload))?;
    Ok(Response::Element { matched: true })
}

/// Handle `Request::ScrollIntoView`. ScrollItemPattern only for now;
/// ScrollPattern fallback can be added later.
pub fn handle_scroll_into_view(
    uia_pool: &UiaPoolHandle,
    selector: Selector,
) -> Result<Response, ProtoError> {
    let hwnd = foreground_hwnd().ok_or_else(|| {
        ProtoError::new(ErrorCode::WindowNotFound, "no foreground window".to_string())
    })?;
    let matched: bool = uia_pool.run(move |uia| {
        let root = get_or_fetch(uia, hwnd)?;
        // CR-02: single BuildUpdatedCache walk per call.
        let cached_root = build_cached_root(uia, &root)?;
        let tree = walk_cached(&cached_root, u32::MAX)?;
        let Some(target) = find_first_in_tree(&tree, &selector) else {
            return Err(ProtoError::new(
                ErrorCode::ElementNotFound,
                "selector matched no element".to_string(),
            ));
        };
        let live = relocate_live_element(uia, &cached_root, &target)?;
        if let Ok(p) = live.get_cached_pattern::<UIScrollItemPattern>() {
            p.scroll_into_view().map_err(|e| {
                ProtoError::new(ErrorCode::Internal, format!("scroll_into_view: {e}"))
            })?;
            Ok(true)
        } else {
            // WR-08: element WAS found; it just doesn't support
            // ScrollItemPattern. Use Response::Element { matched: false }
            // semantics — ElementNotFound is reserved for selector misses.
            Ok(false)
        }
    })?;
    Ok(Response::Element { matched })
}

// Suppress unused-pattern-import on win32 only.
#[allow(dead_code)]
fn _types_used(_: UIPatternType) {}
