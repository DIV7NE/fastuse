//! Window/element fingerprinting. Produces `WindowSignals` and a ranked
//! candidate list. Cached per composite key (hwnd, pid, process_start, gen).

use dashmap::DashMap;
use fastuse_proto::coords::Rect;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::OnceLock;
use windows::Win32::Foundation::FILETIME;

use crate::targeting::candidate::TargetCandidate;
use crate::uia_pool::UiaPoolHandle;

/// Composite cache key. HWND alone is unsafe — Windows reuses HWND values on
/// long-lived sessions; without `pid` + `process_start_time` a stale entry can
/// alias a new process at the same handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProfileCacheKey {
    /// Top-level HWND of the target window, cast to u64 for cross-thread reuse.
    pub hwnd: u64,
    /// Owning process ID.
    pub pid: u32,
    /// Process creation time low 64 bits (`FILETIME` flattened). Stable for
    /// the lifetime of the process; differs across reincarnations of the same PID.
    pub process_start: u64,
    /// Bumped whenever we detect a window-rect / DPI / display-topology change
    /// that invalidates cached candidate bounds.
    pub generation: u64,
}

impl ProfileCacheKey {
    /// Flatten a `FILETIME` into a single u64.
    pub fn flatten_filetime(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
    }
}

/// Heuristic verdict on whether the UIA tree exposed for this window is rich
/// enough to act on directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TreeQuality {
    /// Named descendants with AutomationIds, multiple ControlTypes — ordinary.
    Healthy,
    /// Some named descendants, but anonymous Pane regions dominate (Electron
    /// with assistive tech enabled, Qt with QtAccessibilityPlugin).
    Mixed,
    /// Mostly anonymous Pane elements with no AutomationIds — Electron without
    /// assistive tech, custom canvas renderers, games.
    Degraded,
}

/// Process integrity level — for UIPI gate before action attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrityLevel {
    /// Lower than ours — fine, but we shouldn't be elevated against it.
    Low,
    /// Equal to ours — UIPI permits.
    Medium,
    /// Higher than ours — UIPI blocks SendInput. Return PermissionRequired
    /// before attempting.
    High,
    /// Could not determine.
    Unknown,
}

/// Signals describing the target window. Hint, not authority — `framework_id`
/// can lie (mixed-provider Electron+WebView2), `window_class` is the hard backstop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSignals {
    /// `IUIAutomation::NativeWindowHandle.FrameworkId` — provider-reported,
    /// may be missing/stale for mixed-provider apps.
    pub framework_id: Option<String>,
    /// `GetClassNameW` of the target HWND. Always populated.
    pub window_class: String,
    /// `GetClassNameW` of every visible child HWND, depth ≤ 2. Surfaces
    /// markers like `Chrome_RenderWidgetHostHWND`, `WebView2`, `Qt*`,
    /// `Windows.UI.Core.CoreWindow`.
    pub child_classes: Vec<String>,
    /// Two-level UIA probe verdict.
    pub uia_tree_quality: TreeQuality,
    /// Process integrity level vs ours.
    pub integrity_level: IntegrityLevel,
    /// `DwmGetWindowAttribute(DWMWA_CLOAKED)` returned non-zero.
    pub cloaked: bool,
    /// `IsIconic` true.
    pub minimized: bool,
    /// Bounding rect overlaps a topmost window owned by another process.
    pub occluded: bool,
    /// `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` — modern apps
    /// have window rect ≠ visible frame.
    pub dwm_extended_frame_bounds: Rect,
    /// `GetForegroundWindow() == hwnd` (foreground eligibility hint).
    pub is_foreground: bool,
}

/// Full target profile = window signals + ranked candidates.
#[derive(Debug, Clone)]
pub struct TargetProfile {
    /// Per-window signals.
    pub window: WindowSignals,
    /// Candidates sorted by `score` descending; top one is the chosen target
    /// unless overridden by selector specificity.
    pub candidates: Vec<TargetCandidate>,
}

static PROFILE_CACHE: OnceLock<DashMap<ProfileCacheKey, TargetProfile>> = OnceLock::new();

fn cache() -> &'static DashMap<ProfileCacheKey, TargetProfile> {
    PROFILE_CACHE.get_or_init(DashMap::new)
}

