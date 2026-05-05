//! `TargetCandidate` — a possible answer for a selector. Multiple candidates
//! per selector match are normal (UIA + OCR + geometry can overlap); the
//! strategy picker chooses the highest-scoring kind.

use std::sync::Arc;

use fastuse_proto::coords::Rect;
use fastuse_proto::uia_node::ControlType;
use fastuse_proto::Selector;
use serde::{Deserialize, Serialize};

use crate::uia_pool::UiaPoolHandle;

/// Bitfield of UIA patterns this candidate exposes. Cached at resolution time
/// via `Is{Pattern}PatternAvailable` — checking this is cheap (cached COM read).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternSet {
    /// `IUIAutomationInvokePattern`.
    pub invoke: bool,
    /// `IUIAutomationTogglePattern`.
    pub toggle: bool,
    /// `IUIAutomationSelectionItemPattern`.
    pub selection_item: bool,
    /// `IUIAutomationExpandCollapsePattern`.
    pub expand_collapse: bool,
    /// `IUIAutomationValuePattern`.
    pub value: bool,
    /// `IUIAutomationLegacyIAccessiblePattern` (DoDefaultAction lives here).
    pub legacy_iaccessible: bool,
    /// `IUIAutomationScrollItemPattern`.
    pub scroll_item: bool,
}

/// How a Geometry candidate's rect was sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeometrySource {
    /// Caller-provided absolute coords.
    CallerAbsolute,
    /// UIA-bounds projection on a candidate that exposed no usable patterns.
    UiaBoundsOnly,
}

/// One ranked candidate. Score is internal — exposed only via the chosen
/// strategy's wire-level `Strategy` enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TargetCandidate {
    /// Resolved via UIA. `runtime_id` is opaque bytes from `GetRuntimeId()`.
    Uia {
        /// Stable identity within the tree's lifetime.
        runtime_id: Vec<i32>,
        /// Bounding rect in physical pixels (virtual-desktop origin).
        bounds: Rect,
        /// ControlType at resolve time.
        control_type: ControlType,
        /// Patterns the element exposes.
        patterns: PatternSet,
        /// `IsEnabled` at resolve time.
        is_enabled: bool,
        /// `IsOffscreen` at resolve time.
        is_offscreen: bool,
        /// Confidence score [0.0, 1.0].
        score: f32,
    },
    /// Resolved via OCR text match.
    Ocr {
        /// Matched text.
        text: String,
        /// Bounding rect in physical pixels.
        bounds: Rect,
        /// Confidence score [0.0, 1.0].
        score: f32,
    },
    /// Caller-provided geometry, or UIA bounds without usable patterns.
    Geometry {
        /// Rect.
        bounds: Rect,
        /// How we got the rect.
        source: GeometrySource,
        /// Confidence score [0.0, 1.0].
        score: f32,
    },
}

impl TargetCandidate {
    /// Score accessor for sort ordering.
    pub fn score(&self) -> f32 {
        match self {
            Self::Uia { score, .. } | Self::Ocr { score, .. } | Self::Geometry { score, .. } => {
                *score
            }
        }
    }
}

/// Resolution failure modes.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Underlying COM call failed.
    #[error("uia call failed: {0}")]
    Uia(String),
    /// Selector grammar rejected.
    #[error("selector unsupported: {0}")]
    Selector(String),
}

/// Resolve all UIA candidates matching `selector` rooted at `hwnd`. Builds a
/// CacheRequest fetching ControlType, BoundingRectangle, RuntimeId, IsEnabled,
/// IsOffscreen, and every `Is{Pattern}PatternAvailable` property in one COM
/// round-trip (per UIA-12 / CacheRequest invariant). Returns an empty Vec on
/// any failure — callers fall through to OCR / geometry escalation.
pub fn resolve_candidates(
    hwnd: u64,
    selector: &Selector,
    uia: &Arc<UiaPoolHandle>,
) -> Vec<TargetCandidate> {
    let sel = selector.clone();
    uia.run(move |automation| Ok(resolve_on_uia_thread(automation, hwnd, &sel)))
        .unwrap_or_default()
}

/// Worker-thread side of `resolve_candidates`. All `windows::*` and
/// `uiautomation::*` calls happen here, on the UIA pool's MTA thread.
pub(crate) fn resolve_on_uia_thread(
    automation: &uiautomation::UIAutomation,
    hwnd: u64,
    selector: &Selector,
) -> Vec<TargetCandidate> {
    use uiautomation::types::{Handle, TreeScope, UIProperty};
    use windows::Win32::Foundation::HWND;

    // Look up the top-level UIElement for this HWND.
    let h = HWND(hwnd as *mut core::ffi::c_void);
    let root = match automation.element_from_handle(Handle::from(h)) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    // Build an augmented CacheRequest with bounds, identity, enabled/offscreen,
    // and pattern-availability properties. Single BuildUpdatedCache call below.
    let req = match automation.create_cache_request() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let props = [
        UIProperty::Name,
        UIProperty::AutomationId,
        UIProperty::ClassName,
        UIProperty::ControlType,
        UIProperty::LocalizedControlType,
        UIProperty::BoundingRectangle,
        UIProperty::IsEnabled,
        UIProperty::IsOffscreen,
        UIProperty::HelpText,
        UIProperty::IsKeyboardFocusable,
        UIProperty::IsInvokePatternAvailable,
        UIProperty::IsTogglePatternAvailable,
        UIProperty::IsSelectionItemPatternAvailable,
        UIProperty::IsExpandCollapsePatternAvailable,
        UIProperty::IsValuePatternAvailable,
        UIProperty::IsLegacyIAccessiblePatternAvailable,
        UIProperty::IsScrollItemPatternAvailable,
    ];
    for p in props {
        if req.add_property(p).is_err() {
            return Vec::new();
        }
    }
    if req.set_tree_scope(TreeScope::Subtree).is_err() {
        return Vec::new();
    }
    let cond = match automation.create_true_condition() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    if req.set_tree_filter(cond).is_err() {
        return Vec::new();
    }

    let cached_root = match root.build_updated_cache(&req) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    collect_candidates(&cached_root, selector, &mut out);

    // Score: rank-based descending. Ties broken by visit order.
    for (i, c) in out.iter_mut().enumerate() {
        let s = (1.0_f32 - (i as f32) * 0.05).max(0.0);
        if let TargetCandidate::Uia { score, .. } = c {
            *score = s;
        }
    }
    out
}

