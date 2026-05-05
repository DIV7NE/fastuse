//! Orchestrate the full pipeline: profile → strategy → hit-test → execute →
//! verify, with candidate-kind-dependent escalation. Max 2 verified
//! attempts after the first; structurally-no-op tiers don't count.

use std::sync::Arc;

use fastuse_proto::error::{Error, ErrorCode};
use fastuse_proto::selector::Selector;
use fastuse_proto::wire::{
    ActionOpts, EscalatePolicy, Response, Strategy, VerificationEvidence,
};

use crate::capture_thread::CaptureThreadHandle;
use crate::input_thread::InputThreadHandle;
use crate::ocr_thread::OcrThreadHandle;
use crate::targeting::candidate::TargetCandidate;
use crate::targeting::hit_test::{verify_hit, HitTestVerdict};
use crate::targeting::profile::profile_window_for_selector;
use crate::targeting::strategy::{pick_strategy, Intent};
use crate::uia_pool::UiaPoolHandle;

/// What the dispatcher passes in.
pub struct TargetedRequest<'a> {
    /// Selector to resolve.
    pub selector: &'a Selector,
    /// Modifier chord during click (e.g. ctrl+click).
    pub modifiers: Option<&'a [String]>,
    /// Caller post-action expectations.
    pub opts: Option<&'a ActionOpts>,
    /// Click vs Type intent — drives strategy picker.
    pub intent: Intent,
    /// Optional Type payload (only used when `intent == Type`).
    pub typed_text: Option<&'a fastuse_proto::redact::Redact<String>>,
    /// Resolved root HWND (None = foreground).
    pub root_hwnd: Option<u64>,
    /// UIA pool handle.
    pub uia: &'a Arc<UiaPoolHandle>,
    /// Input thread handle.
    pub input: &'a Arc<InputThreadHandle>,
    /// Capture thread handle (for screenshot_after).
    pub capture: Option<&'a Arc<CaptureThreadHandle>>,
    /// OCR thread handle (used when escalation reaches OCR tier).
    pub ocr: Option<&'a Arc<OcrThreadHandle>>,
}

/// Run the full pipeline. Returns `Response::ActionResult` on completion or
/// `Response::Error` on resolution failure.
pub async fn execute_targeted<'a>(req: TargetedRequest<'a>) -> Response {
    let policy = req
        .opts
        .and_then(|o| o.escalate)
        .unwrap_or(EscalatePolicy::Auto);

    let hwnd = match req.root_hwnd {
        Some(h) => h,
        None => match resolve_foreground(req.uia) {
            Some(h) => h,
            None => {
                return Response::Error(Error::new(
                    ErrorCode::WindowNotFound,
                    "no foreground window",
                ));
            }
        },
    };

    let profile = match profile_window_for_selector(hwnd, req.selector, req.uia) {
        Ok(p) => p,
        Err(e) => {
            return Response::Error(Error::new(
                ErrorCode::Internal,
                format!("profile failed: {e}"),
            ));
        }
    };

    let candidate = match profile.candidates.into_iter().next() {
        Some(c) => c,
        None => {
            return Response::Error(Error::new(
                ErrorCode::ElementNotFound,
                "no candidate matched selector",
            ));
        }
    };

    // Build the ladder for this candidate kind.
    let ladder = build_ladder(&candidate, req.intent);

    let mut attempts_remaining: u8 = if policy == EscalatePolicy::Strict { 0 } else { 2 };
    let mut last_strategy = ladder[0];

    for (i, strategy) in ladder.iter().enumerate() {
        last_strategy = *strategy;

        // Hit-test gate before any coordinate strategy.
        if is_coord_strategy(*strategy) {
            let verdict = hit_test_for_candidate(&candidate, *strategy, req.uia);
            if verdict == HitTestVerdict::Mismatch {
                // Don't count this as a "verified attempt" — we never clicked.
                continue;
            }
        }

        let outcome = run_strategy(*strategy, &candidate, &req).await;

        if outcome.verified {
            return Response::ActionResult {
                verified: true,
                evidence: outcome.evidence,
                strategy_used: *strategy,
                waited_ms: outcome.waited_ms,
                screenshot: outcome.screenshot,
            };
        }

        // Verified attempt that didn't match. Decrement budget.
        if i > 0 {
            if attempts_remaining == 0 {
                break;
            }
            attempts_remaining -= 1;
        }
    }

    // Exhausted ladder without verification. Honest failure.
    Response::ActionResult {
        verified: false,
        evidence: VerificationEvidence::Unverified,
        strategy_used: last_strategy,
        waited_ms: None,
        screenshot: None,
    }
}