/// Resolve or build a profile for `hwnd` using the supplied UIA pool.
/// Caches under `ProfileCacheKey { hwnd, pid, process_start, generation: 0 }`
/// initially; later actions bump generation when bounds invalidate.
pub fn profile_window(
    hwnd: u64,
    uia: &Arc<UiaPoolHandle>,
) -> Result<TargetProfile, ProfileError> {
    let pid = pid_of_hwnd(hwnd)?;
    let process_start = process_start_time(pid).unwrap_or(0);
    let key = ProfileCacheKey {
        hwnd,
        pid,
        process_start,
        generation: 0,
    };

    if let Some(p) = cache().get(&key) {
        return Ok(p.clone());
    }

    // Run the probe on the UIA pool — D-25 invariant.
    let signals = uia
        .run(move |automation| {
            probe_on_uia_thread(automation, hwnd).map_err(|e| {
                fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::Internal,
                    format!("profile probe: {e}"),
                )
            })
        })
        .map_err(|_| ProfileError::UiaUnavailable)?;

    let profile = TargetProfile {
        window: signals,
        candidates: Vec::new(), // populated per-action by Task 4
    };
    cache().insert(key, profile.clone());
    Ok(profile)
}

fn pid_of_hwnd(hwnd: u64) -> Result<u32, ProfileError> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0u32;
    // SAFETY: HWND is opaque; PID out-pointer is u32 stack slot.
    let tid = unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid)) };
    if tid == 0 {
        return Err(ProfileError::WindowGone);
    }
    Ok(pid)
}

fn process_start_time(pid: u32) -> Option<u64> {
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: OpenProcess with QUERY_LIMITED is the documented way to query
    // start-time of arbitrary processes; fails closed when permission denied.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let res = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    res.ok()?;
    Some(ProfileCacheKey::flatten_filetime(creation))
}

/// Runs on the UIA pool MTA worker. All `windows::*` and `uiautomation::*`
/// calls happen here.
fn probe_on_uia_thread(
    automation: &uiautomation::UIAutomation,
    hwnd: u64,
) -> Result<WindowSignals, ProfileError> {
    use windows::Win32::Foundation::HWND;
    let h = HWND(hwnd as *mut _);
    let window_class = read_class_name(h);
    let child_classes = collect_child_classes(h, 2);
    let cloaked = read_cloaked(h);
    let minimized = read_minimized(h);
    let occluded = false; // detailed handling deferred to v1.1
    let dwm_extended_frame_bounds = read_dwm_extended_frame(h);
    let is_foreground = read_foreground(h);
    let framework_id = read_framework_id_uia(automation, hwnd);
    let uia_tree_quality = probe_tree_quality_uia(automation, hwnd);
    let integrity_level = read_integrity_level(h);

    Ok(WindowSignals {
        framework_id,
        window_class,
        child_classes,
        uia_tree_quality,
        integrity_level,
        cloaked,
        minimized,
        occluded,
        dwm_extended_frame_bounds,
        is_foreground,
    })
}

fn read_class_name(h: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 256];
    // SAFETY: GetClassNameW writes at most buf.len() wide chars + NUL.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn collect_child_classes(
    h: windows::Win32::Foundation::HWND,
    max_depth: u32,
) -> Vec<String> {
    use std::cell::RefCell;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, IsWindowVisible};

    thread_local!(static SINK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });
    SINK.with(|s| s.borrow_mut().clear());

    extern "system" fn cb(hwnd: HWND, _lp: LPARAM) -> BOOL {
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let cls = read_class_name(hwnd);
            if !cls.is_empty() {
                SINK.with(|s| s.borrow_mut().push(cls));
            }
        }
        BOOL(1)
    }

    let _ = max_depth; // EnumChildWindows is recursive in Win32 native; we accept that.
    // SAFETY: EnumChildWindows runs cb synchronously on this thread.
    let _ = unsafe { EnumChildWindows(Some(h), Some(cb), LPARAM(0)) };
    SINK.with(|s| s.borrow_mut().drain(..).collect())
}

fn read_cloaked(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut cloaked: u32 = 0;
    // SAFETY: DwmGetWindowAttribute writes a u32 when DWMWA_CLOAKED is queried.
    let res = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    res.is_ok() && cloaked != 0
}

fn read_minimized(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsIconic;
    // SAFETY: IsIconic is a pure HWND query.
    unsafe { IsIconic(h) }.as_bool()
}

