//! Orchestrate the full pipeline: profile → strategy → hit-test → execute →
//! verify, with candidate-kind-dependent escalation. Max 2 verified
//! attempts after the first; structurally-no-op tiers don't count.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastuse_proto::coords::{MouseButton, Rect};
use fastuse_proto::error::{Error, ErrorCode};
use fastuse_proto::selector::Selector;
use fastuse_proto::wire::{
    ActionOpts, EscalatePolicy, ExpectClause, RegionSpec, Response, ScreenshotPayload, Strategy,
    VerificationEvidence,
};

use crate::capture::{handle_screenshot, handle_screenshot_region};
use crate::capture_thread::CaptureThreadHandle;
use crate::input_thread::InputThreadHandle;
use crate::ocr_thread::OcrThreadHandle;
use crate::targeting::candidate::TargetCandidate;
use crate::targeting::hit_test::{verify_hit, HitTestVerdict};
use crate::targeting::profile::profile_window_for_selector;
use crate::targeting::strategy::{pick_strategy, Intent};
use crate::targeting::verify::{
    poll_is_selected, poll_toggle_state, poll_until, poll_value_contains, snapshot_subtree,
    subtree_mutated,
};
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

    let profile = match profile_window_for_selector(
        hwnd,
        req.selector,
        req.uia,
        req.capture,
        req.ocr,
    )
    .await
    {
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
    let cap_ms = req.opts.and_then(|o| o.wait_timeout_ms);
    let started = Instant::now();

    let (verified_postcondition, evidence, waited_ms) = match strategy {
        Strategy::UiaInvoke => run_uia_invoke(candidate, req, cap_ms).await,
        Strategy::UiaToggle => run_uia_toggle(candidate, req, cap_ms).await,
        Strategy::UiaSelect => run_uia_select(candidate, req, cap_ms).await,
        Strategy::UiaExpandCollapse => run_uia_expand_collapse(candidate, req, cap_ms).await,
        Strategy::UiaSetValue => run_uia_set_value(candidate, req, cap_ms).await,
        Strategy::BoundsClickUia => run_bounds_click_uia(candidate, req, cap_ms).await,
        Strategy::BoundsClickOcr => run_bounds_click_ocr(candidate, req).await,
        Strategy::BoundsClickGeometry => run_bounds_click_geometry(candidate, req).await,
    };

    // Honor caller's expect/wait_for. We treat expect as an additional
    // required match: verified iff strategy_postcondition AND expect both
    // matched. wait_for is non-failing — promotes evidence to WaitForMatched
    // when matched but never demotes verification.
    let mut verified = verified_postcondition;
    let mut evidence = evidence;
    let mut waited_total = waited_ms;

    if let Some(opts) = req.opts {
        if let Some(sel) = opts.wait_for.as_ref() {
            let (matched, w) = poll_selector(req.uia, sel.clone(), cap_ms).await;
            waited_total = waited_total.map(|x| x.saturating_add(w)).or(Some(w));
            if matched {
                evidence = VerificationEvidence::WaitForMatched;
                if !verified {
                    verified = true;
                }
            }
        }
        if let Some(exp) = opts.expect.as_ref() {
            let (matched, w) = poll_expect(req.uia, exp, cap_ms).await;
            waited_total = waited_total.map(|x| x.saturating_add(w)).or(Some(w));
            if matched {
                if evidence == VerificationEvidence::Unverified
                    || evidence == VerificationEvidence::HitTestOnly
                {
                    evidence = VerificationEvidence::WaitForMatched;
                }
                // expect matched — count as verification (caller asked for
                // this signal). Stronger postcondition evidence is preserved
                // above by not overwriting PostconditionMet.
                verified = true;
            } else {
                // expect was specified and didn't match → strategy failed.
                verified = false;
            }
        }
    }

    let _ = started;

    let screenshot = capture_screenshot_after(req);

    StrategyOutcome {
        verified,
        evidence,
        waited_ms: waited_total,
        screenshot,
    }
}

// ---------- per-strategy actuators ----------