fn build_ladder(candidate: &TargetCandidate, intent: Intent) -> Vec<Strategy> {
    let first = pick_strategy(candidate, intent);
    match candidate {
        TargetCandidate::Uia { patterns, .. } => {
            let any_pattern = patterns.invoke
                || patterns.toggle
                || patterns.selection_item
                || patterns.expand_collapse
                || patterns.value;
            if any_pattern {
                // 3 tiers: pattern → BoundsClickUia → BoundsClickOcr (re-resolve)
                vec![first, Strategy::BoundsClickUia, Strategy::BoundsClickOcr]
            } else {
                // 2 tiers: BoundsClickUia → BoundsClickOcr
                vec![Strategy::BoundsClickUia, Strategy::BoundsClickOcr]
            }
        }
        TargetCandidate::Geometry { .. } => {
            vec![Strategy::BoundsClickGeometry, Strategy::BoundsClickOcr]
        }
        TargetCandidate::Ocr { .. } => vec![Strategy::BoundsClickOcr],
    }
}

fn is_coord_strategy(s: Strategy) -> bool {
    matches!(
        s,
        Strategy::BoundsClickUia | Strategy::BoundsClickOcr | Strategy::BoundsClickGeometry,
    )
}

fn hit_test_for_candidate(
    candidate: &TargetCandidate,
    _strategy: Strategy,
    uia: &Arc<UiaPoolHandle>,
) -> HitTestVerdict {
    match candidate {
        TargetCandidate::Uia {
            runtime_id, bounds, ..
        } => {
            let center = (bounds.x + bounds.w / 2, bounds.y + bounds.h / 2);
            verify_hit(center, Some(runtime_id), Some(*bounds), uia)
        }
        TargetCandidate::Ocr { bounds, .. } | TargetCandidate::Geometry { bounds, .. } => {
            let center = (bounds.x + bounds.w / 2, bounds.y + bounds.h / 2);
            verify_hit(center, None, Some(*bounds), uia)
        }
    }
}

#[derive(Debug, Clone)]
struct StrategyOutcome {
    verified: bool,
    evidence: VerificationEvidence,
    waited_ms: Option<u32>,
    screenshot: Option<Box<fastuse_proto::wire::ScreenshotPayload>>,
}

async fn run_strategy<'a>(
    strategy: Strategy,
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
) -> StrategyOutcome {
    // Implementer: each strategy variant maps to:
    //  - UiaInvoke / UiaToggle / UiaSelect / UiaExpandCollapse / UiaSetValue
    //    -> dispatch to uia_pool, call the matching pattern, then poll the
    //    matching postcondition (Task 7 helpers).
    //  - BoundsClick* -> compute DPI-correct center of candidate.bounds,
    //    use the existing Phase-2 input::click via req.input, then verify
    //    via subtree-mutation snapshot/diff (UIA case) or caller wait_for
    //    only (OCR case).
    //
    // Each branch returns StrategyOutcome with the right evidence variant.

    let _ = (strategy, candidate, req);
    StrategyOutcome {
        verified: false,
        evidence: VerificationEvidence::Unverified,
        waited_ms: None,
        screenshot: None,
    }
}

fn resolve_foreground(_uia: &Arc<UiaPoolHandle>) -> Option<u64> {
    // Implementer: GetForegroundWindow() — already exposed in fastuse_win
    // window module. Cast to u64.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targeting::candidate::{GeometrySource, PatternSet};
    use fastuse_proto::coords::Rect;
    use fastuse_proto::uia_node::ControlType;

    fn uia(patterns: PatternSet) -> TargetCandidate {
        TargetCandidate::Uia {
            runtime_id: vec![1, 2, 3],
            bounds: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            control_type: ControlType::Button,
            patterns,
            is_enabled: true,
            is_offscreen: false,
            score: 1.0,
        }
    }

    #[test]
    fn ladder_for_uia_with_patterns_has_3_tiers() {
        let c = uia(PatternSet {
            invoke: true,
            ..Default::default()
        });
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], Strategy::UiaInvoke);
        assert_eq!(l[1], Strategy::BoundsClickUia);
        assert_eq!(l[2], Strategy::BoundsClickOcr);
    }

    #[test]
    fn ladder_for_uia_no_patterns_has_2_tiers() {
        let c = uia(PatternSet::default());
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], Strategy::BoundsClickUia);
    }

    #[test]
    fn ladder_for_geometry_has_2_tiers() {
        let c = TargetCandidate::Geometry {
            bounds: Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            source: GeometrySource::CallerAbsolute,
            score: 1.0,
        };
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], Strategy::BoundsClickGeometry);
    }

    #[test]
    fn ladder_for_ocr_has_1_tier() {
        let c = TargetCandidate::Ocr {
            text: fastuse_proto::redact::Redact::new("x".to_string()),
            bounds: Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            score: 0.8,
        };
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0], Strategy::BoundsClickOcr);
    }
}
