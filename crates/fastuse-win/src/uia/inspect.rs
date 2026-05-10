//! `inspect_at_point` handler (Phase 3 Task 15, UIA-04).
//!
//! Single-element resolution at a screen point. Builds a one-shot
//! CacheRequest tuned to populate the `UIANode` properties for that single
//! element (no children walk). Single round-trip.
//!
//! v2.3.0: when UIA can't resolve an element at the probe (typical of
//! custom-rendered ImGui / Direct2D regions), fall back to
//! `WindowFromPoint` and return a `Response::InspectPixelOnly` carrying the
//! owning HWND, title, and process name. Lets callers branch to vision
//! (screenshot + click) without losing the "which app are we in" signal.

use fastuse_proto::{Error as ProtoError, Response};
use uiautomation::types::Point;

use crate::uia::automation::process_name_for_hwnd;
use crate::uia::degraded::detect_degraded;
use crate::uia::walk::{build_subtree_cache_request, walk_subtree};
use crate::uia_pool::UiaPoolHandle;
use crate::window::build_window_info;

#[derive(serde::Serialize, serde::Deserialize)]
struct InspectRaw {
    node: fastuse_proto::UIANode,
    degraded: bool,
}

/// Handle a `Request::InspectAtPoint`.
pub fn handle_inspect_at_point(pool: &UiaPoolHandle, x: i32, y: i32) -> Result<Response, ProtoError> {
    let uia_result: Result<InspectRaw, ProtoError> = pool.run(move |uia| {
        let req = build_subtree_cache_request(uia, fastuse_proto::TreeView::Content)?;
        let element = uia
            .element_from_point_build_cache(Point::new(x, y), &req)
            .map_err(|e| {
                ProtoError::new(
                    fastuse_proto::ErrorCode::ElementNotFound,
                    format!("element_from_point({x},{y}): {e}"),
                )
            })?;
        // The CacheRequest scope is Subtree, so the element's children are
        // already cached — reuse walk_subtree for a consistent shape but
        // with depth=Some(0) so we get just the focused element back.
        let node = walk_subtree(uia, &element, fastuse_proto::TreeView::Content, Some(0))?;
        // Owning HWND from the cached native window handle for degraded
        // process-name lookup.
        let owning_hwnd = element.get_cached_native_window_handle().ok().map(|h| {
            // uiautomation::types::Handle wraps HWND; cast HWND -> u64 via raw pointer.
            let hwnd: windows::Win32::Foundation::HWND = h.into();
            hwnd.0 as u64
        });
        let degraded = match owning_hwnd {
            Some(h) => detect_degraded(&node, process_name_for_hwnd(h).as_deref()),
            None => detect_degraded(&node, None),
        };
        Ok(InspectRaw { node, degraded })
    });

    match uia_result {
        Ok(raw) => Ok(Response::Inspect {
            node: raw.node,
            degraded: raw.degraded,
        }),
        Err(_) => {
            // UIA couldn't see anything at this point. If a real window is
            // there, surface the pixel-only fallback so callers know to
            // switch to vision instead of getting a bare ElementNotFound.
            use windows::Win32::Foundation::POINT;
            use windows::Win32::UI::WindowsAndMessaging::WindowFromPoint;
            // SAFETY: WindowFromPoint accepts arbitrary POINT; returns null
            // HWND when no window is hit.
            let hwnd = unsafe { WindowFromPoint(POINT { x, y }) };
            if hwnd.0 as isize == 0 {
                // No window under the point — propagate the original UIA
                // failure unchanged.
                return uia_result.map(|r| Response::Inspect {
                    node: r.node,
                    degraded: r.degraded,
                });
            }
            // Climb to the top-level so the reported HWND/title is useful for
            // caller branching (the immediate child HWND is usually a render
            // surface that means nothing on its own).
            let top = top_level_of(hwnd);
            let info = build_window_info(top)?;
            Ok(Response::InspectPixelOnly {
                hwnd: info.hwnd,
                window_title: info.title,
                process_name: info.process_name,
            })
        }
    }
}

/// Walk up via `GetAncestor(GA_ROOT)` so we report the top-level window
/// (taskbar-visible) rather than an inner child / render surface.
fn top_level_of(h: windows::Win32::Foundation::HWND) -> windows::Win32::Foundation::HWND {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_ROOT};
    // SAFETY: GetAncestor accepts any HWND; returns same HWND for top-level.
    unsafe { GetAncestor(h, GA_ROOT) }
}