async fn run_uia_invoke<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
    cap_ms: Option<u32>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let (rid, _bounds) = match uia_runtime_id_and_bounds(candidate) {
        Some(p) => p,
        None => return (false, VerificationEvidence::Unverified, None),
    };

    // Pre-snapshot parent's children. Resolve parent on the UIA thread.
    let parent_rid = parent_runtime_id(req.uia, rid.clone());
    let pre_snapshot = parent_rid
        .as_ref()
        .and_then(|prid| snapshot_subtree(prid, req.uia));

    // Dispatch the Invoke call on the UIA pool.
    let rid_for_call = rid.clone();
    let invoke_result: Result<bool, Error> = req.uia.run(move |automation| {
        let el = match find_element_by_runtime_id(
            automation,
            &rid_for_call,
            Some(uiautomation::patterns::UIPatternType::Invoke),
        ) {
            Some(e) => e,
            None => return Ok(false),
        };
        let pat = match el.get_pattern::<uiautomation::patterns::UIInvokePattern>() {
            Ok(p) => p,
            Err(_) => return Ok(false),
        };
        match pat.invoke() {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    });
    if !invoke_result.unwrap_or(false) {
        return (false, VerificationEvidence::Unverified, None);
    }

    // Postcondition: subtree mutated (parent's children RuntimeId set,
    // foreground HWND, or focused element changed).
    if let (Some(prid), Some(pre)) = (parent_rid, pre_snapshot) {
        let prid_owned = prid.clone();
        let uia_handle = req.uia.clone();
        let outcome = poll_until(cap_ms, move || {
            match snapshot_subtree(&prid_owned, &uia_handle) {
                Some(post) => subtree_mutated(&pre, &post),
                None => true, // parent vanished — strong mutation signal
            }
        })
        .await;
        if outcome.matched {
            return (
                true,
                VerificationEvidence::PostconditionMet,
                Some(outcome.waited_ms),
            );
        }
        return (false, VerificationEvidence::Unverified, Some(outcome.waited_ms));
    }
    (false, VerificationEvidence::Unverified, None)
}

async fn run_uia_toggle<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
    cap_ms: Option<u32>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let (rid, _) = match uia_runtime_id_and_bounds(candidate) {
        Some(p) => p,
        None => return (false, VerificationEvidence::Unverified, None),
    };

    // Read current toggle state (so we can expect the OTHER value after).
    let rid_for_pre = rid.clone();
    let pre_state: Option<i32> = req
        .uia
        .run(move |a| Ok(read_toggle_state_inline(a, &rid_for_pre)))
        .ok()
        .flatten();

    // Dispatch Toggle.
    let rid_for_call = rid.clone();
    let toggled: Result<bool, Error> = req.uia.run(move |automation| {
        let el = match find_element_by_runtime_id(
            automation,
            &rid_for_call,
            Some(uiautomation::patterns::UIPatternType::Toggle),
        ) {
            Some(e) => e,
            None => return Ok(false),
        };
        let pat = match el.get_pattern::<uiautomation::patterns::UITogglePattern>() {
            Ok(p) => p,
            Err(_) => return Ok(false),
        };
        Ok(pat.toggle().is_ok())
    });
    if !toggled.unwrap_or(false) {
        return (false, VerificationEvidence::Unverified, None);
    }

    // Expected after = opposite of pre. ToggleState: 0=Off, 1=On, 2=Indeterminate.
    let expected_after = match pre_state {
        Some(0) => 1,
        Some(1) => 0,
        // Without a clean pre-read, fall back to "any change" by trying both.
        _ => -1,
    };
    if expected_after >= 0 {
        let outcome = poll_toggle_state(&rid, expected_after, cap_ms, req.uia.clone()).await;
        return (
            outcome.matched,
            if outcome.matched {
                VerificationEvidence::PostconditionMet
            } else {
                VerificationEvidence::Unverified
            },
            Some(outcome.waited_ms),
        );
    }
    // Pre-state unknown: poll for any non-equal value.
    let rid_for_poll = rid.clone();
    let uia_handle = req.uia.clone();
    let outcome = poll_until(cap_ms, move || {
        let rid = rid_for_poll.clone();
        let uia2 = uia_handle.clone();
        let live = uia2.run(move |a| Ok(read_toggle_state_inline(a, &rid))).ok().flatten();
        live.is_some() && live != pre_state
    })
    .await;
    (
        outcome.matched,
        if outcome.matched {
            VerificationEvidence::PostconditionMet
        } else {
            VerificationEvidence::Unverified
        },
        Some(outcome.waited_ms),
    )
}

