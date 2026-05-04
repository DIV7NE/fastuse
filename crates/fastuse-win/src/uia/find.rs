//! Selector resolution (Phase 3 Task 10, UIA-03 backbone).
//!
//! Both `find_first` and `find_all` walk the subtree ONCE via `walk::walk_subtree`
//! (the canonical CacheRequest reader) and then filter in-process via
//! `Selector::matches`. **Never re-walks.**

use fastuse_proto::{Selector, TreeView, UIANode};
use uiautomation::{UIAutomation, UIElement};

use crate::uia::walk::walk_subtree;

/// Walk root once and collect every matching node.
pub fn find_all(
    uia: &UIAutomation,
    root: &UIElement,
    selector: &Selector,
) -> Result<Vec<UIANode>, fastuse_proto::Error> {
    let tree = walk_subtree(uia, root, TreeView::Content, None)?;
    let mut out = Vec::new();
    collect_matching(&tree, selector, &mut out);
    Ok(out)
}

/// Walk root once and return the first matching node (DFS pre-order).
pub fn find_first(
    uia: &UIAutomation,
    root: &UIElement,
    selector: &Selector,
) -> Result<Option<UIANode>, fastuse_proto::Error> {
    let tree = walk_subtree(uia, root, TreeView::Content, None)?;
    Ok(first_matching(&tree, selector))
}

fn collect_matching(node: &UIANode, selector: &Selector, out: &mut Vec<UIANode>) {
    if selector.matches(node) {
        out.push(node.clone());
    }
    for child in &node.children {
        collect_matching(child, selector, out);
    }
}

fn first_matching(node: &UIANode, selector: &Selector) -> Option<UIANode> {
    if selector.matches(node) {
        return Some(node.clone());
    }
    for child in &node.children {
        if let Some(hit) = first_matching(child, selector) {
            return Some(hit);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::{ControlType, Selector};

    fn leaf(name: &str, ct: ControlType) -> UIANode {
        let mut n = UIANode::empty();
        n.name = name.into();
        n.control_type = ct;
        n
    }

    #[test]
    fn collect_finds_two_buttons() {
        let mut root = UIANode::empty();
        root.name = "root".into();
        root.children = vec![
            leaf("OK", ControlType::Button),
            leaf("Cancel", ControlType::Button),
            leaf("Title", ControlType::Text),
        ];
        let sel = Selector::ByControlType(ControlType::Button);
        let mut out = Vec::new();
        collect_matching(&root, &sel, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn first_matches_dfs_preorder() {
        let mut root = UIANode::empty();
        root.children = vec![
            leaf("first", ControlType::Button),
            leaf("second", ControlType::Button),
        ];
        let sel = Selector::ByName("second".into());
        let hit = first_matching(&root, &sel).expect("found");
        assert_eq!(hit.name, "second");
    }
}
