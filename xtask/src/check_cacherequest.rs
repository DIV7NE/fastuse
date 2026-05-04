//! Phase 3 UIA-12: forbid `Current[A-Z]*` accessor calls in
//! `crates/fastuse-win/src/uia/`.
//!
//! Rationale: UIA's `Current*` properties round-trip through COM per call,
//! costing 5–50ms each. The Phase 3 keystone discipline is that EVERY UIA
//! tree/element read is sourced from a single `IUIAutomationCacheRequest`
//! + `BuildUpdatedCache` pass; readers use `Cached*` accessors only.
//!
//! This lint walks every `*.rs` file under `crates/fastuse-win/src/` and
//! flags any `ExprMethodCall` whose method identifier matches the regex
//! `^Current[A-Z][A-Za-z0-9_]*$`. AST is used (not grep) so that doc
//! comments mentioning `Current*` (e.g. "do not use Current*") do not
//! false-positive — see the planner's "self-invalidating gate" rule.
//!
//! Standalone: `cargo xtask check-cacherequest`.
//! Aggregate: `cargo xtask lints`.

use std::path::Path;

use syn::visit::Visit;
use syn::{ExprMethodCall, File};

pub fn run(workspace_root: &Path) -> anyhow::Result<()> {
    let scan_root = workspace_root.join("crates").join("fastuse-win").join("src");
    let mut hits: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(&scan_root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(p)?;
        let file: File = match syn::parse_file(&src) {
            Ok(f) => f,
            Err(e) => {
                hits.push(format!("{}: parse error: {e}", p.display()));
                continue;
            }
        };
        let mut v = Checker {
            file_path: p.display().to_string(),
            hits: Vec::new(),
        };
        v.visit_file(&file);
        hits.extend(v.hits);
    }
    if !hits.is_empty() {
        anyhow::bail!(
            "check-cacherequest: {} forbidden Current* accessor(s) found:\n{}",
            hits.len(),
            hits.join("\n")
        );
    }
    Ok(())
}

struct Checker {
    file_path: String,
    hits: Vec<String>,
}

impl<'ast> Visit<'ast> for Checker {
    fn visit_expr_method_call(&mut self, mc: &'ast ExprMethodCall) {
        let m = mc.method.to_string();
        if is_current_accessor(&m) {
            self.hits.push(format!(
                "{}: forbidden Current* accessor: .{}(...)",
                self.file_path, m
            ));
        }
        // Recurse so nested chains (a.X().CurrentName()) are still scanned.
        syn::visit::visit_expr_method_call(self, mc);
    }
}

/// Match the regex `^Current[A-Z][A-Za-z0-9_]*$` without pulling in the
/// `regex` crate.
pub fn is_current_accessor(name: &str) -> bool {
    let mut chars = name.chars();
    // "Current"
    for expect in "Current".chars() {
        match chars.next() {
            Some(c) if c == expect => continue,
            _ => return false,
        }
    }
    // Next char must be ASCII uppercase A-Z.
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    // Remaining chars: word-ish (letter / digit / underscore).
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_current_name() {
        assert!(is_current_accessor("CurrentName"));
        assert!(is_current_accessor("CurrentBoundingRectangle"));
        assert!(is_current_accessor("CurrentControlType"));
        assert!(is_current_accessor("CurrentValueValue"));
    }

    #[test]
    fn rejects_non_current() {
        assert!(!is_current_accessor("CachedName"));
        assert!(!is_current_accessor("Name"));
        assert!(!is_current_accessor("Current")); // no trailing uppercase
        assert!(!is_current_accessor("currentname")); // lowercase
        assert!(!is_current_accessor("CurrentlyActive")); // 'l' not uppercase after Current
    }

    #[test]
    fn rejects_partial_or_prefixed() {
        assert!(!is_current_accessor("PreCurrentName"));
        assert!(!is_current_accessor(""));
    }

    #[test]
    fn ast_detects_planted_offender() {
        let src = r#"
            fn bad(elem: &Foo) -> String {
                elem.CurrentName().unwrap_or_default()
            }
            fn good(elem: &Foo) -> String {
                elem.CachedName().unwrap_or_default()
            }
        "#;
        let file: syn::File = syn::parse_str(src).unwrap();
        let mut v = Checker {
            file_path: "<test>".into(),
            hits: vec![],
        };
        v.visit_file(&file);
        assert_eq!(v.hits.len(), 1, "expected exactly one hit, got {:?}", v.hits);
        assert!(v.hits[0].contains("CurrentName"));
    }

    #[test]
    fn ast_ignores_doc_comments_mentioning_current() {
        // The exact pattern that motivated the AST approach: doc comments
        // forbidding Current* must not trigger the lint.
        let src = r#"
            /// Do not use Current* accessors. CurrentName is forbidden.
            fn good(elem: &Foo) -> String {
                elem.CachedName().unwrap_or_default()
            }
        "#;
        let file: syn::File = syn::parse_str(src).unwrap();
        let mut v = Checker {
            file_path: "<test>".into(),
            hits: vec![],
        };
        v.visit_file(&file);
        assert!(v.hits.is_empty(), "doc comment should not trigger: {:?}", v.hits);
    }
}
