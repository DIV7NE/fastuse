//! `focus_window(hwnd)` via the AttachThreadInput dance to bypass
//! `SetForegroundWindow`'s lockout heuristic (PITFALLS #6).

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow,
    SetForegroundWindow, ShowWindow, ASFW_ANY, SW_RESTORE,
};

use fastuse_proto::{Error as ProtoError, ErrorCode};

use crate::window::{list_windows::invalidate_cache, window_not_found};

/// Bring `hwnd` to the foreground. Returns `WindowNotFound` if HWND is
/// stale.
pub fn focus_window(hwnd_raw: u64) -> Result<(), ProtoError> {
    let hwnd = HWND(hwnd_raw as *mut _);
    // SAFETY: IsWindow accepts any value.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(window_not_found(hwnd_raw));
    }
    // SAFETY: GetWindowThreadProcessId is always safe.
    let target_thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
    // SAFETY: GetForegroundWindow is always safe.
    let fg = unsafe { GetForegroundWindow() };
    // SAFETY: GetWindowThreadProcessId on possibly-null fg is safe (returns 0).
    let fg_thread = unsafe { GetWindowThreadProcessId(fg, None) };
    // SAFETY: GetCurrentThreadId is always safe.
    let our_thread = unsafe { GetCurrentThreadId() };

    // Best-effort: AllowSetForegroundWindow lifts the lockout on our
    // children. Returns FALSE if not entitled — we don't treat that as
    // fatal.
    // SAFETY: ASFW_ANY is the documented sentinel.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };

    // Foreground-privilege grant: synthesize an Alt down/up via SendInput
    // so Windows registers user-input recency on our thread and lifts
    // SetForegroundWindow's lockout for the next call. Without this,
    // SetForegroundWindow can return TRUE while the system silently just
    // flashes the taskbar instead of raising the window — exactly the
    // failure mode that blocks driving an app hidden behind another.
    tap_alt_for_foreground_privilege();

    // If the window is minimised, restore it before SetForegroundWindow —
    // SetForegroundWindow on an iconic window does not un-minimise.
    // SAFETY: IsIconic accepts any HWND.
    if unsafe { IsIconic(hwnd) }.as_bool() {
        // SAFETY: ShowWindow accepts any HWND + SHOW_WINDOW_CMD.
        let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
    }

    // Attach our input queue to the target thread (and the current
    // foreground thread) so SetForegroundWindow won't be denied.
    let attach1 = if fg_thread != 0 && fg_thread != our_thread {
        // SAFETY: AttachThreadInput accepts any TIDs; returns FALSE if invalid.
        unsafe { AttachThreadInput(our_thread, fg_thread, true) }.as_bool()
    } else {
        false
    };
    let attach2 = if target_thread != 0 && target_thread != our_thread {
        // SAFETY: same.
        unsafe { AttachThreadInput(our_thread, target_thread, true) }.as_bool()
    } else {
        false
    };

    // SAFETY: SetForegroundWindow is always safe.
    let r = unsafe { SetForegroundWindow(hwnd) };

    // Detach in reverse order. Drop attachments unconditionally — we don't
    // care about their return values.
    if attach2 {
        // SAFETY: AttachThreadInput symmetric detach.
        let _ = unsafe { AttachThreadInput(our_thread, target_thread, false) };
    }
    if attach1 {
        // SAFETY: AttachThreadInput symmetric detach.
        let _ = unsafe { AttachThreadInput(our_thread, fg_thread, false) };
    }

    if !r.as_bool() {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("SetForegroundWindow on HWND {hwnd_raw:#x} returned FALSE"),
        )
        .with_hint(
            "the AttachThreadInput dance was attempted; some applications (full-screen games, secure desktop) cannot be focused programmatically".to_string(),
        ));
    }

    // Verify foreground actually changed. SetForegroundWindow returns TRUE
    // even when Windows just flashes the taskbar instead of raising — the
    // Alt-tap above usually prevents this, but verify so the caller sees a
    // real error instead of a silent lie.
    // SAFETY: GetForegroundWindow is always safe.
    let now_fg = unsafe { GetForegroundWindow() };
    if now_fg.0 != hwnd.0 {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!(
                "focus_window: SetForegroundWindow on HWND {hwnd_raw:#x} returned TRUE but foreground is still {:#x}",
                now_fg.0 as u64
            ),
        )
        .with_hint(
            "Windows foreground-lockout: the target may be on another desktop, behind an always-on-top window, or the calling thread lacks foreground privilege. Try clicking on the target window manually first."
                .to_string(),
        ));
    }

    invalidate_cache();
    Ok(())
}

/// Inject one paired Alt down/up via `SendInput` so the calling thread
/// gains foreground-grant privilege. Win32 quirk: Windows treats synthetic
/// input from a thread as user-activity, lifting `SetForegroundWindow`'s
/// lockout for the immediately following call. A bare Alt down/up has no
/// observable effect in any well-behaved app (menu bars only highlight on
/// release-without-key, which doesn't happen here).
fn tap_alt_for_foreground_privilege() {
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_MENU.0),
                wScan: 0,
                dwFlags: Default::default(),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_MENU.0),
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [down, up];
    // SAFETY: INPUT structs are fully initialised. SendInput accepts any
    // slice length; failure (returns 0) is silent — we only need the
    // foreground-privilege side-effect, so dropping events is acceptable.
    let _ = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_hwnd_returns_window_not_found() {
        let r = focus_window(0xDEAD_BEEF_DEAD_BEEF);
        match r {
            Err(e) => assert_eq!(e.code, ErrorCode::WindowNotFound),
            Ok(()) => panic!("expected WindowNotFound"),
        }
    }
}
