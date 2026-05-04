//! `uia_query` handler (Phase 3 Task 14, UIA-03 / UIA-11 surface).
//!
//! Same root-resolution path as `tree.rs`; matches via `find::find_all`
//! which performs ONE `walk_subtree` and filters via `Selector::matches`.

use fastuse_proto::{Error as ProtoError, ErrorCode, Response, Selector};

use crate::uia::automation::{foreground_hwnd, process_name_for_hwnd};
use crate::uia::cache::get_or_fetch;
use crate::uia::degraded::detect_degraded;
use crate::uia::find::collect_matching;
use crate::uia::walk::walk_subtree;
use crate::uia_pool::UiaPoolHandle;

#[derive(serde::Serialize, serde::Deserialize)]
struct QueryRaw {
    matches: Vec<fastuse_proto::UIANode>,
    degraded: bool,
}

/// Handle a `Request::UiaQuery`.
pub fn handle_uia_query(
    pool: &UiaPoolHandle,
    selector: Selector,
    root_hwnd: Option<u64>,
) -> Result<Response, ProtoError> {
    let hwnd = match root_hwnd.or_else(foreground_hwnd) {
        Some(h) => h,
        None => {
            return Err(ProtoError::new(
                ErrorCode::WindowNotFound,
                "no foreground window".to_string(),
            ));
        }
    };
    let raw: QueryRaw = pool.run(move |uia| {
        let root = get_or_fetch(uia, hwnd)?;
        // CR-02: single BuildUpdatedCache walk per call (PLAN Task 10
        // contract: "Only one walk per call"; UIA-11 budget: <20ms p99
        // on 1000-node tree). The walked `UIANode` tree feeds BOTH the
        // degraded heuristic AND the selector match — no second walk.
        let tree = walk_subtree(uia, &root, fastuse_proto::TreeView::Content, None)?;
        let degraded = detect_degraded(&tree, process_name_for_hwnd(hwnd).as_deref());
        let mut matches = Vec::new();
        collect_matching(&tree, &selector, &mut matches);
        Ok(QueryRaw {
            matches,
            degraded,
        })
    })?;
    Ok(Response::UiaQuery {
        matches: raw.matches,
        degraded: raw.degraded,
    })
}
