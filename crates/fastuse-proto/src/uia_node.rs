//! UIA node shape on the wire (Phase 3 UIA-02..08).
//!
//! `UIANode` is the canonical UIA element representation that crosses the
//! daemon ↔ client pipe. It is populated from `IUIAutomationCacheRequest` /
//! `BuildUpdatedCache` results inside `fastuse-win::uia::walk` (Task 09);
//! every property on this struct MUST be sourced from a cache read, never
//! from a `Current*` accessor (UIA-12, lint enforced by `xtask
//! check-cacherequest`).
//!
//! `value` is wrapped in [`Redact<String>`] so password-like fields never
//! leak through Debug/tracing (T-03-02).

use serde::{Deserialize, Serialize};

use crate::coords::Rect;
use crate::redact::Redact;

/// Stable, serde-mapped subset of UIA `UIA_*_ControlTypeId` constants
/// surfaced to MCP/CLI clients. Matches the closed set the selector
/// grammar can target. Unknown control types fall through to `Custom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ControlType {
    /// `UIA_ButtonControlTypeId`.
    Button,
    /// `UIA_EditControlTypeId` (text input).
    Edit,
    /// `UIA_TextControlTypeId` (read-only label).
    Text,
    /// `UIA_ComboBoxControlTypeId`.
    ComboBox,
    /// `UIA_ListControlTypeId`.
    List,
    /// `UIA_ListItemControlTypeId`.
    ListItem,
    /// `UIA_MenuItemControlTypeId`.
    MenuItem,
    /// `UIA_TabControlTypeId`.
    Tab,
    /// `UIA_TabItemControlTypeId`.
    TabItem,
    /// `UIA_HyperlinkControlTypeId`.
    Hyperlink,
    /// `UIA_WindowControlTypeId`.
    Window,
    /// `UIA_PaneControlTypeId`.
    Pane,
    /// `UIA_GroupControlTypeId`.
    Group,
    /// `UIA_CheckBoxControlTypeId`.
    CheckBox,
    /// `UIA_RadioButtonControlTypeId`.
    RadioButton,
    /// Anything outside the closed set above.
    Custom,
}

/// Tree walker view selection. Default `Content` (skips chrome).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TreeView {
    /// `IUIAutomation::ContentViewWalker`.
    Content,
    /// `IUIAutomation::RawViewWalker`.
    Raw,
}

impl Default for TreeView {
    fn default() -> Self {
        TreeView::Content
    }
}

/// Image format for screenshot encoding (Phase 3 CAP-05). JPEG default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ImageFormat {
    /// JPEG q=85 (default).
    Jpeg,
    /// PNG (opt-in; `mtpng` feature parallelizes 4K).
    Png,
}

impl Default for ImageFormat {
    fn default() -> Self {
        ImageFormat::Jpeg
    }
}

/// Cache-fetched UIA element node (Phase 3 UIA-02..08).
///
/// Populated by `walk::walk_subtree` from a single `BuildUpdatedCache`
/// pass. `value` is `Redact<String>` so password-like fields never leak
/// through Debug/Display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UIANode {
    /// `UIA_NamePropertyId`.
    pub name: String,
    /// `UIA_AutomationIdPropertyId` — stable when authored.
    pub automation_id: String,
    /// `UIA_ClassNamePropertyId`.
    pub class_name: String,
    /// Mapped `UIA_ControlTypePropertyId`.
    pub control_type: ControlType,
    /// Localized `UIA_LocalizedControlTypePropertyId` (fallback display label).
    pub localized_control_type: String,
    /// `UIA_BoundingRectanglePropertyId` in physical pixels (virtual desktop).
    pub bounding_rect: Rect,
    /// `UIA_IsEnabledPropertyId`.
    pub is_enabled: bool,
    /// `UIA_IsKeyboardFocusablePropertyId`.
    pub is_keyboard_focusable: bool,
    /// `UIA_HelpTextPropertyId`.
    pub help_text: String,
    /// `UIA_ValueValuePropertyId` — wrapped to redact passwords (T-03-02).
    pub value: Option<Redact<String>>,
    /// Cached children (already walked).
    pub children: Vec<UIANode>,
}

impl UIANode {
    /// Synthetic empty node — useful for unit tests of the selector grammar
    /// before the real UIA backend lands.
    pub fn empty() -> Self {
        Self {
            name: String::new(),
            automation_id: String::new(),
            class_name: String::new(),
            control_type: ControlType::Custom,
            localized_control_type: String::new(),
            bounding_rect: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            is_enabled: false,
            is_keyboard_focusable: false,
            help_text: String::new(),
            value: None,
            children: Vec::new(),
        }
    }
}