async fn run_uia_select<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
    cap_ms: Option<u32>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let (rid, _) = match uia_runtime_id_and_bounds(candidate) {
        Some(p) => p,
        None => return (false, VerificationEvidence::Unverified, None),
    };
    let rid_for_call = rid.clone();
    let selected: Result<bool, Error> = req.uia.run(move |automation| {
        let el = match find_element_by_runtime_id(
            automation,
            &rid_for_call,
            Some(uiautomation::patterns::UIPatternType::SelectionItem),
        ) {
            Some(e) => e,
            None => return Ok(false),
        };
        let pat = match el.get_pattern::<uiautomation::patterns::UISelectionItemPattern>() {
            Ok(p) => p,
            Err(_) => return Ok(false),
        };
        Ok(pat.select().is_ok())
    });
    if !selected.unwrap_or(false) {
        return (false, VerificationEvidence::Unverified, None);
    }
    let outcome = poll_is_selected(&rid, cap_ms, req.uia.clone()).await;
    (
        outcome.matched,
        if outcome.matched {
            VerificationEvidence::PostconditionMet
        } else {
            VerificationEvidence::Unverified
        },
        Some(outcome.waited_ms),
    )
}

async fn run_uia_expand_collapse<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
    cap_ms: Option<u32>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let (rid, _) = match uia_runtime_id_and_bounds(candidate) {
        Some(p) => p,
        None => return (false, VerificationEvidence::Unverified, None),
    };

    // Run expand-or-collapse-as-appropriate on the UIA thread; report the
    // intended new state (0=Collapsed, 1=Expanded) via JSON-friendly i32.
    let rid_for_call = rid.clone();
    let action_result: Result<(bool, i32), Error> = req.uia.run(move |automation| {
        let el = match find_element_by_runtime_id(
            automation,
            &rid_for_call,
            Some(uiautomation::patterns::UIPatternType::ExpandCollapse),
        ) {
            Some(e) => e,
            None => return Ok((false, -1)),
        };
        let pat = match el.get_cached_pattern::<uiautomation::patterns::UIExpandCollapsePattern>() {
            Ok(p) => p,
            Err(_) => return Ok((false, -1)),
        };
        let cur = match pat.get_cached_state() {
            Ok(s) => s as i32,
            Err(_) => return Ok((false, -1)),
        };
        // 0=Collapsed,1=Expanded,2=PartiallyExpanded,3=LeafNode
        let target_state;
        let ok = if cur == 1 {
            target_state = 0;
            pat.collapse().is_ok()
        } else {
            target_state = 1;
            pat.expand().is_ok()
        };
        Ok((ok, target_state))
    });
    let (acted, target_state) = action_result.unwrap_or((false, -1));
    if !acted || target_state < 0 {
        return (false, VerificationEvidence::Unverified, None);
    }

    // Poll until live state equals target.
    let rid_for_poll = rid.clone();
    let uia_handle = req.uia.clone();
    let outcome = poll_until(cap_ms, move || {
        let rid = rid_for_poll.clone();
        let uia2 = uia_handle.clone();
        uia2.run(move |a| Ok(read_expand_state_inline(a, &rid)))
            .ok()
            .flatten()
            .map(|s| s == target_state)
            .unwrap_or(false)
    })
    .await;
    (
        outcome.matched,
        if outcome.matched {
            VerificationEvidence::PostconditionMet
        } else {
            VerificationEvidence::Unverified
        },
        Some(outcome.waited_ms),
    )
}

async fn run_uia_set_value<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
    cap_ms: Option<u32>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let (rid, _) = match uia_runtime_id_and_bounds(candidate) {
        Some(p) => p,
        None => return (false, VerificationEvidence::Unverified, None),
    };
    // Unwrap the redacted typed text only inside the closure that consumes
    // it. Never log the inner.
    let typed = match req.typed_text {
        Some(r) => r.clone(),
        None => return (false, VerificationEvidence::Unverified, None),
    };

    let rid_for_call = rid.clone();
    let typed_for_call = typed.clone();
    let set_ok: Result<bool, Error> = req.uia.run(move |automation| {
        let el = match find_element_by_runtime_id(
            automation,
            &rid_for_call,
            Some(uiautomation::patterns::UIPatternType::Value),
        ) {
            Some(e) => e,
            None => return Ok(false),
        };
        let pat = match el.get_pattern::<uiautomation::patterns::UIValuePattern>() {
            Ok(p) => p,
            Err(_) => return Ok(false),
        };
        let payload: &String = typed_for_call.as_inner();
        Ok(pat.set_value(payload.as_str()).is_ok())
    });
    if !set_ok.unwrap_or(false) {
        return (false, VerificationEvidence::Unverified, None);
    }

    let typed_for_poll: String = typed.as_inner().clone();
    let outcome = poll_value_contains(&rid, &typed_for_poll, cap_ms, req.uia.clone()).await;
    (
        outcome.matched,
        if outcome.matched {
            VerificationEvidence::PostconditionMet
        } else {
            VerificationEvidence::Unverified
        },
        Some(outcome.waited_ms),
    )
}