fn read_dwm_extended_frame(h: windows::Win32::Foundation::HWND) -> Rect {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    let mut r = RECT::default();
    // SAFETY: DwmGetWindowAttribute writes a RECT for DWMWA_EXTENDED_FRAME_BOUNDS.
    let res = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as _,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if res.is_err() {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    }
    Rect {
        x: r.left,
        y: r.top,
        w: (r.right - r.left).max(0),
        h: (r.bottom - r.top).max(0),
    }
}

fn read_foreground(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: GetForegroundWindow has no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    fg == h
}

fn read_framework_id_uia(
    automation: &uiautomation::UIAutomation,
    hwnd: u64,
) -> Option<String> {
    use uiautomation::types::{Handle, UIProperty};
    use windows::Win32::Foundation::HWND;

    let h = HWND(hwnd as *mut core::ffi::c_void);
    let element = automation.element_from_handle(Handle::from(h)).ok()?;
    let req = automation.create_cache_request().ok()?;
    req.add_property(UIProperty::FrameworkId).ok()?;
    let cached = element.build_updated_cache(&req).ok()?;
    let id = cached.get_cached_framework_id().ok()?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Two-level UIA tree heuristic. Counts nodes at depth ≤ 2 with non-empty
/// Name OR non-empty AutomationId OR non-Pane ControlType. Falls open to
/// `Healthy` on any cache-fetch failure — better to act and let the
/// hit-test gate (Task 6) reject than to over-report Degraded.
fn probe_tree_quality_uia(
    automation: &uiautomation::UIAutomation,
    hwnd: u64,
) -> TreeQuality {
    use uiautomation::types::{ControlType as U, Handle, TreeScope, UIProperty};
    use windows::Win32::Foundation::HWND;

    let h = HWND(hwnd as *mut core::ffi::c_void);
    let element = match automation.element_from_handle(Handle::from(h)) {
        Ok(e) => e,
        Err(_) => return TreeQuality::Healthy,
    };
    let req = match automation.create_cache_request() {
        Ok(r) => r,
        Err(_) => return TreeQuality::Healthy,
    };
    for p in [
        UIProperty::Name,
        UIProperty::AutomationId,
        UIProperty::ControlType,
    ] {
        if req.add_property(p).is_err() {
            return TreeQuality::Healthy;
        }
    }
    if req.set_tree_scope(TreeScope::Subtree).is_err() {
        return TreeQuality::Healthy;
    }
    let cond = match automation.create_true_condition() {
        Ok(c) => c,
        Err(_) => return TreeQuality::Healthy,
    };
    if req.set_tree_filter(cond).is_err() {
        return TreeQuality::Healthy;
    }
    let cached_root = match element.build_updated_cache(&req) {
        Ok(r) => r,
        Err(_) => return TreeQuality::Healthy,
    };

    fn walk(
        el: &uiautomation::UIElement,
        depth_left: u32,
        total: &mut u32,
        named_or_id: &mut u32,
    ) {
        *total += 1;
        let name = el.get_cached_name().unwrap_or_default();
        let aid = el.get_cached_automation_id().unwrap_or_default();
        let ct = el.get_cached_control_type().ok();
        let is_named_or_id = !name.is_empty() || !aid.is_empty();
        let is_non_pane = ct.map(|c| !matches!(c, U::Pane)).unwrap_or(false);
        if is_named_or_id || is_non_pane {
            *named_or_id += 1;
        }
        if depth_left == 0 {
            return;
        }
        if let Ok(kids) = el.get_cached_children() {
            for k in &kids {
                walk(k, depth_left - 1, total, named_or_id);
            }
        }
    }
    let mut total: u32 = 0;
    let mut named_or_id: u32 = 0;
    walk(&cached_root, 2, &mut total, &mut named_or_id);

    if total <= 2 {
        return TreeQuality::Degraded;
    }
    let ratio = named_or_id as f32 / total as f32;
    if ratio < 0.15 {
        TreeQuality::Degraded
    } else if ratio < 0.40 {
        TreeQuality::Mixed
    } else {
        TreeQuality::Healthy
    }
}

/// Build a full `TargetProfile` for `hwnd` against `selector`. Composes
/// `profile_window` (signals, cached) with `resolve_candidates` (per-call;
/// candidate identity is selector-dependent so it's not cached at the
/// profile level).
///
/// When `capture` and `ocr` handles are supplied AND the selector carries a
/// text component (`ByName` directly, or nested under `And`/`Or`), the
/// profile call augments empty / degraded UIA results with OCR-derived
/// `TargetCandidate::Ocr` entries. This is the only way an `execute_targeted`
/// pipeline can reach the `BoundsClickOcr` tier on apps where UIA returns
/// nothing (Electron without assistive tech, custom-rendered WPF like
/// L-Connect3, native Direct2D / canvas surfaces).
pub async fn profile_window_for_selector(
    hwnd: u64,
    selector: &fastuse_proto::Selector,
    uia: &Arc<UiaPoolHandle>,
    capture: Option<&Arc<crate::capture_thread::CaptureThreadHandle>>,
    ocr: Option<&Arc<crate::ocr_thread::OcrThreadHandle>>,
) -> Result<TargetProfile, ProfileError> {
    let mut profile = profile_window(hwnd, uia)?;
    let uia_candidates = crate::targeting::candidate::resolve_candidates(hwnd, selector, uia);

    // Healthy UIA + at least one candidate → done. Skip OCR.
    let degraded = profile.window.uia_tree_quality == TreeQuality::Degraded;
    if !uia_candidates.is_empty() && !degraded {
        profile.candidates = uia_candidates;
        return Ok(profile);
    }

    // OCR fallback condition: selector carries searchable text, capture +
    // ocr handles available, AND (UIA empty OR tree degraded).
    let needs_ocr = uia_candidates.is_empty() || degraded;
    tracing::info!(
        target: "fastuse_win::targeting::profile",
        needs_ocr,
        degraded,
        uia_count = uia_candidates.len(),
        has_capture = capture.is_some(),
        has_ocr = ocr.is_some(),
        selector_kind = ?std::mem::discriminant(selector),
        "ocr_fallback decision",
    );
    let mut ocr_candidates: Vec<crate::targeting::candidate::TargetCandidate> = Vec::new();
    if needs_ocr {
        if let (Some(text), Some(cap), Some(ocr_h)) = (selector_text(selector), capture, ocr) {
            tracing::info!(target: "fastuse_win::targeting::profile", text = %text, "ocr fallback firing");
            // Resolve client rect via UIA pool worker (D-25 — windows::* not
            // on tokio threads). Fall back to DWM extended frame on failure.
            let region = client_rect_via_uia(hwnd, uia)
                .unwrap_or(profile.window.dwm_extended_frame_bounds);
            tracing::info!(target: "fastuse_win::targeting::profile", ?region, "ocr region resolved");
            if region.w > 0 && region.h > 0 {
                let hits = crate::ocr::cropped::ocr_cropped_progressive(
                    region,
                    &text,
                    cap.clone(),
                    ocr_h.clone(),
                )
                .await;
                tracing::info!(target: "fastuse_win::targeting::profile", hit_count = hits.len(), "ocr hits");
                ocr_candidates = hits
                    .into_iter()
                    .map(|h| crate::targeting::candidate::TargetCandidate::Ocr {
                        text: h.text,
                        bounds: h.bounds,
                        score: (h.confidence * 0.85).clamp(0.0, 0.85),
                    })
                    .collect();
            }
        }
    }

    let mut all = uia_candidates;
    all.extend(ocr_candidates);
    all.sort_by(|a, b| {
        b.score()
            .partial_cmp(&a.score())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    profile.candidates = all;
    Ok(profile)
}

/// Extract a text component from the selector for OCR seeding. Returns the
/// first `ByName` payload, recursing into `And`/`Or`. `Not` and the non-text
/// selector kinds (`ByControlType`/`ByClass`/`ByAutomationId`) yield `None` —
/// AutomationIds are not visible text and class names are framework strings,
/// neither survives an OCR pass cleanly.
fn selector_text(s: &fastuse_proto::Selector) -> Option<String> {
    use fastuse_proto::Selector;
    match s {
        Selector::ByName(n) if !n.is_empty() => Some(n.clone()),
        Selector::ByName(_) => None,
        Selector::And(parts) | Selector::Or(parts) => parts.iter().find_map(selector_text),
        Selector::Not(_)
        | Selector::ByControlType(_)
        | Selector::ByClass(_)
        | Selector::ByAutomationId(_) => None,
    }
}

/// `GetClientRect` mapped to virtual-desktop coords. Dispatched through the
/// UIA pool worker so the underlying `windows::*` call honors D-25.
fn client_rect_via_uia(hwnd: u64, uia: &Arc<UiaPoolHandle>) -> Option<Rect> {
    uia.run(move |_automation| Ok(client_rect_inline(hwnd))).ok().flatten()
}

fn client_rect_inline(hwnd: u64) -> Option<Rect> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    let h = HWND(hwnd as *mut core::ffi::c_void);
    let mut r = RECT::default();
    // SAFETY: HWND is the dispatcher-supplied target; RECT is a stack out-pointer.
    unsafe { GetClientRect(h, &mut r) }.ok()?;
    let mut origin = POINT { x: r.left, y: r.top };
    // SAFETY: ClientToScreen takes HWND + POINT in/out pointer; both valid here.
    if !unsafe { ClientToScreen(h, &mut origin) }.as_bool() {
        return None;
    }
    Some(Rect {
        x: origin.x,
        y: origin.y,
        w: (r.right - r.left).max(0),
        h: (r.bottom - r.top).max(0),
    })
}

fn read_integrity_level(h: windows::Win32::Foundation::HWND) -> IntegrityLevel {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if tid == 0 {
        return IntegrityLevel::Unknown;
    }
    // SAFETY: standard PROCESS_QUERY_LIMITED_INFORMATION + TOKEN_QUERY ladder.
    let proc_h = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(_) => return IntegrityLevel::Unknown,
    };
    let mut tok = HANDLE::default();
    let ok = unsafe { OpenProcessToken(proc_h, TOKEN_QUERY, &mut tok) };
    let _ = unsafe { CloseHandle(proc_h) };
    if ok.is_err() {
        return IntegrityLevel::Unknown;
    }
    let mut size = 0u32;
    let _ = unsafe {
        GetTokenInformation(tok, TokenIntegrityLevel, None, 0, &mut size)
    };
    if size == 0 {
        let _ = unsafe { CloseHandle(tok) };
        return IntegrityLevel::Unknown;
    }
    let mut buf = vec![0u8; size as usize];
    let res = unsafe {
        GetTokenInformation(
            tok,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr() as _),
            size,
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(tok) };
    if res.is_err() {
        return IntegrityLevel::Unknown;
    }
    // The SID's last sub-authority encodes the IL: 0x2000=Low, 0x2000-0x3000=Medium,
    // 0x3000-0x4000=High. Read the count and last sub-authority via raw pointer.
    let label = unsafe { &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
    let sid = label.Label.Sid;
    if sid.0.is_null() {
        return IntegrityLevel::Unknown;
    }
    use windows::Win32::Security::{GetSidSubAuthority, GetSidSubAuthorityCount};
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return IntegrityLevel::Unknown;
    }
    let last = unsafe { *GetSidSubAuthority(sid, (count - 1) as u32) };
    match last {
        x if x < 0x2000 => IntegrityLevel::Low,
        x if x < 0x3000 => IntegrityLevel::Medium,
        x if x < 0x4000 => IntegrityLevel::High,
        _ => IntegrityLevel::High,
    }
}

/// Drop a cache entry. Called from process-exit / window-destroy hooks (already
/// wired in `fastuse-win/src/uia/cache.rs`).
pub fn invalidate_profile_cache(key: ProfileCacheKey) {
    cache().remove(&key);
}

/// Errors from profile construction.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// Phase-1 stub — replaced in Task 3.
    #[error("profile_window not yet implemented")]
    NotImplemented,
    /// HWND is no longer valid.
    #[error("window vanished or HWND invalid")]
    WindowGone,
    /// UIA pool unavailable.
    #[error("uia pool unavailable")]
    UiaUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_round_trip() {
        let k = ProfileCacheKey {
            hwnd: 0xdead_beef,
            pid: 4242,
            process_start: 0x0123_4567_89ab_cdef,
            generation: 7,
        };
        let copy = k;
        assert_eq!(k, copy);
    }

    #[test]
    fn read_class_name_of_invalid_hwnd_is_empty() {
        use windows::Win32::Foundation::HWND;
        let s = read_class_name(HWND(std::ptr::null_mut()));
        assert!(s.is_empty());
    }

    #[test]
    fn read_minimized_handles_invalid_hwnd() {
        use windows::Win32::Foundation::HWND;
        let _ = read_minimized(HWND(std::ptr::null_mut()));
        // Expectation: does not panic. Return value is meaningless for an
        // invalid HWND but the call must be safe.
    }

    #[test]
    fn flatten_filetime_layout() {
        let ft = FILETIME {
            dwHighDateTime: 0x1122_3344,
            dwLowDateTime: 0x5566_7788,
        };
        assert_eq!(
            ProfileCacheKey::flatten_filetime(ft),
            0x1122_3344_5566_7788_u64
        );
    }
}
