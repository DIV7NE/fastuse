//! `inspect_at_point` handler (Phase 3 Task 15, UIA-04).
//!
//! Single-element resolution at a screen point. Builds a one-shot
//! CacheRequest tuned to populate the `UIANode` properties for that single
//! element (no children walk). Single round-trip.

use fastuse_proto::{Error as ProtoError, ErrorCode, Response};
use uiautomation::types::Point;

use crate::uia::automation::process_name_for_hwnd;
use crate::uia::degraded::detect_degraded;
use crate::uia::walk::{build_subtree_cache_request, walk_subtree};
use crate::uia_pool::UiaPoolHandle;

#[derive(serde::Serialize, serde::Deserialize)]
struct InspectRaw {
    node: fastuse_proto::UIANode,
    degraded: bool,
}

/// Handle a `Request::InspectAtPoint`.
pub fn handle_inspect_at_point(pool: &UiaPoolHandle, x: i32, y: i32) -> Result<Response, ProtoError> {
    let raw: InspectRaw = pool.run(move |uia| {
        let req = build_subtree_cache_request(uia, fastuse_proto::TreeView::Content)?;
        let element = uia
            .element_from_point_build_cache(Point::new(x, y), &req)
            .map_err(|e| {
                ProtoError::new(
                    ErrorCode::ElementNotFound,
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
    })?;
    Ok(Response::Inspect {
        node: raw.node,
        degraded: raw.degraded,
    })
}
