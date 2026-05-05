//! Per-action postcondition contracts.
//!
//! UIA pattern handlers race against provider-side updates. We can't
//! single-read after Invoke and trust the result — Electron, WPF binding
//! pipelines, and message-pump-routed apps take 50-200ms before properties
//! settle. The helper polls at 25ms cadence with a 250ms cap (configurable
//! via wait_timeout_ms).

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastuse_proto::wire::VerificationEvidence;
use uiautomation::patterns::{UIPatternType, UISelectionItemPattern, UITogglePattern, UIValuePattern};
use uiautomation::types::{TreeScope, UIProperty};
use uiautomation::variants::{Value, Variant};
use uiautomation::{UIAutomation, UIElement};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::uia_pool::UiaPoolHandle;

/// Resolve a live `UIElement` by RuntimeId from the UIA root, prefetching
/// the requested pattern + RuntimeId property in one `BuildUpdatedCache`
/// pass (UIA-12 / `check_cacherequest`). Returns `None` when the element
/// is no longer in the tree (e.g. dialog closed mid-poll).
fn element_from_runtime_id(
    automation: &UIAutomation,
    runtime_id: &[i32],
    pattern: Option<UIPatternType>,
) -> Option<UIElement> {
    // Build a Variant wrapping the RuntimeId i32 array (VT_ARRAY|VT_I4).
    let rid_variant: Variant = Value::ArrayI4(runtime_id.to_vec()).into();
    let condition = automation
        .create_property_condition(UIProperty::RuntimeId, rid_variant, None)
        .ok()?;
    let req = automation.create_cache_request().ok()?;
    // Always cache RuntimeId so we can re-confirm identity from the cache.
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
            _ => {}
        }
        req.add_pattern(p).ok()?;
    }
    let root = automation.get_root_element().ok()?;
    root.find_first_build_cache(TreeScope::Subtree, &condition, &req)
        .ok()
}

/// Outcome of a verification poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Whether the postcondition matched within the cap.
    pub matched: bool,
    /// Wall-clock spent polling (ms).
    pub waited_ms: u32,
    /// Tag describing what evidence we gathered.
    pub evidence: VerificationEvidence,
}

const DEFAULT_CADENCE_MS: u64 = 25;
const DEFAULT_CAP_MS: u32 = 250;

/// Poll a closure at 25ms cadence until it returns `true` or `cap_ms`
/// elapses. The closure runs on whatever thread owns the relevant resource;
/// for UIA reads, dispatch to `uia_pool` from inside the closure.
pub async fn poll_until<F>(cap_ms: Option<u32>, mut check: F) -> VerifyOutcome
where
    F: FnMut() -> bool + Send,
{
    let cap = cap_ms.unwrap_or(DEFAULT_CAP_MS);
    let start = Instant::now();
    let cap_dur = Duration::from_millis(cap as u64);
    loop {
        if check() {
            return VerifyOutcome {
                matched: true,
                waited_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
                evidence: VerificationEvidence::PostconditionMet,
            };
        }
        if start.elapsed() >= cap_dur {
            return VerifyOutcome {
                matched: false,
                waited_ms: cap,
                evidence: VerificationEvidence::Unverified,
            };
        }
        tokio::time::sleep(Duration::from_millis(DEFAULT_CADENCE_MS)).await;
    }
}

/// Pre-action snapshot for subtree-mutation detection. Captures the
/// target's parent's children RuntimeId set, the foreground HWND, and the
/// focused element's RuntimeId.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubtreeSnapshot {
    /// Sorted RuntimeId byte vectors for each child of the target's parent.
    pub child_runtime_ids: Vec<Vec<i32>>,
    /// Foreground HWND at snapshot time.
    pub foreground_hwnd: u64,
    /// Focused element's RuntimeId, if any.
    pub focused_runtime_id: Option<Vec<i32>>,
}

