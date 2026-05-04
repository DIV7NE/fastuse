//! Degraded-tree heuristic (Phase 3 UIA-10).
//!
//! Used by `uia_tree`, `uia_query`, `inspect_at_point` to set the
//! `degraded` field on the response so agents can fall back to OCR /
//! image-based perception when UIA returns a sparse tree from
//! Electron / JavaFX / Discord.
//!
//! Heuristic:
//! 1. Total descendant node count under [`MIN_NODE_COUNT`] AND the
//!    bounding rect is larger than [`MIN_DEGENERATE_DIM`] x
//!    [`MIN_DEGENERATE_DIM`] pixels — likely a degraded host.
//! 2. The owning process name matches [`KNOWN_DEGRADED_PROCESSES`] —
//!    overrides the count check (some Electron apps publish a partial
//!    accessible tree that would otherwise pass the count test).

use fastuse_proto::UIANode;

/// Minimum descendant count for a "rich enough" tree.
pub const MIN_NODE_COUNT: usize = 5;

/// A window must be at least this many pixels wide AND tall before the
/// degenerate-count branch fires; smaller windows (toasts, popups) can
/// legitimately have <5 nodes.
pub const MIN_DEGENERATE_DIM: i32 = 200;

/// Process basenames that publish chronically degraded UIA trees.
/// Matched case-insensitively. Maintained as a hardcoded allowlist —
/// verified empirically and updated via PR.
pub const KNOWN_DEGRADED_PROCESSES: &[&str] = &[
    "discord.exe",
    "slack.exe",
    "teams.exe",
    "code.exe",       // VS Code (Electron)
    "idea64.exe",     // JetBrains (Swing/JavaFX)
    "studio64.exe",   // Android Studio
    "rider64.exe",    // JetBrains Rider
    "webstorm64.exe", // JetBrains WebStorm
];

/// Count this node and every transitive child.
fn descendant_count(n: &UIANode) -> usize {
    1 + n.children.iter().map(descendant_count).sum::<usize>()
}

/// Apply the degraded heuristic against a walked subtree root.
///
/// `process_name` should be the owning HWND's process basename (e.g.
/// "discord.exe") in any case; lowercased internally for comparison.
/// Pass `None` if the process name is unavailable; the count branch
/// still runs.
pub fn detect_degraded(root: &UIANode, process_name: Option<&str>) -> bool {
    if let Some(name) = process_name {
        let name_lc = name.to_ascii_lowercase();
        if KNOWN_DEGRADED_PROCESSES
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&name_lc))
        {
            return true;
        }
    }
    let r = root.bounding_rect;
    let big_enough = r.w > MIN_DEGENERATE_DIM && r.h > MIN_DEGENERATE_DIM;
    let count = descendant_count(root);
    big_enough && count < MIN_NODE_COUNT
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::coords::Rect;
    use fastuse_proto::ControlType;

    fn node(rect: Rect, children: Vec<UIANode>) -> UIANode {
        UIANode {
            name: String::new(),
            automation_id: String::new(),
            class_name: String::new(),
            control_type: ControlType::Custom,
            localized_control_type: String::new(),
            bounding_rect: rect,
            is_enabled: false,
            is_keyboard_focusable: false,
            help_text: String::new(),
            value: None,
            children,
        }
    }

    fn rect(w: i32, h: i32) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    #[test]
    fn three_node_root_on_large_window_is_degraded() {
        // root + 2 children = 3 nodes; rect 800x600 (> 200x200).
        let root = node(
            rect(800, 600),
            vec![node(rect(10, 10), vec![]), node(rect(10, 10), vec![])],
        );
        assert!(detect_degraded(&root, None));
    }

    #[test]
    fn rich_tree_is_not_degraded() {
        // 6 nodes total: root + 5 children.
        let kids = (0..5).map(|_| node(rect(10, 10), vec![])).collect();
        let root = node(rect(800, 600), kids);
        assert!(!detect_degraded(&root, None));
    }

    #[test]
    fn small_window_with_few_nodes_is_not_degraded() {
        // Toast-sized 100x100 window with 1 node — legitimate, not degraded.
        let root = node(rect(100, 100), vec![]);
        assert!(!detect_degraded(&root, None));
    }

    #[test]
    fn process_name_match_overrides_count() {
        // Rich tree (10 nodes), but process is Discord — degraded by override.
        let kids = (0..9).map(|_| node(rect(10, 10), vec![])).collect();
        let root = node(rect(1200, 800), kids);
        assert!(detect_degraded(&root, Some("Discord.exe")));
        assert!(detect_degraded(&root, Some("discord.exe")));
        assert!(detect_degraded(&root, Some("DISCORD.EXE")));
    }

    #[test]
    fn unknown_process_name_falls_through_to_count() {
        // Rich tree, unknown process — not degraded.
        let kids = (0..9).map(|_| node(rect(10, 10), vec![])).collect();
        let root = node(rect(1200, 800), kids);
        assert!(!detect_degraded(&root, Some("notepad.exe")));
    }

    #[test]
    fn known_degraded_list_is_complete_set() {
        // Sanity: list contains the apps research PITFALLS #6 calls out.
        let names: Vec<&str> = KNOWN_DEGRADED_PROCESSES.iter().copied().collect();
        for must in &["discord.exe", "slack.exe", "code.exe", "idea64.exe"] {
            assert!(names.contains(must), "missing {must} from list");
        }
    }
}
