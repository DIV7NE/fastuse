//! Verify every `crates/*/src/main.rs` has `set_per_monitor_v2_first_call`
//! as the first executable statement of `fn main`.

use std::path::Path;

use syn::{File, Item, Stmt, Expr};

pub const TARGET_FN: &str = "set_per_monitor_v2_first_call";

pub fn run(workspace_root: &Path) -> anyhow::Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(workspace_root.join("crates"))
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        if !p.is_file() || p.file_name() != Some(std::ffi::OsStr::new("main.rs")) {
            continue;
        }
        let src = std::fs::read_to_string(p)?;
        if let Err(msg) = check_source(&src) {
            failures.push(format!("{}: {}", p.display(), msg));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("check-firstcall failures:\n{}", failures.join("\n"));
    }
    Ok(())
}

pub fn check_source(src: &str) -> Result<(), String> {
    let file: File = syn::parse_file(src).map_err(|e| format!("parse: {e}"))?;
    let main_fn = file.items.iter().find_map(|i| match i {
        Item::Fn(f) if f.sig.ident == "main" => Some(f),
        _ => None,
    });
    let f = match main_fn {
        Some(f) => f,
        None => return Ok(()), // No main fn — non-bin file; skip.
    };
    let first = match f.block.stmts.first() {
        Some(s) => s,
        None => return Err("main() body is empty".into()),
    };
    if !is_target_call(first) {
        return Err(format!("first stmt of main() is not {TARGET_FN}()"));
    }
    Ok(())
}

fn is_target_call(stmt: &Stmt) -> bool {
    let expr = match stmt {
        Stmt::Expr(e, _) => e,
        Stmt::Local(l) => match l.init.as_ref().map(|i| &*i.expr) {
            Some(e) => e,
            None => return false,
        },
        _ => return false,
    };
    match expr {
        Expr::Call(call) => match &*call.func {
            Expr::Path(p) => p
                .path
                .segments
                .last()
                .map(|s| s.ident == TARGET_FN)
                .unwrap_or(false),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_first_stmt() {
        let src = r#"
            fn main() {
                set_per_monitor_v2_first_call();
                println!("hi");
            }
        "#;
        assert!(check_source(src).is_ok());
    }

    #[test]
    fn positive_path_qualified() {
        let src = r#"
            fn main() {
                fastuse_win::set_per_monitor_v2_first_call();
            }
        "#;
        assert!(check_source(src).is_ok());
    }

    #[test]
    fn negative_other_first_stmt() {
        let src = r#"
            fn main() {
                let x = 1;
                set_per_monitor_v2_first_call();
            }
        "#;
        assert!(check_source(src).is_err());
    }

    #[test]
    fn negative_no_call() {
        let src = r#"
            fn main() {
                println!("hi");
            }
        "#;
        assert!(check_source(src).is_err());
    }

    #[test]
    fn no_main_fn_passes() {
        let src = r#"
            fn helper() { let x = 1; }
        "#;
        assert!(check_source(src).is_ok());
    }
}
