//! Thin `SetCursorPos` helper. Default-on for absolute clicks per CONTEXT.md
//! (PITFALLS #5: games / Electron require a real cursor move before click).

use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// Move the system cursor to the given physical-pixel virtual-desktop coords.
pub fn set_cursor_pos(x: i32, y: i32) -> Result<(), ProtoError> {
    // SAFETY: SetCursorPos is always safe; failure returns FALSE.
    let r = unsafe { SetCursorPos(x, y) };
    if r.is_err() {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("SetCursorPos({x},{y}) failed: {:?}", r),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    use windows::Win32::Foundation::POINT;

    #[test]
    fn set_cursor_pos_is_observable() {
        // Save & restore.
        let mut before = POINT::default();
        // SAFETY: GetCursorPos writes a single POINT.
        unsafe {
            let _ = GetCursorPos(&mut before);
        }
        // Choose a target inside the primary monitor; on a single-mon dev
        // system this should always land safely.
        let target_x = 100;
        let target_y = 100;
        set_cursor_pos(target_x, target_y).expect("set");
        let mut after = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut after);
        }
        // Restore.
        let _ = set_cursor_pos(before.x, before.y);
        // Allow ±1px of OS rounding.
        assert!((after.x - target_x).abs() <= 1, "x: {} vs target {target_x}", after.x);
        assert!((after.y - target_y).abs() <= 1, "y: {} vs target {target_y}", after.y);
    }
}
