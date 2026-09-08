//! `walk_subtree` — the canonical CacheRequest tree reader (Phase 3 Task 09,
//! UIA-02 keystone, UIA-12 enforcement target).
//!
//! Every UIA tree reader in fastuse-win MUST go through this function.
//! Only ONE `BuildUpdatedCache` call per invocation; every subsequent
//! property/child fetch reads from the cached snapshot via
//! `get_cached_*` accessors. **No `Current[A-Z]*` accessor is ever called**
//! — `cargo xtask check-cacherequest` (Task 19) blocks any regression at CI.

use fastuse_proto::{
    coords::Rect as ProtoRect, ControlType as ProtoControlType, Error as ProtoError, ErrorCode,
    Redact, TreeView, UIANode,
};
use uiautomation::patterns::{UIPatternType, UIValuePattern};
use uiautomation::types::{ControlType as UiaControlType, TreeScope, UIProperty};
use uiautomation::core::UICacheRequest;
use uiautomation::{UIAutomation, UIElement};

/// Properties bulk-fetched on every walk. Keep this list aligned with the
/// `UIANode` shape — every field on `UIANode` either comes from a property
/// here or is filled from a child walk.
fn properties_to_cache() -> &'static [UIProperty] {
    &[
        UIProperty::Name,
        UIProperty::AutomationId,
        UIProperty::ClassName,
        UIProperty::ControlType,
        UIProperty::LocalizedControlType,
        UIProperty::BoundingRectangle,
        UIProperty::IsEnabled,
        UIProperty::IsKeyboardFocusable,
        UIProperty::HelpText,
    ]
}

/// Build a `UICacheRequest` configured to bulk-fetch the property set used
/// by `walk_subtree`, plus the patterns needed by click_element /
/// type_into_element / scroll_into_view convenience tools.
pub fn build_subtree_cache_request(
    uia: &UIAutomation,
    view: TreeView,
) -> Result<UICacheRequest, ProtoError> {
    let req = uia
        .create_cache_request()
        .map_err(|e| internal(format!("create_cache_request: {e}")))?;
    for prop in properties_to_cache() {
        req.add_property(*prop)
            .map_err(|e| internal(format!("add_property({prop:?}): {e}")))?;
    }
    // Patterns we need from the cache for the action tools.
    for p in [
        UIPatternType::Invoke,
        UIPatternType::Value,
        UIPatternType::ScrollItem,
    ] {
        req.add_pattern(p)
            .map_err(|e| internal(format!("add_pattern({p:?}): {e}")))?;
    }
    // Tree scope — Subtree walks every descendant of the root in the cache
    // pass so subsequent get_cached_children() reads are zero-COM.
    req.set_tree_scope(TreeScope::Subtree)
        .map_err(|e| internal(format!("set_tree_scope: {e}")))?;
    // Tree filter — content view (default, skips chrome) or raw (true cond).
    // The Raw view IS the IUIAutomation::RawViewCondition which is just the
    // true condition (UIA returns every element). The uiautomation crate
    // surfaces no raw-view-condition helper; we use create_true_condition.
    let cond = match view {
        TreeView::Content => uia
            .get_content_view_condition()
            .map_err(|e| internal(format!("get_content_view_condition: {e}")))?,
        TreeView::Raw => uia
            .create_true_condition()
            .map_err(|e| internal(format!("create_true_condition: {e}")))?,
    };
    req.set_tree_filter(cond)
        .map_err(|e| internal(format!("set_tree_filter: {e}")))?;
    Ok(req)
}

/// Walk `root`'s subtree under the given view, bounded by `depth`.
///
/// **Calls `BuildUpdatedCache` exactly once** on the root. All subsequent
/// property and child reads source from the cached snapshot via
/// `get_cached_*` accessors (UIA-12). Returns the canonical `UIANode` wire
/// shape ready to ride back to the client.
pub fn walk_subtree(
    uia: &UIAutomation,
    root: &UIElement,
    view: TreeView,
    depth: Option<u32>,
) -> Result<UIANode, ProtoError> {
    let req = build_subtree_cache_request(uia, view)?;
    let cached_root = root
        .build_updated_cache(&req)
        .map_err(|e| internal(format!("build_updated_cache: {e}")))?;
    let max_depth = depth.unwrap_or(u32::MAX);
    walk_cached(&cached_root, max_depth)
}