/// Capture the pre-action snapshot. Runs on uia_pool.
pub fn snapshot_subtree(
    parent_runtime_id: &[i32],
    uia: &Arc<UiaPoolHandle>,
) -> Option<SubtreeSnapshot> {
    let prid = parent_runtime_id.to_vec();
    uia.run(move |a| Ok(snapshot_on_uia_thread(a, &prid)))
        .ok()
        .flatten()
}

fn snapshot_on_uia_thread(
    automation: &UIAutomation,
    parent_runtime_id: &[i32],
) -> Option<SubtreeSnapshot> {
    // Locate the parent. If it's gone, the subtree we'd compare against
    // is gone too — bail with None so callers fall back to Unverified.
    let parent = element_from_runtime_id(automation, parent_runtime_id, None)?;

    // Children: enumerate via FindAll(TreeScope::Children, true_cond) and
    // read each child's RuntimeId. RuntimeId is a method, not a Current*
    // accessor, so it satisfies UIA-12.
    let true_cond = automation.create_true_condition().ok()?;
    let mut child_runtime_ids: Vec<Vec<i32>> = parent
        .find_all(TreeScope::Children, &true_cond)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| c.get_runtime_id().ok())
        .collect();
    // Sort each id (already sorted as a sequence; sort the outer Vec for
    // stable set comparison in `subtree_mutated`).
    child_runtime_ids.sort();

    // SAFETY: GetForegroundWindow is safe to call from any thread.
    let foreground_hwnd = unsafe { GetForegroundWindow() }.0 as u64;

    let focused_runtime_id = automation
        .get_focused_element()
        .ok()
        .and_then(|el| el.get_runtime_id().ok());

    Some(SubtreeSnapshot {
        child_runtime_ids,
        foreground_hwnd,
        focused_runtime_id,
    })
}

/// Compare a fresh snapshot against the pre-action snapshot. Subtree
/// mutated iff: child RuntimeId set differs, OR foreground HWND changed,
/// OR focused RuntimeId changed.
pub fn subtree_mutated(pre: &SubtreeSnapshot, post: &SubtreeSnapshot) -> bool {
    if pre.foreground_hwnd != post.foreground_hwnd {
        return true;
    }
    if pre.focused_runtime_id != post.focused_runtime_id {
        return true;
    }
    if pre.child_runtime_ids.len() != post.child_runtime_ids.len() {
        return true;
    }
    // Both already sorted by snapshot_on_uia_thread.
    for (a, b) in pre
        .child_runtime_ids
        .iter()
        .zip(post.child_runtime_ids.iter())
    {
        if a != b {
            return true;
        }
    }
    false
}

/// Toggle postcondition: poll `ToggleState` until it equals `expected_after`.
/// Runs on uia_pool because each read is a UIA COM call.
pub async fn poll_toggle_state(
    runtime_id: &[i32],
    expected_after: i32,
    cap_ms: Option<u32>,
    uia: Arc<UiaPoolHandle>,
) -> VerifyOutcome {
    let rid = runtime_id.to_vec();
    poll_until(cap_ms, || {
        let rid = rid.clone();
        let uia2 = uia.clone();
        // One poll iteration: dispatch a UIA read.
        uia2.run(move |a| Ok(read_toggle_state(a, &rid)))
            .ok()
            .flatten()
            .map(|s| s == expected_after)
            .unwrap_or(false)
    })
    .await
}

fn read_toggle_state(automation: &UIAutomation, runtime_id: &[i32]) -> Option<i32> {
    let el = element_from_runtime_id(automation, runtime_id, Some(UIPatternType::Toggle))?;
    let pat = el.get_cached_pattern::<UITogglePattern>().ok()?;
    let state = pat.get_cached_toggle_state().ok()?;
    Some(state as i32)
}

