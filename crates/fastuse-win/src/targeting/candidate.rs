//! `TargetCandidate` — a possible answer for a selector. Multiple candidates
//! per selector match are normal (UIA + OCR + geometry can overlap); the
//! strategy picker chooses the highest-scoring kind.

use fastuse_proto::coords::Rect;
use fastuse_proto::uia_node::ControlType;

/// Bitfield of UIA patterns this candidate exposes. Cached at resolution time
/// via `Is{Pattern}PatternAvailable` — checking this is cheap (cached COM read).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometrySource {
    /// Caller-provided absolute coords.
    CallerAbsolute,
    /// UIA-bounds projection on a candidate that exposed no usable patterns.
    UiaBoundsOnly,
}

/// One ranked candidate. Score is internal — exposed only via the chosen
/// strategy's wire-level `Strategy` enum.
#[derive(Debug, Clone)]
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
