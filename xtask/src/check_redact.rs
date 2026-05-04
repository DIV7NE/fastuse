//! Verify Request/Response variants in fastuse-proto don't expose raw byte
//! payloads outside `Redact<T>`.
//!
//! Scope (WR-09): all `.rs` files under `crates/fastuse-proto/src` plus any
//! `handler*.rs` or `dispatch*.rs` under `crates/*/src` so payload-bearing
//! tool handlers added in later phases are caught the moment they declare a
//! suspicious field name or a bare `Vec<u8>`.

use std::path::Path;

use syn::{File, Item};

const SUSPICIOUS_FIELD_NAMES: &[&str] =
    &["payload", "text", "clipboard", "image_bytes", "secret", "password"];

pub fn run(workspace_root: &Path) -> anyhow::Result<()> {
    let mut failures: Vec<String> = Vec::new();
    let crates_dir = workspace_root.join("crates");
    for entry in walkdir::WalkDir::new(&crates_dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if !is_in_scope(p) {
            continue;
        }
        let src = std::fs::read_to_string(p)?;
        if let Err(msg) = check_source(&src) {
            failures.push(format!("{}: {}", p.display(), msg));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("check-redact failures:\n{}", failures.join("\n"));
    }
    Ok(())
}

/// In-scope files: everything under `fastuse-proto/src`, plus any
/// `handler*.rs` / `dispatch*.rs` under `crates/*/src` (WR-09).
fn is_in_scope(p: &Path) -> bool {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.contains("/fastuse-proto/src/") {
        return true;
    }
    let fname = p
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    (fname.starts_with("handler") || fname.starts_with("dispatch"))
        && fname.ends_with(".rs")
        && s.contains("/src/")
}

pub fn check_source(src: &str) -> Result<(), String> {
    let file: File = syn::parse_file(src).map_err(|e| format!("parse: {e}"))?;
    let mut violations: Vec<String> = Vec::new();
    for item in &file.items {
        if let Item::Enum(e) = item {
            for variant in &e.variants {
                for f in &variant.fields {
                    if let Some(name) = &f.ident {
                        let n = name.to_string();
                        if SUSPICIOUS_FIELD_NAMES.contains(&n.as_str()) {
                            let ty = quote_ty(&f.ty);
                            if !ty.contains("Redact") {
                                violations.push(format!(
                                    "{}::{}: field `{}: {}` should be Redact<{}>",
                                    e.ident, variant.ident, n, ty, ty
                                ));
                            }
                        }
                        // Also flag bare Vec<u8> regardless of name in proto.
                        let ty = quote_ty(&f.ty);
                        if (ty.contains("Vec < u8 >") || ty.contains("Vec<u8>"))
                            && !ty.contains("Redact")
                        {
                            violations.push(format!(
                                "{}::{}: field `{}: {}` is raw Vec<u8>; wrap in Redact<Vec<u8>>",
                                e.ident, variant.ident, n, ty
                            ));
                        }
                    }
                }
            }
        }
    }
    if !violations.is_empty() {
        return Err(violations.join("; "));
    }
    Ok(())
}

fn quote_ty(t: &syn::Type) -> String {
    use quote::ToTokens;
    let mut ts = proc_macro2::TokenStream::new();
    t.to_tokens(&mut ts);
    ts.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_clean() {
        let src = r#"
            pub enum Request {
                Hello { client: String },
                Type { text: Redact<String> },
            }
        "#;
        assert!(check_source(src).is_ok());
    }

    #[test]
    fn negative_raw_payload() {
        let src = r#"
            pub enum Request {
                Type { payload: Vec<u8> },
            }
        "#;
        assert!(check_source(src).is_err());
    }

    #[test]
    fn negative_text_string() {
        let src = r#"
            pub enum Request {
                Type { text: String },
            }
        "#;
        assert!(check_source(src).is_err());
    }
}