/// Selection postcondition: poll `IsSelected == true`.
pub async fn poll_is_selected(
    runtime_id: &[i32],
    cap_ms: Option<u32>,
    uia: Arc<UiaPoolHandle>,
) -> VerifyOutcome {
    let rid = runtime_id.to_vec();
    poll_until(cap_ms, || {
        let rid = rid.clone();
        let uia2 = uia.clone();
        uia2.run(move |a| Ok(read_is_selected(a, &rid)))
            .ok()
            .flatten()
            .unwrap_or(false)
    })
    .await
}

fn read_is_selected(automation: &UIAutomation, runtime_id: &[i32]) -> Option<bool> {
    let el = element_from_runtime_id(automation, runtime_id, Some(UIPatternType::SelectionItem))?;
    let pat = el.get_cached_pattern::<UISelectionItemPattern>().ok()?;
    pat.is_cached_selected().ok()
}

/// Value postcondition: poll until the live value contains the typed text
/// (case-insensitive) within length tolerance.
pub async fn poll_value_contains(
    runtime_id: &[i32],
    typed: &str,
    cap_ms: Option<u32>,
    uia: Arc<UiaPoolHandle>,
) -> VerifyOutcome {
    let rid = runtime_id.to_vec();
    let typed_owned = typed.to_string();
    let typed_lower = typed_owned.to_lowercase();
    let typed_len = typed_owned.chars().count() as f32;
    let outcome = poll_until(cap_ms, || {
        let rid = rid.clone();
        let uia2 = uia.clone();
        let live = uia2.run(move |a| Ok(read_value(a, &rid))).ok().flatten();
        if let Some(v) = live {
            let v_lower = v.to_lowercase();
            let v_len = v.chars().count() as f32;
            let len_ratio = if typed_len > 0.0 { v_len / typed_len } else { 0.0 };
            if v_lower.contains(&typed_lower) && (0.7..=1.5).contains(&len_ratio) {
                return true;
            }
        }
        false
    })
    .await;

    // On tolerance failure (we tried but text didn't match cleanly) return
    // Unverified instead of false-fail. Apps normalize text — Excel
    // uppercases formulas, search boxes strip whitespace, IME composition
    // reorders. Better to admit "couldn't tell" than yell wolf.
    if !outcome.matched {
        return VerifyOutcome {
            matched: false,
            waited_ms: outcome.waited_ms,
            evidence: VerificationEvidence::Unverified,
        };
    }
    outcome
}

fn read_value(automation: &UIAutomation, runtime_id: &[i32]) -> Option<String> {
    let el = element_from_runtime_id(automation, runtime_id, Some(UIPatternType::Value))?;
    let pat = el.get_cached_pattern::<UIValuePattern>().ok()?;
    pat.get_cached_value().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn poll_until_matches_first_iter() {
        let outcome = poll_until(Some(1000), || true).await;
        assert!(outcome.matched);
        assert!(outcome.waited_ms < 50, "got {}", outcome.waited_ms);
        assert_eq!(outcome.evidence, VerificationEvidence::PostconditionMet);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_until_caps_at_timeout() {
        let outcome = poll_until(Some(80), || false).await;
        assert!(!outcome.matched);
        assert_eq!(outcome.waited_ms, 80);
        assert_eq!(outcome.evidence, VerificationEvidence::Unverified);
    }

    #[test]
    fn subtree_mutated_detects_foreground_change() {
        let a = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: None,
        };
        let b = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2]],
            foreground_hwnd: 0xbbbb,
            focused_runtime_id: None,
        };
        assert!(subtree_mutated(&a, &b));
    }

    #[test]
    fn subtree_mutated_detects_child_set_diff() {
        let a = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: None,
        };
        let b = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2], vec![3, 4]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: None,
        };
        assert!(subtree_mutated(&a, &b));
    }

    #[test]
    fn subtree_unchanged_returns_false() {
        let a = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2], vec![3, 4]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: Some(vec![1, 2]),
        };
        let b = a.clone();
        assert!(!subtree_mutated(&a, &b));
    }
}
