//! `cursor_position()` — `GetCursorPos` + `MonitorFromPoint`.

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// Read the current cursor position and the monitor it lies on.
pub fn cursor_position() -> Result<(i32, i32, u64), ProtoError> {
    let mut p = POINT::default();
    // SAFETY: GetCursorPos writes a single POINT.
    unsafe {
        GetCursorPos(&mut p).map_err(|e| ProtoError::new(
            ErrorCode::Internal,
            format!("GetCursorPos failed: {e}"),
        ))?;
    }
    // SAFETY: MonitorFromPoint is always safe.
    let hmon = unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) };
    Ok((p.x, p.y, hmon.0 as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_finite_coords_and_monitor() {
        let (x, y, mid) = cursor_position().expect("read");
        // Cursor is somewhere; monitor handle is non-null on every Windows
        // session (MONITOR_DEFAULTTONEAREST guarantee).
        assert_ne!(mid, 0);
        // Coordinates can be negative on multi-monitor; just sanity check the
        // call returned.
        let _ = (x, y);
    }
}
