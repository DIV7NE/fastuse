//! Monitor enumeration with `WM_DISPLAYCHANGE` cache invalidation.
//!
//! Uses `EnumDisplayMonitors` + `GetMonitorInfoW` + `GetDpiForMonitor`. The
//! cache is process-global and invalidated by the input thread's WndProc
//! when `WM_DISPLAYCHANGE` fires (see `input_thread.rs`).

use std::sync::RwLock;

use once_cell::sync::Lazy;
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

// MONITORINFOF_PRIMARY = 0x00000001 — windows-rs 0.62 doesn't re-export it.
const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use fastuse_proto::{Error as ProtoError, ErrorCode, MonitorInfo, Rect};

static CACHE: Lazy<RwLock<Option<Vec<MonitorInfo>>>> = Lazy::new(|| RwLock::new(None));

/// Invalidate the monitor cache. Called by the input thread's WndProc on
/// `WM_DISPLAYCHANGE`.
pub fn invalidate_cache() {
    if let Ok(mut g) = CACHE.write() {
        *g = None;
    }
}

/// Return the cached monitor list, or rebuild it on first call.
pub fn list_monitors() -> Result<Vec<MonitorInfo>, ProtoError> {
    if let Some(cached) = CACHE.read().ok().and_then(|g| g.clone()) {
        return Ok(cached);
    }
    let fresh = enumerate_now()?;
    if let Ok(mut g) = CACHE.write() {
        *g = Some(fresh.clone());
    }
    Ok(fresh)
}

fn enumerate_now() -> Result<Vec<MonitorInfo>, ProtoError> {
    let mut out: Vec<MonitorInfo> = Vec::new();
    let raw: *mut Vec<MonitorInfo> = &mut out;
    // SAFETY: we pass `raw` as LPARAM; the callback reinterprets it back. The
    // pointer outlives the EnumDisplayMonitors call (which is synchronous).
    let ok = unsafe {
        EnumDisplayMonitors(None, None, Some(enum_proc), LPARAM(raw as isize))
    };
    if !ok.as_bool() {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            "EnumDisplayMonitors failed".to_string(),
        ));
    }
    Ok(out)
}

unsafe extern "system" fn enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rc: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let out = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: GetMonitorInfoW writes into `info`; size set above.
    let ok = unsafe {
        GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO)
    };
    if !ok.as_bool() {
        return BOOL(1);
    }
    let r = info.monitorInfo.rcMonitor;
    let bounds = Rect {
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    };
    let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
    let name = String::from_utf16_lossy(
        &info.szDevice[..info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len())],
    );
    let mut dx: u32 = 96;
    let mut dy: u32 = 96;
    // SAFETY: GetDpiForMonitor writes both u32 outs.
    let _ = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    let dpi_scale = (dx as f32) / 96.0;
    out.push(MonitorInfo {
        id: hmon.0 as u64,
        name,
        bounds,
        dpi_scale,
        is_primary,
    });
    BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_at_least_one_monitor() {
        invalidate_cache();
        let mons = list_monitors().expect("enumerate");
        assert!(!mons.is_empty());
        let primary_count = mons.iter().filter(|m| m.is_primary).count();
        assert_eq!(primary_count, 1, "exactly one primary expected, got {mons:?}");
        for m in &mons {
            assert!(m.bounds.w > 0);
            assert!(m.bounds.h > 0);
            assert!(m.dpi_scale > 0.0);
        }
    }

    #[test]
    fn cache_hit_is_fast() {
        let _ = list_monitors().unwrap();
        let start = std::time::Instant::now();
        let _ = list_monitors().unwrap();
        let dt = start.elapsed();
        assert!(dt.as_micros() < 500, "cached call took {}us", dt.as_micros());
    }

    #[test]
    fn invalidate_then_rebuild() {
        let a = list_monitors().unwrap();
        invalidate_cache();
        let b = list_monitors().unwrap();
        // Same content (no DPI change between calls).
        assert_eq!(a.len(), b.len());
    }
}
