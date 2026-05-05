//! Pattern-availability dominates ControlType. Given a candidate, pick the
//! one strategy to attempt first. ControlType is informational only — used
//! for breaking ties between equally-supported patterns and for SetValue
//! intent inference.

use fastuse_proto::wire::Strategy;

use crate::targeting::candidate::{GeometrySource, PatternSet, TargetCandidate};

/// Caller intent. Click vs type maps to different first-choice patterns
/// even on the same element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Treat as click — Invoke / Toggle / Select / ExpandCollapse fit.
    Click,
    /// Treat as type-into — SetValue fits.
    Type,
}

/// Pick the first-choice strategy for `candidate` given `intent`. Returns the
/// `Strategy` that should be tried in tier 1 of the escalation ladder.
///
/// For Type intent: SetValue if available; else falls back to BoundsClick on
/// the element so caller-provided text injection by Phase-2 input::type_text
/// can run after focus.
pub fn pick_strategy(candidate: &TargetCandidate, intent: Intent) -> Strategy {
    match candidate {
        TargetCandidate::Uia { patterns, is_enabled, .. } if *is_enabled => {
            pick_uia_strategy(*patterns, intent)
        }
        TargetCandidate::Uia { .. } => {
            // Disabled element — falls through to BoundsClick which will
            // then fail the hit-test or postcondition and surface honestly.
            Strategy::BoundsClickUia
        }
        TargetCandidate::Ocr { .. } => Strategy::BoundsClickOcr,
        TargetCandidate::Geometry { source, .. } => match source {
            GeometrySource::CallerAbsolute => Strategy::BoundsClickGeometry,
            GeometrySource::UiaBoundsOnly => Strategy::BoundsClickUia,
        },
    }
}

fn pick_uia_strategy(p: PatternSet, intent: Intent) -> Strategy {
    match intent {
        Intent::Type => {
            if p.value {
                return Strategy::UiaSetValue;
            }
            // Fall through to BoundsClick so caller can inject keystrokes
            // after focus. The set_focus + Phase-2 type_text path handles this.
            Strategy::BoundsClickUia
        }
        Intent::Click => {
            // Order matters when multiple patterns are exposed. Invoke wins
            // for plain buttons; Toggle wins for checkbox-shaped controls
            // even if Invoke is also available, because the postcondition
            // is sharper (ToggleState read).
            if p.toggle {
                return Strategy::UiaToggle;
            }
            if p.selection_item {
                return Strategy::UiaSelect;
            }
            if p.expand_collapse {
                return Strategy::UiaExpandCollapse;
            }
            if p.invoke {
                return Strategy::UiaInvoke;
            }
            // LegacyIAccessible.DoDefaultAction deferred to v1.1 per spec.
            Strategy::BoundsClickUia
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::coords::Rect;
    use fastuse_proto::uia_node::ControlType;

    fn uia(p: PatternSet, is_enabled: bool) -> TargetCandidate {
        TargetCandidate::Uia {
            runtime_id: vec![1, 2, 3],
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            control_type: ControlType::Button,
            patterns: p,
            is_enabled,
            is_offscreen: false,
            score: 1.0,
        }
    }

    #[test]
    fn invoke_only_picks_invoke() {
        let c = uia(PatternSet { invoke: true, ..Default::default() }, true);
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::UiaInvoke);
    }

    #[test]
    fn toggle_outranks_invoke() {
        let c = uia(
            PatternSet { invoke: true, toggle: true, ..Default::default() },
            true,
        );
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::UiaToggle);
    }

    #[test]
    fn type_intent_prefers_value() {
        let c = uia(PatternSet { value: true, ..Default::default() }, true);
        assert_eq!(pick_strategy(&c, Intent::Type), Strategy::UiaSetValue);
    }

    #[test]
    fn no_patterns_falls_to_bounds() {
        let c = uia(PatternSet::default(), true);
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickUia);
    }

    #[test]
    fn disabled_falls_to_bounds() {
        let c = uia(PatternSet { invoke: true, ..Default::default() }, false);
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickUia);
    }

    #[test]
    fn ocr_candidate_picks_ocr_bounds() {
        let c = TargetCandidate::Ocr {
            text: fastuse_proto::redact::Redact::new("Settings".to_string()),
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            score: 0.9,
        };
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickOcr);
    }

    #[test]
    fn geometry_caller_picks_geometry() {
        let c = TargetCandidate::Geometry {
            bounds: Rect { x: 5, y: 5, w: 1, h: 1 },
            source: GeometrySource::CallerAbsolute,
            score: 1.0,
        };
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickGeometry);
    }
}
