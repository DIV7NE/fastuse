//! Verify no `tokio::spawn(...)` body or `#[tokio::main]` async fn body
//! contains a `windows::*` or `uiautomation::*` path reference.
//!
//! # Allowed MTA / STA COM-thread hosts (D-25)
//!
//! `fastuse-win` is excluded from this scan by crate filter (see `run`).
//! Inside that crate, the only modules permitted to host a dedicated COM
//! thread (and therefore the only ones permitted to call into `windows::*`
//! activation factories on a non-tokio thread) are:
//!
//! - `input_thread` — STA, hosts SendInput synthesis
//! - `uia_pool` — MTA worker pool, hosts `IUIAutomation` queries
//! - `capture_thread` — MTA, hosts D3D11 + DXGI desktop duplication
//!
//! Adding a fourth COM-thread surface requires extending this list.
//!
//! # Known limits (WR-08)
//!
//! This lint is intentionally heuristic. Reviewers should not trust it
//! beyond the cases it actually inspects:
//!
//! 1. **First-segment match only.** `ForbiddenPathScanner::visit_expr_path`
//!    looks at `segments.first()`. A `use windows::Win32::Foundation::HWND;`
//!    followed by bare `HWND::default()` inside a tokio task is invisible to
//!    this lint. So is any aliased import (`use windows as w; w::...`).
//! 2. **`tokio::spawn` + `#[tokio::main]` only.** Manual runtimes built via
//!    `tokio::runtime::Builder::new_multi_thread().build()` followed by
//!    `rt.block_on(...)` are NOT scanned. The daemon currently uses this
//!    pattern in `fastuse-daemon::main`, so async code reachable from
//!    `server::serve` / `dispatch::handle` is not lint-covered.
//! 3. **Type paths and method-call receivers.** `visit_expr_path` ignores
//!    `Type` nodes (turbofish, generics) and method-call segments.
//!
//! Treat a green run as "the obvious cases are clean," not "no Win32 work
//! happens on tokio threads." The semantic guarantee comes from D-26 and
//! review discipline; this lint catches regressions only at the syntactic
//! shapes enumerated above.

use std::path::Path;

use syn::visit::Visit;
use syn::{Expr, ExprPath, File, ItemFn};

pub fn run(workspace_root: &Path) -> anyhow::Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(workspace_root.join("crates"))
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        // Only check daemon/mcp/cli/core where tokio is in play; fastuse-win
        // is *expected* to use windows::*.
        let s = p.display().to_string();
        if !(s.contains("fastuse-daemon") || s.contains("fastuse-mcp") || s.contains("fastuse-cli")) {
            continue;
        }
        let src = std::fs::read_to_string(p)?;
        if let Err(msg) = check_source(&src) {
            failures.push(format!("{}: {}", p.display(), msg));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("check-com failures:\n{}", failures.join("\n"));
    }
    Ok(())
}

pub fn check_source(src: &str) -> Result<(), String> {
    let file: File = syn::parse_file(src).map_err(|e| format!("parse: {e}"))?;
    let mut v = Checker { violations: vec![] };
    v.visit_file(&file);
    if !v.violations.is_empty() {
        return Err(v.violations.join("; "));
    }
    Ok(())
}

#[derive(Default)]
struct Checker {
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for Checker {
    fn visit_item_fn(&mut self, f: &'ast ItemFn) {
        // tokio::main attribute -> scan the body for forbidden paths.
        let is_tokio_main = f.attrs.iter().any(|a| {
            a.path()
                .segments
                .iter()
                .any(|s| s.ident == "tokio")
                && a.path()
                    .segments
                    .iter()
                    .any(|s| s.ident == "main")
        });
        if is_tokio_main {
            if let Some(bad) = scan_block_for_forbidden(&f.block) {
                self.violations.push(format!(
                    "fn {}: tokio::main body references forbidden path `{}`",
                    f.sig.ident, bad
                ));
            }
        }
        syn::visit::visit_item_fn(self, f);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // tokio::spawn(closure) or tokio::task::spawn(closure)
        let is_spawn = match &*call.func {
            Expr::Path(p) => path_ends_with(&p.path, "spawn") && path_contains_seg(&p.path, "tokio"),
            _ => false,
        };
        if is_spawn {
            for arg in &call.args {
                if let Expr::Closure(cl) = arg {
                    if let Expr::Block(b) = &*cl.body {
                        if let Some(bad) = scan_block_for_forbidden(&b.block) {
                            self.violations.push(format!(
                                "tokio::spawn closure references forbidden path `{}`",
                                bad
                            ));
                        }
                    }
                }
                if let Expr::Async(a) = arg {
                    if let Some(bad) = scan_block_for_forbidden(&a.block) {
                        self.violations.push(format!(
                            "tokio::spawn async block references forbidden path `{}`",
                            bad
                        ));
                    }
                }
            }
        }
        syn::visit::visit_expr_call(self, call);
    }
}

fn path_ends_with(path: &syn::Path, name: &str) -> bool {
    path.segments.last().map(|s| s.ident == name).unwrap_or(false)
}

fn path_contains_seg(path: &syn::Path, name: &str) -> bool {
    path.segments.iter().any(|s| s.ident == name)
}

fn scan_block_for_forbidden(block: &syn::Block) -> Option<String> {
    let mut s = ForbiddenPathScanner { found: None };
    s.visit_block(block);
    s.found
}

struct ForbiddenPathScanner {
    found: Option<String>,
}

impl<'ast> Visit<'ast> for ForbiddenPathScanner {
    fn visit_expr_path(&mut self, ep: &'ast ExprPath) {
        if self.found.is_some() {
            return;
        }
        if let Some(seg0) = ep.path.segments.first() {
            if seg0.ident == "windows" || seg0.ident == "uiautomation" {
                self.found = Some(format!("{}", seg0.ident));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_clean() {
        let src = r#"
            fn main() {
                tokio::spawn(async {
                    let x = 1;
                    println!("ok");
                });
            }
        "#;
        assert!(check_source(src).is_ok());
    }

    #[test]
    fn negative_windows_in_spawn() {
        let src = r#"
            fn main() {
                tokio::spawn(async {
                    let _ = windows::Win32::Foundation::HWND::default();
                });
            }
        "#;
        assert!(check_source(src).is_err());
    }

    #[test]
    fn negative_uia_in_tokio_main() {
        let src = r#"
            #[tokio::main]
            async fn main() {
                let _ = uiautomation::UIAutomation::new();
            }
        "#;
        assert!(check_source(src).is_err());
    }
}
