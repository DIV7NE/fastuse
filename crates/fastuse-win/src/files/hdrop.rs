//! `CF_HDROP` payload construction.
//!
//! Kept as a pure byte-buffer builder with no Win32 calls: both the clipboard
//! path and the drag path consume it, and neither of those is unit-testable on
//! its own. The layout is a `DROPFILES` header (`pFiles`, `pt`, `fNC`,
//! `fWide` — 20 bytes on x64 with 4-byte fields and an 8-byte POINT) followed
//! by each path as UTF-16, NUL-separated, with one extra NUL to close the list.

use std::path::Path;

/// `sizeof(DROPFILES)`: u32 + POINT(2×i32) + BOOL + BOOL.
const DROPFILES_SIZE: u32 = 4 + 8 + 4 + 4;

/// Build a `CF_HDROP` payload for `paths`.
pub fn build_hdrop<P: AsRef<Path>>(paths: &[P]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(DROPFILES_SIZE as usize + paths.len() * 64);
    buf.extend_from_slice(&DROPFILES_SIZE.to_le_bytes()); // pFiles
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    buf.extend_from_slice(&0u32.to_le_bytes()); // fNC = FALSE
    buf.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
    debug_assert_eq!(buf.len(), DROPFILES_SIZE as usize);

    for p in paths {
        for unit in p.as_ref().to_string_lossy().encode_utf16() {
            buf.extend_from_slice(&unit.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes()); // list terminator
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Decode the UTF-16 path list that follows the header.
    fn names_from(buf: &[u8]) -> Vec<String> {
        let off = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let units: Vec<u16> = buf[off..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        units
            .split(|u| *u == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn header_is_20_bytes_and_declares_wide() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        // pFiles is the offset to the list and must equal sizeof(DROPFILES).
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), 20);
        // fWide is the last BOOL in the header, at offset 16.
        assert_eq!(u32::from_le_bytes(buf[16..20].try_into().unwrap()), 1);
    }

    #[test]
    fn single_path_round_trips() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        assert_eq!(names_from(&buf), vec![r"C:\a.txt".to_string()]);
    }

    #[test]
    fn several_paths_round_trip_in_order() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt"), PathBuf::from(r"C:\b.png")]);
        assert_eq!(names_from(&buf), vec![r"C:\a.txt".to_string(), r"C:\b.png".to_string()]);
    }

    #[test]
    fn list_is_double_null_terminated() {
        let buf = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        assert_eq!(&buf[buf.len() - 4..], &[0, 0, 0, 0], "needs a trailing extra NUL");
    }
}
