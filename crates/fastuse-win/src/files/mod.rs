//! File-upload primitives: the common dialog, `CF_HDROP` on the clipboard,
//! and OLE drag-drop.
//!
//! All three entry points share [`resolve_paths`], which is the only place
//! that decides whether a caller-supplied path is usable. Validating once, up
//! front, is not defensive padding: a nonexistent path makes a file dialog
//! silently refuse to close (indistinguishable at the screenshot level from
//! the app rejecting the file) and makes `CF_HDROP` produce a drop the target
//! quietly discards.

pub mod dialog;
pub mod hdrop;

use std::path::PathBuf;

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// Canonicalize and validate every caller-supplied path.
///
/// Returns absolute paths with the `\\?\` verbatim prefix stripped.
/// `std::fs::canonicalize` on Windows always produces that prefix, and both
/// the common file dialog and `CF_HDROP` consumers mishandle it — the dialog
/// treats it as a literal filename and shell targets reject the drop.
///
/// Fails the whole call if any entry is missing, unreadable, or a directory.
pub fn resolve_paths(raw: &[String]) -> Result<Vec<PathBuf>, ProtoError> {
    resolve(raw, false)
}

/// Save-mode sibling of [`resolve_paths`]: the leaf may be missing, because
/// naming a file that is not there yet is what a Save dialog is for. The
/// parent directory must still exist — a path under a mistyped directory is a
/// mistake no dialog can recover from.
pub fn resolve_paths_allowing_new(raw: &[String]) -> Result<Vec<PathBuf>, ProtoError> {
    resolve(raw, true)
}

fn resolve(raw: &[String], allow_new: bool) -> Result<Vec<PathBuf>, ProtoError> {
    if raw.is_empty() {
        return Err(ProtoError::new(
            ErrorCode::FileNotFound,
            "paths: at least one path is required".to_string(),
        ));
    }
    raw.iter().map(|p| resolve_one(p, allow_new)).collect()
}

fn resolve_one(raw: &str, allow_new: bool) -> Result<PathBuf, ProtoError> {
    match std::fs::canonicalize(raw) {
        Ok(canon) => {
            let meta = std::fs::metadata(&canon).map_err(|e| {
                ProtoError::new(ErrorCode::FileNotFound, format!("path {raw}: {e}"))
            })?;
            if meta.is_dir() {
                return Err(ProtoError::new(
                    ErrorCode::FileNotFound,
                    format!("path {raw}: is a directory, expected a file"),
                ));
            }
            Ok(strip_verbatim_prefix(canon))
        }
        Err(e) if allow_new => {
            let path = std::path::Path::new(raw);
            let name = path.file_name().ok_or_else(|| {
                ProtoError::new(
                    ErrorCode::FileNotFound,
                    format!("path {raw}: has no file name"),
                )
            })?;
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => std::path::Path::new("."),
            };
            let parent = std::fs::canonicalize(parent).map_err(|_| {
                ProtoError::new(
                    ErrorCode::FileNotFound,
                    format!("path {raw}: parent directory does not exist ({e})"),
                )
            })?;
            Ok(strip_verbatim_prefix(parent).join(name))
        }
        Err(e) => Err(ProtoError::new(
            ErrorCode::FileNotFound,
            format!("path {raw}: {e}"),
        )),
    }
}

/// Strip the `\\?\` verbatim prefix that `canonicalize` adds on Windows.
/// Leaves `\\?\UNC\server\share` paths alone — rewriting those to `\\server`
/// is a separate normalization and no caller needs it yet.
fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_paths_rejects_empty() {
        let err = resolve_paths(&[]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
    }

    #[test]
    fn resolve_paths_rejects_missing_file() {
        let err = resolve_paths(&["Z:\\definitely\\not\\here.txt".to_string()]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
        // The offending path must be visible: an agent that cannot see which
        // path was wrong cannot fix it.
        assert!(err.message.contains("not\\here.txt"), "message was {}", err.message);
    }

    #[test]
    fn resolve_paths_rejects_directory() {
        let dir = std::env::temp_dir();
        let err = resolve_paths(&[dir.to_string_lossy().into_owned()]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
        assert!(err.message.contains("directory"), "message was {}", err.message);
    }

    #[test]
    fn allowing_new_accepts_a_missing_leaf_under_an_existing_parent() {
        let p = std::env::temp_dir().join("fastuse_not_created_yet.txt");
        std::fs::remove_file(&p).ok();
        let out = resolve_paths_allowing_new(&[p.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(out.len(), 1);
        let s = out[0].to_string_lossy().into_owned();
        assert!(!s.starts_with(r"\?\"), "UNC prefix leaked: {s}");
        assert!(s.ends_with("fastuse_not_created_yet.txt"), "got {s}");
        assert!(!out[0].exists(), "resolver must not create the file");
    }

    #[test]
    fn allowing_new_rejects_a_missing_parent() {
        let p = std::env::temp_dir().join("fastuse_no_such_dir").join("new.txt");
        let err = resolve_paths_allowing_new(&[p.to_string_lossy().into_owned()]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
        assert!(err.message.contains("parent directory"), "message was {}", err.message);
    }

    #[test]
    fn allowing_new_still_rejects_a_directory_leaf() {
        let dir = std::env::temp_dir();
        let err = resolve_paths_allowing_new(&[dir.to_string_lossy().into_owned()]).unwrap_err();
        assert_eq!(err.code, fastuse_proto::ErrorCode::FileNotFound);
        assert!(err.message.contains("directory"), "message was {}", err.message);
    }

    #[test]
    fn resolve_paths_canonicalizes_and_strips_unc_prefix() {
        let p = std::env::temp_dir().join("fastuse_resolve_paths_test.txt");
        std::fs::write(&p, b"x").unwrap();
        let out = resolve_paths(&[p.to_string_lossy().into_owned()]).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(out.len(), 1);
        let s = out[0].to_string_lossy().into_owned();
        assert!(!s.starts_with(r"\\?\"), "UNC prefix leaked: {s}");
        assert!(s.ends_with("fastuse_resolve_paths_test.txt"), "got {s}");
    }
}
