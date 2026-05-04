//! `uia_tree` handler (Phase 3 Task 13, UIA-02 surface).
//!
//! Resolves the root via the foreground-HWND cache (UIA-09), walks the
//! subtree once via `walk::walk_subtree` (UIA-12 keystone), and applies
//! the degraded heuristic (UIA-10) for the response payload.

use fastuse_proto::{Error as ProtoError, ErrorCode, Response, TreeView};

use crate::uia::automation::{foreground_hwnd, process_name_for_hwnd};
use crate::uia::cache::get_or_fetch;
use crate::uia::degraded::detect_degraded;
use crate::uia::walk::walk_subtree;
use crate::uia_pool::UiaPoolHandle;

/// Wire-typed payload sent across the pool closure boundary
/// (`UiaPoolHandle::run` requires Serialize+DeserializeOwned).
#[derive(serde::Serialize, serde::Deserialize)]
struct TreeRaw {
    root: fastuse_proto::UIANode,
    degraded: bool,
}

/// Handle a `Request::UiaTree`.
pub fn handle_uia_tree(
    pool: &UiaPoolHandle,
    hwnd: Option<u64>,
    depth: Option<u32>,
    view: Option<TreeView>,
) -> Result<Response, ProtoError> {
    let hwnd = match hwnd.or_else(foreground_hwnd) {
        Some(h) => h,
        None => {
            return Err(ProtoError::new(
                ErrorCode::WindowNotFound,
                "no foreground window".to_string(),
            ));
        }
    };
    let view = view.unwrap_or_default();
    let raw: TreeRaw = pool.run(move |uia| {
        let root = get_or_fetch(uia, hwnd)?;
        let tree = walk_subtree(uia, &root, view, depth)?;
        let degraded = detect_degraded(&tree, process_name_for_hwnd(hwnd).as_deref());
        Ok(TreeRaw {
            root: tree,
            degraded,
        })
    })?;
    Ok(Response::UiaTree {
        root: raw.root,
        degraded: raw.degraded,
    })
}
