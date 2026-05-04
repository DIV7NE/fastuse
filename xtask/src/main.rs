//! xtask: project-wide lints (firstcall, com, redact, cacherequest).

mod check_cacherequest;
mod check_com;
mod check_firstcall;
mod check_redact;

use std::path::PathBuf;

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let root = workspace_root();
    let res = match cmd.as_str() {
        "check-firstcall" => check_firstcall::run(&root),
        "check-com" => check_com::run(&root),
        "check-redact" => check_redact::run(&root),
        "check-cacherequest" => check_cacherequest::run(&root),
        "lints" => check_firstcall::run(&root)
            .and_then(|_| check_com::run(&root))
            .and_then(|_| check_redact::run(&root))
            .and_then(|_| check_cacherequest::run(&root)),
        _ => {
            eprintln!(
                "usage: cargo xtask <check-firstcall|check-com|check-redact|check-cacherequest|lints>"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("{e}");
        std::process::exit(1);
    }
    println!("ok");
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