async fn run_bounds_click_uia<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
    cap_ms: Option<u32>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let (rid, bounds) = match uia_runtime_id_and_bounds(candidate) {
        Some(p) => p,
        None => return (false, VerificationEvidence::Unverified, None),
    };
    let parent_rid = parent_runtime_id(req.uia, rid);
    let pre_snapshot = parent_rid
        .as_ref()
        .and_then(|prid| snapshot_subtree(prid, req.uia));

    let click_ok = perform_click_at_bounds(bounds, req).await;
    if !click_ok {
        return (false, VerificationEvidence::Unverified, None);
    }

    if let (Some(prid), Some(pre)) = (parent_rid, pre_snapshot) {
        let uia_handle = req.uia.clone();
        let prid_owned = prid.clone();
        let outcome = poll_until(cap_ms, move || {
            match snapshot_subtree(&prid_owned, &uia_handle) {
                Some(post) => subtree_mutated(&pre, &post),
                None => true,
            }
        })
        .await;
        if outcome.matched {
            return (
                true,
                VerificationEvidence::PostconditionMet,
                Some(outcome.waited_ms),
            );
        }
        return (
            false,
            VerificationEvidence::HitTestOnly,
            Some(outcome.waited_ms),
        );
    }
    (false, VerificationEvidence::HitTestOnly, None)
}

async fn run_bounds_click_ocr<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let bounds = match candidate {
        TargetCandidate::Ocr { bounds, .. } => *bounds,
        _ => return (false, VerificationEvidence::Unverified, None),
    };
    let click_ok = perform_click_at_bounds(bounds, req).await;
    if !click_ok {
        return (false, VerificationEvidence::Unverified, None);
    }
    // No reliable postcondition without caller hint. Caller's wait_for /
    // expect (handled by run_strategy wrapper) provides verification.
    (false, VerificationEvidence::HitTestOnly, None)
}

async fn run_bounds_click_geometry<'a>(
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
) -> (bool, VerificationEvidence, Option<u32>) {
    let bounds = match candidate {
        TargetCandidate::Geometry { bounds, .. } => *bounds,
        _ => return (false, VerificationEvidence::Unverified, None),
    };
    let click_ok = perform_click_at_bounds(bounds, req).await;
    if !click_ok {
        return (false, VerificationEvidence::Unverified, None);
    }
    (false, VerificationEvidence::HitTestOnly, None)
}

// ---------- helpers ----------

fn uia_runtime_id_and_bounds(candidate: &TargetCandidate) -> Option<(Vec<i32>, Rect)> {
    match candidate {
        TargetCandidate::Uia {
            runtime_id, bounds, ..
        } => Some((runtime_id.clone(), *bounds)),
        _ => None,
    }
}

/// Inline twin of `verify::element_from_runtime_id`. Locates a live element
/// by RuntimeId on the UIA pool thread, prefetching the requested pattern
/// and identity in one BuildUpdatedCache pass (UIA-12).
fn find_element_by_runtime_id(
    automation: &uiautomation::UIAutomation,
    runtime_id: &[i32],
    pattern: Option<uiautomation::patterns::UIPatternType>,
) -> Option<uiautomation::UIElement> {
    use uiautomation::patterns::UIPatternType;
    use uiautomation::types::{TreeScope, UIProperty};
    use uiautomation::variants::{Value, Variant};
    let rid_variant: Variant = Value::ArrayI4(runtime_id.to_vec()).into();
    let condition = automation
        .create_property_condition(UIProperty::RuntimeId, rid_variant, None)
        .ok()?;
    let req = automation.create_cache_request().ok()?;
    req.add_property(UIProperty::RuntimeId).ok()?;
    if let Some(p) = pattern {
        match p {
            UIPatternType::Value => {
                req.add_property(UIProperty::ValueValue).ok()?;
            }
            UIPatternType::Toggle => {
                req.add_property(UIProperty::ToggleToggleState).ok()?;
            }
            UIPatternType::SelectionItem => {
                req.add_property(UIProperty::SelectionItemIsSelected).ok()?;
            }
            UIPatternType::ExpandCollapse => {
                req.add_property(UIProperty::ExpandCollapseExpandCollapseState).ok()?;
            }
            _ => {}
        }
        req.add_pattern(p).ok()?;
    }
    let root = automation.get_root_element().ok()?;
    root.find_first_build_cache(TreeScope::Subtree, &condition, &req)
        .ok()
}

