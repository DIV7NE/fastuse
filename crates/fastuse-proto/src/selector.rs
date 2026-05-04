//! UIA selector grammar (Phase 3 UIA-03).
//!
//! A small, expressive selector tree the MCP/CLI client sends to the
//! daemon. The daemon resolves selectors via `fastuse-win::uia::find` which
//! walks ONCE through `walk::walk_subtree` (Task 09) and filters via
//! [`Selector::matches`].
//!
//! Lives in `fastuse-proto` so both client and daemon share the exact
//! shape (postcard wire + serde-JSON at the MCP edge).

use serde::{Deserialize, Serialize};

use crate::uia_node::{ControlType, UIANode};

/// Composable selector for matching `UIANode`s.
///
/// Leaves match against a single property; combinators compose via
/// boolean truth tables. Matching is intentionally cheap — the heavy
/// work is the one-pass cache walk that produces the candidate nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Selector {
    /// `UIA_NamePropertyId` exact match.
    ByName(String),
    /// `UIA_AutomationIdPropertyId` exact match.
    ByAutomationId(String),
    /// `UIA_ControlTypePropertyId` mapped match.
    ByControlType(ControlType),
    /// `UIA_ClassNamePropertyId` exact match.
    ByClass(String),
    /// All sub-selectors must match (logical AND).
    And(Vec<Selector>),
    /// At least one sub-selector must match (logical OR).
    Or(Vec<Selector>),
    /// Inverts the inner selector.
    Not(Box<Selector>),
}

impl Selector {
    /// True if `node` satisfies this selector.
    pub fn matches(&self, node: &UIANode) -> bool {
        match self {
            Selector::ByName(s) => node.name == *s,
            Selector::ByAutomationId(s) => node.automation_id == *s,
            Selector::ByControlType(ct) => node.control_type == *ct,
            Selector::ByClass(s) => node.class_name == *s,
            Selector::And(xs) => xs.iter().all(|s| s.matches(node)),
            Selector::Or(xs) => xs.iter().any(|s| s.matches(node)),
            Selector::Not(inner) => !inner.matches(node),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::Rect;
    use crate::wire::{decode_frame, encode_frame};

    fn n(name: &str, ct: ControlType) -> UIANode {
        UIANode {
            name: name.into(),
            automation_id: String::new(),
            class_name: String::new(),
            control_type: ct,
            localized_control_type: String::new(),
            bounding_rect: Rect { x: 0, y: 0, w: 0, h: 0 },
            is_enabled: false,
            is_keyboard_focusable: false,
            help_text: String::new(),
            value: None,
            children: vec![],
        }
    }

    #[test]
    fn by_name_matches() {
        assert!(Selector::ByName("OK".into()).matches(&n("OK", ControlType::Button)));
        assert!(!Selector::ByName("OK".into()).matches(&n("Cancel", ControlType::Button)));
    }

    #[test]
    fn by_control_type_matches() {
        let s = Selector::ByControlType(ControlType::Button);
        assert!(s.matches(&n("X", ControlType::Button)));
        assert!(!s.matches(&n("X", ControlType::Edit)));
    }

    #[test]
    fn and_requires_all() {
        let s = Selector::And(vec![
            Selector::ByName("OK".into()),
            Selector::ByControlType(ControlType::Button),
        ]);
        assert!(s.matches(&n("OK", ControlType::Button)));
        assert!(!s.matches(&n("OK", ControlType::Edit)));
        assert!(!s.matches(&n("Cancel", ControlType::Button)));
    }

    #[test]
    fn or_requires_any() {
        let s = Selector::Or(vec![
            Selector::ByName("OK".into()),
            Selector::ByName("Cancel".into()),
        ]);
        assert!(s.matches(&n("OK", ControlType::Button)));
        assert!(s.matches(&n("Cancel", ControlType::Button)));
        assert!(!s.matches(&n("Other", ControlType::Button)));
    }

    #[test]
    fn not_inverts() {
        let s = Selector::Not(Box::new(Selector::ByName("OK".into())));
        assert!(!s.matches(&n("OK", ControlType::Button)));
        assert!(s.matches(&n("Cancel", ControlType::Button)));
    }

    #[test]
    fn empty_and_is_vacuously_true() {
        let s = Selector::And(vec![]);
        assert!(s.matches(&n("anything", ControlType::Custom)));
    }

    #[test]
    fn empty_or_is_vacuously_false() {
        let s = Selector::Or(vec![]);
        assert!(!s.matches(&n("anything", ControlType::Custom)));
    }

    #[test]
    fn postcard_round_trip() {
        let s = Selector::And(vec![
            Selector::ByName("OK".into()),
            Selector::Or(vec![
                Selector::ByControlType(ControlType::Button),
                Selector::Not(Box::new(Selector::ByClass("Static".into()))),
            ]),
        ]);
        let bytes = encode_frame(&s).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        let decoded: Selector = decode_frame(&mut cur).unwrap();
        assert_eq!(s, decoded);
    }
}