/// Recurse over a *cache-populated* element, filling in `UIANode`. Never
/// goes back to COM — all reads are `get_cached_*`. Exposed `pub(crate)`
/// so callers that already have a `BuildUpdatedCache`-populated root
/// (e.g. `element_actions::resolve_element` after CR-02) can produce a
/// `UIANode` snapshot without re-walking.
pub(crate) fn walk_cached(el: &UIElement, depth_remaining: u32) -> Result<UIANode, ProtoError> {
    // WR-07: name/class/help_text/automation_id/localized are legitimately
    // empty for many controls — keep lossy. But bounding_rectangle and
    // control_type are essential identity properties: a real cache-fetch
    // failure here means the element went stale between BuildUpdatedCache
    // and read, and silently falling back to (0,0,0,0) makes click_element
    // click the desktop origin. Promote those two to ElementNotFound so
    // callers can distinguish "stale cache" from "anonymous control."
    let name = el.get_cached_name().unwrap_or_default();
    let automation_id = el.get_cached_automation_id().unwrap_or_default();
    let class_name = el.get_cached_classname().unwrap_or_default();
    let control_type = el.get_cached_control_type().map(map_control_type).map_err(|e| {
        ProtoError::new(
            ErrorCode::ElementNotFound,
            format!("get_cached_control_type (stale cache?): {e}"),
        )
    })?;
    let localized_control_type = el.get_cached_localized_control_type().unwrap_or_default();
    let help_text = el.get_cached_help_text().unwrap_or_default();
    let is_enabled = el.is_cached_enabled().unwrap_or(false);
    let is_keyboard_focusable = el.is_cached_keyboard_focusable().unwrap_or(false);
    let bounding_rect = el
        .get_cached_bounding_rectangle()
        .map(|r| ProtoRect {
            x: r.get_left(),
            y: r.get_top(),
            w: r.get_right() - r.get_left(),
            h: r.get_bottom() - r.get_top(),
        })
        .map_err(|e| {
            ProtoError::new(
                ErrorCode::ElementNotFound,
                format!("get_cached_bounding_rectangle (stale cache?): {e}"),
            )
        })?;

    // Value pattern (cached). Wrap in Redact so password-like fields never
    // leak through Debug/tracing (T-03-02).
    let value: Option<Redact<String>> = el
        .get_cached_pattern::<UIValuePattern>()
        .ok()
        .and_then(|p| p.get_cached_value().ok())
        .map(Redact::new);

    let mut children = Vec::new();
    if depth_remaining > 0 {
        if let Ok(kids) = el.get_cached_children() {
            children.reserve(kids.len());
            for child in &kids {
                children.push(walk_cached(child, depth_remaining - 1)?);
            }
        }
    }

    Ok(UIANode {
        name,
        automation_id,
        class_name,
        control_type,
        localized_control_type,
        bounding_rect,
        is_enabled,
        is_keyboard_focusable,
        help_text,
        value,
        children,
    })
}

/// Map the uiautomation crate's `ControlType` enum to our protocol's closed
/// subset. Anything outside the closed set falls through to `Custom`.
pub(crate) fn map_control_type(ct: UiaControlType) -> ProtoControlType {
    match ct {
        UiaControlType::Button => ProtoControlType::Button,
        UiaControlType::Edit => ProtoControlType::Edit,
        UiaControlType::Text => ProtoControlType::Text,
        UiaControlType::ComboBox => ProtoControlType::ComboBox,
        UiaControlType::List => ProtoControlType::List,
        UiaControlType::ListItem => ProtoControlType::ListItem,
        UiaControlType::MenuItem => ProtoControlType::MenuItem,
        UiaControlType::Tab => ProtoControlType::Tab,
        UiaControlType::TabItem => ProtoControlType::TabItem,
        UiaControlType::Hyperlink => ProtoControlType::Hyperlink,
        UiaControlType::Window => ProtoControlType::Window,
        UiaControlType::Pane => ProtoControlType::Pane,
        UiaControlType::Group => ProtoControlType::Group,
        UiaControlType::CheckBox => ProtoControlType::CheckBox,
        UiaControlType::RadioButton => ProtoControlType::RadioButton,
        _ => ProtoControlType::Custom,
    }
}

fn internal(msg: String) -> ProtoError {
    ProtoError::new(ErrorCode::Internal, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::ControlType;

    #[test]
    fn map_control_type_round_trips_known() {
        assert_eq!(map_control_type(UiaControlType::Button), ControlType::Button);
        assert_eq!(map_control_type(UiaControlType::Edit), ControlType::Edit);
        assert_eq!(
            map_control_type(UiaControlType::CheckBox),
            ControlType::CheckBox
        );
    }

    #[test]
    fn map_control_type_unknown_falls_through_to_custom() {
        // ToolTip is not in our closed proto set.
        assert_eq!(
            map_control_type(UiaControlType::ToolTip),
            ControlType::Custom
        );
    }
}