fn read_toggle_state_inline(
    automation: &uiautomation::UIAutomation,
    runtime_id: &[i32],
) -> Option<i32> {
    use uiautomation::patterns::{UIPatternType, UITogglePattern};
    let el = find_element_by_runtime_id(automation, runtime_id, Some(UIPatternType::Toggle))?;
    let pat = el.get_cached_pattern::<UITogglePattern>().ok()?;
    Some(pat.get_cached_toggle_state().ok()? as i32)
}

fn read_expand_state_inline(
    automation: &uiautomation::UIAutomation,
    runtime_id: &[i32],
) -> Option<i32> {
    use uiautomation::patterns::{UIExpandCollapsePattern, UIPatternType};
    let el = find_element_by_runtime_id(
        automation,
        runtime_id,
        Some(UIPatternType::ExpandCollapse),
    )?;
    let pat = el.get_cached_pattern::<UIExpandCollapsePattern>().ok()?;
    Some(pat.get_cached_state().ok()? as i32)
}

/// Walk to parent's RuntimeId via the control-view tree walker. Used to
/// snapshot the subtree we expect to mutate post-action.
fn parent_runtime_id(uia: &Arc<UiaPoolHandle>, runtime_id: Vec<i32>) -> Option<Vec<i32>> {
    let rid = runtime_id;
    uia.run(move |automation| {
        let el = match find_element_by_runtime_id(automation, &rid, None) {
            Some(e) => e,
            None => return Ok(None),
        };
        let walker = match automation.get_control_view_walker() {
            Ok(w) => w,
            Err(_) => return Ok(None),
        };
        let parent = match walker.get_parent(&el) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        Ok(parent.get_runtime_id().ok())
    })
    .ok()
    .flatten()
}

/// Dispatch a click via the input STA thread at bounds center.
async fn perform_click_at_bounds(bounds: Rect, req: &TargetedRequest<'_>) -> bool {
    let cx = bounds.x + bounds.w / 2;
    let cy = bounds.y + bounds.h / 2;
    let mods: Vec<String> = req
        .modifiers
        .map(|m| m.to_vec())
        .unwrap_or_default();
    req.input
        .run(move || crate::input::handlers::click(cx, cy, MouseButton::Left, 1, &mods, false))
        .is_ok()
}

// ---------- caller expect / wait_for evaluation ----------

