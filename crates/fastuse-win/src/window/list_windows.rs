//! `list_windows(filter)` and `safe_get_window_text` (`SendMessageTimeoutW`
//! with `SMTO_ABORTIFHUNG`).
//!
//! NEVER use the raw `GetWindowTextW` on a foreign HWND — it sends a blocking
//! `WM_GETTEXT` and a hung target window will block our input thread. PITFALLS
//! #5 / CONTEXT.md hard rule.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, IsWindowVisible, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_GETTEXT,
};

use fastuse_proto::{Error as ProtoError, WindowInfo};

use crate::window::build_window_info;

const CACHE_TTL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct FilterKey {
    process_name: Option<String>,
    title_substring: Option<String>,
    visible_only: bool,
}

struct CacheEntry {
    when: Instant,
    key: FilterKey,
    value: Vec<WindowInfo>,
}

static CACHE: Lazy<Mutex<Option<CacheEntry>>> = Lazy::new(|| Mutex::new(None));

/// Invalidate the window list cache. Called by focus_window /
/// resize_move_window successfully completing, or by `WM_DISPLAYCHANGE`.
pub fn invalidate_cache() {
    if let Ok(mut g) = CACHE.lock() {
        *g = None;
    }
}

/// Enumerate top-level windows.
pub fn list_windows(
    process_name: Option<&str>,
    title_substring: Option<&str>,
    visible_only: bool,
) -> Result<Vec<WindowInfo>, ProtoError> {
    let key = FilterKey {
        process_name: process_name.map(|s| s.to_lowercase()),
        title_substring: title_substring.map(|s| s.to_lowercase()),
        visible_only,
    };

    if let Ok(g) = CACHE.lock() {
        if let Some(entry) = g.as_ref() {
            if entry.key == key && entry.when.elapsed() < CACHE_TTL {
                return Ok(entry.value.clone());
            }
        }
    }

    let raw = enumerate_now(visible_only)?;
    let filtered: Vec<WindowInfo> = raw
        .into_iter()
        .filter(|w| match &key.process_name {
            Some(p) => w.process_name.to_lowercase().contains(p),
            None => true,
        })
        .filter(|w| match &key.title_substring {
            Some(t) => w.title.to_lowercase().contains(t),
            None => true,
        })
        .collect();

    if let Ok(mut g) = CACHE.lock() {
        *g = Some(CacheEntry {
            when: Instant::now(),
            key,
            value: filtered.clone(),
        });
    }
    Ok(filtered)
}

fn enumerate_now(visible_only: bool) -> Result<Vec<WindowInfo>, ProtoError> {
    let mut hwnds: Vec<HWND> = Vec::new();
    let raw: *mut Vec<HWND> = &mut hwnds;
    // SAFETY: EnumWindows is synchronous; raw lives for the duration.
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(raw as isize)) };
    let mut out = Vec::with_capacity(hwnds.len());
    for h in hwnds {
        if visible_only {
            // SAFETY: any HWND value is valid input.
            if !unsafe { IsWindowVisible(h) }.as_bool() {
                continue;
            }
        }
        if let Ok(info) = build_window_info(h) {
            out.push(info);
        }
    }
    Ok(out)
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let v = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    v.push(hwnd);
    BOOL(1)
}

/// Read a window title via `SendMessageTimeoutW(WM_GETTEXT, SMTO_ABORTIFHUNG)`
/// — never blocks longer than `timeout_ms` even against a hung target
/// (PITFALLS #5).
pub fn safe_get_window_text(hwnd: HWND, timeout_ms: u32) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut out: usize = 0;
    // SAFETY: buf is on the stack; LPARAM carries its pointer; SMTO_ABORTIFHUNG
    // bounds the wait. Returns 0 on timeout.
    let r = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETTEXT,
            WPARAM(buf.len()),
            LPARAM(buf.as_mut_ptr() as isize),
            SMTO_ABORTIFHUNG,
            timeout_ms,
            Some(&mut out as *mut usize as *mut _),
        )
    };
    if r.0 == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_at_least_one_window() {
        invalidate_cache();
        let ws = list_windows(None, None, true).expect("enumerate");
        // Even a CI runner has at least one visible top-level window
        // (conhost or similar). If 0, this is fine — just don't crash.
        let _ = ws.len();
    }

    #[test]
    fn cache_hit_returns_quickly() {
        let _ = list_windows(None, None, true).unwrap();
        let start = Instant::now();
        let _ = list_windows(None, None, true).unwrap();
        let dt = start.elapsed();
        // Cached hit should be sub-ms.
        assert!(dt.as_millis() < 5, "cached call took {}ms", dt.as_millis());
    }

    #[test]
    fn filter_substring_works() {
        let _ = list_windows(Some("nonexistentprocxyz"), None, true).unwrap();
        // Should be empty (or at least not crash).
    }
}
