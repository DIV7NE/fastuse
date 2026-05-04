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
use uiautomation::patterns::{UIPatternType, UIScrollItemPattern};
use uiautomation::{UIAutomation, UIElement};

use crate::input::handlers as ih;
use crate::input_thread::InputThreadHandle;
use crate::uia::automation::foreground_hwnd;
use crate::uia::cache::get_or_fetch;
use crate::uia::walk::walk_subtree;
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
        let cached_root = walk_uia_subtree_for_match(uia, &root)?;
        // walk_subtree above populates the cached snapshot so we can pick
        // the element from the resulting UIANode tree, then re-resolve the
        // live UIElement to call set_focus / scroll. We do that by walking
        // children of the live root via get_cached_children() and matching
        // by automation_id+name+control_type.
        let tree = walk_subtree(uia, &root, fastuse_proto::TreeView::Content, None)?;
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

/// Just walk; primary purpose is to ensure we have a fresh cache populated.
fn walk_uia_subtree_for_match(
    uia: &UIAutomation,
    root: &UIElement,
) -> Result<UIElement, ProtoError> {
    let req = crate::uia::walk::build_subtree_cache_request(uia, fastuse_proto::TreeView::Content)?;
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
        let _ = walk_uia_subtree_for_match(uia, &root)?;
        let tree = walk_subtree(uia, &root, fastuse_proto::TreeView::Content, None)?;
        let Some(target) = find_first_in_tree(&tree, &selector) else {
            return Err(ProtoError::new(
                ErrorCode::ElementNotFound,
                "selector matched no element".to_string(),
            ));
        };
        let live = relocate_live_element(uia, &root, &target)?;
        if let Ok(p) = live.get_cached_pattern::<UIScrollItemPattern>() {
            p.scroll_into_view().map_err(|e| {
                ProtoError::new(ErrorCode::Internal, format!("scroll_into_view: {e}"))
            })?;
            Ok(true)
        } else {
            Err(ProtoError::new(
                ErrorCode::ElementNotFound,
                "element does not support ScrollItemPattern".to_string(),
            )
            .with_hint("v2: fall back to parent ScrollPattern + SetScrollPercent".to_string()))
        }
    })?;
    Ok(Response::Element { matched })
}

// Suppress unused-pattern-import on win32 only.
#[allow(dead_code)]
fn _types_used(_: UIPatternType) {}