async fn poll_selector(
    uia: &Arc<UiaPoolHandle>,
    sel: Selector,
    cap_ms: Option<u32>,
) -> (bool, u32) {
    let cap = cap_ms.unwrap_or(2000);
    let deadline = Instant::now() + Duration::from_millis(cap as u64);
    let start = Instant::now();
    loop {
        if Instant::now() >= deadline {
            return (false, start.elapsed().as_millis().min(u32::MAX as u128) as u32);
        }
        let s = sel.clone();
        let probe: Result<Option<fastuse_proto::UIANode>, Error> = uia.run(move |automation| {
            let hwnd = match crate::uia::automation::foreground_hwnd() {
                Some(h) => h,
                None => return Ok(None),
            };
            let root = match crate::uia::cache::get_or_fetch(automation, hwnd) {
                Ok(r) => r,
                Err(_) => return Ok(None),
            };
            crate::uia::find::find_first(automation, &root, &s)
        });
        if matches!(probe, Ok(Some(_))) {
            return (true, start.elapsed().as_millis().min(u32::MAX as u128) as u32);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn poll_expect(
    uia: &Arc<UiaPoolHandle>,
    exp: &ExpectClause,
    cap_ms: Option<u32>,
) -> (bool, u32) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    match exp {
        ExpectClause::SelectorMatches(sel) => poll_selector(uia, sel.clone(), cap_ms).await,
        ExpectClause::DialogOpens => {
            // SAFETY: GetForegroundWindow is safe from any thread.
            let pre = unsafe { GetForegroundWindow() }.0 as u64;
            let cap = cap_ms.unwrap_or(2000);
            let start = Instant::now();
            let outcome = poll_until(Some(cap), move || {
                // SAFETY: GetForegroundWindow is always safe.
                let cur = unsafe { GetForegroundWindow() }.0 as u64;
                cur != 0 && cur != pre
            })
            .await;
            (
                outcome.matched,
                start.elapsed().as_millis().min(u32::MAX as u128) as u32,
            )
        }
        ExpectClause::WindowTitleMatches(substr) => {
            let needle = substr.clone();
            let cap = cap_ms.unwrap_or(2000);
            let start = Instant::now();
            let outcome = poll_until(Some(cap), move || {
                // SAFETY: GetForegroundWindow is always safe.
                let h = unsafe { GetForegroundWindow() };
                if h.is_invalid() {
                    return false;
                }
                let title = crate::window::list_windows::safe_get_window_text(h, 50)
                    .unwrap_or_default();
                title.contains(&needle)
            })
            .await;
            (
                outcome.matched,
                start.elapsed().as_millis().min(u32::MAX as u128) as u32,
            )
        }
        ExpectClause::ForegroundChangesTo { class, title } => {
            let want_class = class.clone();
            let want_title = title.clone();
            let cap = cap_ms.unwrap_or(2000);
            let start = Instant::now();
            let outcome = poll_until(Some(cap), move || {
                // SAFETY: GetForegroundWindow is always safe.
                let h: HWND = unsafe { GetForegroundWindow() };
                if h.is_invalid() {
                    return false;
                }
                if let Some(want) = want_title.as_ref() {
                    let t = crate::window::list_windows::safe_get_window_text(h, 50)
                        .unwrap_or_default();
                    if !t.contains(want) {
                        return false;
                    }
                }
                if let Some(want) = want_class.as_ref() {
                    let mut buf = [0u16; 256];
                    // SAFETY: HWND is the foreground (just observed); buf sized.
                    let n = unsafe {
                        windows::Win32::UI::WindowsAndMessaging::GetClassNameW(h, &mut buf)
                    };
                    if n <= 0 {
                        return false;
                    }
                    let cls = String::from_utf16_lossy(&buf[..n as usize]);
                    if !cls.contains(want) {
                        return false;
                    }
                }
                true
            })
            .await;
            (
                outcome.matched,
                start.elapsed().as_millis().min(u32::MAX as u128) as u32,
            )
        }
    }
}

// ---------- screenshot_after ----------

fn capture_screenshot_after<'a>(
    req: &TargetedRequest<'a>,
) -> Option<Box<ScreenshotPayload>> {
    let opts = req.opts?.screenshot_after.as_ref()?;
    let cap = req.capture?;
    let format = opts.format;
    let resp = match opts.region.as_ref() {
        None => handle_screenshot(cap, None, format).ok()?,
        Some(RegionSpec::Auto) => {
            let (x, y, w, h) = foreground_window_rect()?;
            handle_screenshot_region(
                cap,
                Rect { x, y, w: w as i32, h: h as i32 },
                None,
                format,
            )
            .ok()?
        }
        Some(RegionSpec::Rect { x, y, w, h }) => handle_screenshot_region(
            cap,
            Rect {
                x: *x,
                y: *y,
                w: *w as i32,
                h: *h as i32,
            },
            None,
            format,
        )
        .ok()?,
    };
    match resp {
        Response::Screenshot {
            bytes,
            mime,
            width,
            height,
        } => Some(Box::new(ScreenshotPayload {
            bytes,
            mime,
            width,
            height,
        })),
        _ => None,
    }
}

fn foreground_window_rect() -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    // SAFETY: GetForegroundWindow is always safe.
    unsafe {
        let h = GetForegroundWindow();
        if h.is_invalid() {
            return None;
        }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() {
            return None;
        }
        let w = (r.right - r.left).max(0) as u32;
        let h = (r.bottom - r.top).max(0) as u32;
        Some((r.left, r.top, w, h))
    }
}

fn resolve_foreground(_uia: &Arc<UiaPoolHandle>) -> Option<u64> {
    // GetForegroundWindow() is thread-safe Win32 — no UIA pool dispatch
    // required. Reuses the same helper that powers Request::ForegroundWindow
    // (via uia/automation.rs) so behavior matches the existing arm.
    crate::uia::automation::foreground_hwnd()
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