fn collect_candidates(
    el: &uiautomation::UIElement,
    selector: &Selector,
    out: &mut Vec<TargetCandidate>,
) {
    if selector_matches_element(el, selector) {
        if let Some(c) = candidate_from_element(el) {
            out.push(c);
        }
    }
    if let Ok(kids) = el.get_cached_children() {
        for k in &kids {
            collect_candidates(k, selector, out);
        }
    }
}

fn selector_matches_element(el: &uiautomation::UIElement, selector: &Selector) -> bool {
    match selector {
        Selector::ByName(s) => el.get_cached_name().unwrap_or_default() == *s,
        Selector::ByAutomationId(s) => el.get_cached_automation_id().unwrap_or_default() == *s,
        Selector::ByClass(s) => el.get_cached_classname().unwrap_or_default() == *s,
        Selector::ByControlType(ct) => {
            let cached = match el.get_cached_control_type() {
                Ok(c) => c,
                Err(_) => return false,
            };
            map_uia_control_type(cached) == *ct
        }
        Selector::And(xs) => xs.iter().all(|s| selector_matches_element(el, s)),
        Selector::Or(xs) => xs.iter().any(|s| selector_matches_element(el, s)),
        Selector::Not(inner) => !selector_matches_element(el, inner),
    }
}

fn candidate_from_element(el: &uiautomation::UIElement) -> Option<TargetCandidate> {
    use uiautomation::types::UIProperty;

    let control_type = el
        .get_cached_control_type()
        .ok()
        .map(map_uia_control_type)
        .unwrap_or(ControlType::Custom);
    let rect = el.get_cached_bounding_rectangle().ok()?;
    let bounds = Rect {
        x: rect.get_left(),
        y: rect.get_top(),
        w: (rect.get_right() - rect.get_left()).max(0),
        h: (rect.get_bottom() - rect.get_top()).max(0),
    };
    let is_enabled = el.is_cached_enabled().unwrap_or(false);
    let is_offscreen = el.is_cached_offscreen().unwrap_or(false);
    let runtime_id = el.get_runtime_id().unwrap_or_default();

    let read_bool = |p: UIProperty| -> bool {
        el.get_cached_property_value(p)
            .ok()
            .and_then(|v| TryInto::<bool>::try_into(v).ok())
            .unwrap_or(false)
    };
    let patterns = PatternSet {
        invoke: read_bool(UIProperty::IsInvokePatternAvailable),
        toggle: read_bool(UIProperty::IsTogglePatternAvailable),
        selection_item: read_bool(UIProperty::IsSelectionItemPatternAvailable),
        expand_collapse: read_bool(UIProperty::IsExpandCollapsePatternAvailable),
        value: read_bool(UIProperty::IsValuePatternAvailable),
        legacy_iaccessible: read_bool(UIProperty::IsLegacyIAccessiblePatternAvailable),
        scroll_item: read_bool(UIProperty::IsScrollItemPatternAvailable),
    };

    Some(TargetCandidate::Uia {
        runtime_id,
        bounds,
        control_type,
        patterns,
        is_enabled,
        is_offscreen,
        score: 1.0,
    })
}

/// Local copy of the proto control-type mapper used by the walker. Kept in
/// sync with `crate::uia::walk::map_control_type`.
fn map_uia_control_type(ct: uiautomation::types::ControlType) -> ControlType {
    use uiautomation::types::ControlType as U;
    match ct {
        U::Button => ControlType::Button,
        U::Edit => ControlType::Edit,
        U::Text => ControlType::Text,
        U::ComboBox => ControlType::ComboBox,
        U::List => ControlType::List,
        U::ListItem => ControlType::ListItem,
        U::MenuItem => ControlType::MenuItem,
        U::Tab => ControlType::Tab,
        U::TabItem => ControlType::TabItem,
        U::Hyperlink => ControlType::Hyperlink,
        U::Window => ControlType::Window,
        U::Pane => ControlType::Pane,
        U::Group => ControlType::Group,
        U::CheckBox => ControlType::CheckBox,
        U::RadioButton => ControlType::RadioButton,
        _ => ControlType::Custom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires live Notepad window — run manually"]
    fn resolve_notepad_edit_field() {
        // Manual smoke: launch notepad, find the foreground HWND, call
        // `resolve_candidates` with `Selector::ByControlType(Edit)`. Expect
        // at least one match; the edit field should expose the Value pattern.
        use crate::uia::automation::foreground_hwnd;
        use crate::uia_pool::spawn_uia_pool;

        let pool = Arc::new(spawn_uia_pool(2).expect("spawn uia pool"));
        let hwnd = foreground_hwnd().expect("a foreground window");
        let sel = Selector::ByControlType(ControlType::Edit);
        let cands = resolve_candidates(hwnd, &sel, &pool);
        assert!(!cands.is_empty(), "expected at least one Edit candidate");
        let any_value = cands.iter().any(|c| matches!(
            c,
            TargetCandidate::Uia { patterns, .. } if patterns.value
        ));
        assert!(any_value, "expected at least one candidate with ValuePattern");
    }
}
