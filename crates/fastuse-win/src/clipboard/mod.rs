//! Clipboard primitive — `clipboard_get` / `clipboard_set` for text + image.
//!
//! ## STA requirement
//!
//! Clipboard APIs (`OpenClipboard`, `GetClipboardData`, `SetClipboardData`)
//! require a window-bearing thread for stable behavior across formats. Per
//! the Phase 1 D-26 decision, fastuse routes Win32 work onto dedicated
//! threads. For Phase 4 we use a short-lived blocking-friendly STA helper
//! that runs the clipboard op end-to-end on a single OS thread (CoInitialize
//! STA → OpenClipboard(NULL) → op → CloseClipboard → CoUninitialize).
//!
//! In a future tightening pass the input thread can take ownership of these
//! ops via a `ClipboardJob` variant on `InputJob`; the call site here is
//! intentionally swappable.
//!
//! ## Redaction
//!
//! Every payload bytestring is wrapped in `Redact<T>` before crossing the
//! module boundary. Tracing spans only ever record `format` + byte counts —
//! never the payload. The `xtask check-redact` lint enforces this against
//! suspect field names.

use fastuse_core::FastuseError;
use fastuse_proto::{ClipFormat, ClipboardGet, ClipboardGetResp, ClipboardSet, Redact};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// Standard clipboard format: CF_UNICODETEXT.
const CF_UNICODETEXT: u32 = 13;

/// Read clipboard contents.
#[tracing::instrument]
pub fn clipboard_get(req: ClipboardGet) -> Result<ClipboardGetResp, FastuseError> {
    let want_text = matches!(req.format, None | Some(ClipFormat::Text));
    let want_image = matches!(req.format, None | Some(ClipFormat::Image));

    run_on_clipboard_thread(move || {
        // Open with NULL owner — works for read-only use.
        // SAFETY: OpenClipboard with HWND(0); paired with CloseClipboard.
        unsafe {
            OpenClipboard(Some(HWND::default()))
                .map_err(|e| FastuseError::Io(format!("OpenClipboard: {e}")))?;
        }
        let result = (|| -> Result<ClipboardGetResp, FastuseError> {
            if want_text {
                // SAFETY: format constant is well-known.
                let avail = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) };
                if avail.is_ok() {
                    // SAFETY: GetClipboardData on UNICODETEXT returns HGLOBAL of u16s.
                    let h: HANDLE = unsafe { GetClipboardData(CF_UNICODETEXT) }
                        .map_err(|e| FastuseError::Io(format!("GetClipboardData: {e}")))?;
                    if !h.is_invalid() {
                        // SAFETY: HGLOBAL must be locked before access.
                        let hg = HGLOBAL(h.0 as *mut _);
                        let p = unsafe { GlobalLock(hg) } as *const u16;
                        if !p.is_null() {
                            // Walk until NUL terminator.
                            let mut len = 0usize;
                            // SAFETY: clipboard text always NUL-terminated.
                            unsafe {
                                while *p.add(len) != 0 {
                                    len += 1;
                                    if len > 64 * 1024 * 1024 {
                                        break;
                                    }
                                }
                            }
                            // SAFETY: len bounded above; pointer valid until GlobalUnlock.
                            let slice = unsafe { std::slice::from_raw_parts(p, len) };
                            let s = String::from_utf16_lossy(slice);
                            // SAFETY: balanced with GlobalLock.
                            unsafe {
                                let _ = GlobalUnlock(hg);
                            }
                            tracing::info!(format = "text", payload_size_bytes = s.len(), "clipboard_get");
                            return Ok(ClipboardGetResp::Text {
                                text: Redact::new(s),
                            });
                        }
                        unsafe {
                            let _ = GlobalUnlock(hg);
                        }
                    }
                }
            }
            if want_image {
                // CF_DIBV5 / CF_BITMAP support deferred — return None for now
                // so callers fall back gracefully. (Phase 4 deliverable: clipboard image
                // round-trip is gated by Confirmed tier; Phase 4 ships text fully and
                // returns None for image until DIBV5 -> PNG conversion lands.)
                tracing::info!(format = "image", "clipboard_get image: not yet implemented (returns None)");
            }
            Ok(ClipboardGetResp::None)
        })();
        // SAFETY: matched OpenClipboard.
        unsafe {
            let _ = CloseClipboard();
        }
        result
    })
}

/// Write clipboard contents.
#[tracing::instrument(skip(set))]
pub fn clipboard_set(set: ClipboardSet) -> Result<(), FastuseError> {
    match set {
        ClipboardSet::Text(text) => {
            let s = text.into_inner();
            tracing::info!(format = "text", payload_size_bytes = s.len(), "clipboard_set");
            run_on_clipboard_thread(move || set_text_inner(&s))
        }
        ClipboardSet::Image { mime, bytes, w, h } => {
            // Image set deferred — return Internal error to signal caller.
            tracing::info!(
                format = "image",
                payload_size_bytes = bytes.as_inner().len(),
                w,
                h,
                mime = %mime,
                "clipboard_set image: not yet implemented"
            );
            Err(FastuseError::Internal(
                "clipboard_set image not yet implemented in this Phase 4 slice".into(),
            ))
        }
    }
}

fn set_text_inner(s: &str) -> Result<(), FastuseError> {
    // Allocate UTF-16 buffer including trailing NUL.
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0u16)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();

    // SAFETY: open with NULL owner; balanced close on every path.
    unsafe {
        OpenClipboard(Some(HWND::default()))
            .map_err(|e| FastuseError::Io(format!("OpenClipboard: {e}")))?;
    }
    let result: Result<(), FastuseError> = (|| {
        // SAFETY: required before SetClipboardData per docs.
        unsafe {
            EmptyClipboard().map_err(|e| FastuseError::Io(format!("EmptyClipboard: {e}")))?;
        }
        // SAFETY: GMEM_MOVEABLE buffer for SetClipboardData.
        let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
            .map_err(|e| FastuseError::Io(format!("GlobalAlloc: {e}")))?;
        // SAFETY: lock to copy into the moveable buffer.
        let p = unsafe { GlobalLock(hglobal) } as *mut u16;
        if p.is_null() {
            return Err(FastuseError::Io("GlobalLock returned null".into()));
        }
        // SAFETY: dst capacity matches wide.len(); src is owned vec.
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(hglobal);
        }
        // SAFETY: hand HGLOBAL ownership to the clipboard.
        unsafe {
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hglobal.0 as _)))
                .map_err(|e| FastuseError::Io(format!("SetClipboardData: {e}")))?;
        }
        Ok(())
    })();
    // SAFETY: matched OpenClipboard.
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// Run a clipboard op on a dedicated short-lived STA thread.
///
/// This exists because clipboard APIs require a window-bearing thread; the
/// daemon's tokio worker pool has none. Each call costs ~1ms thread spawn.
/// In a future tightening pass this routes through the Phase 1 input thread
/// via a `ClipboardJob` variant on `InputJob`.
fn run_on_clipboard_thread<F, R>(f: F) -> Result<R, FastuseError>
where
    F: FnOnce() -> Result<R, FastuseError> + Send + 'static,
    R: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("fastuse-clipboard".into())
        .spawn(move || {
            // SAFETY: STA init paired with CoUninitialize on this thread.
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
            };
            // SAFETY: per-thread STA bump; clipboard APIs are STA-affine.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            let r = f();
            // SAFETY: matched.
            unsafe { CoUninitialize() };
            let _ = tx.send(r);
        })
        .map_err(|e| FastuseError::Io(format!("spawn clipboard thread: {e}")))?;
    rx.recv()
        .map_err(|_| FastuseError::Internal("clipboard thread vanished".into()))?
}

// Suppress unused-import warning for PCWSTR (kept for future image impl).
#[allow(dead_code)]
const _PCWSTR_KEEPALIVE: Option<PCWSTR> = None;

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs single-threaded since clipboard is process-wide. Save and restore
    /// the user's clipboard around each test pass.
    #[test]
    fn text_round_trip() {
        // Save existing.
        let prev = clipboard_get(ClipboardGet { format: Some(ClipFormat::Text) });

        let marker = "fastuse-test-clip-marker-AAA";
        clipboard_set(ClipboardSet::Text(Redact::new(marker.into()))).expect("set");
        let got = clipboard_get(ClipboardGet {
            format: Some(ClipFormat::Text),
        })
        .expect("get");
        match got {
            ClipboardGetResp::Text { text } => assert_eq!(text.as_inner(), marker),
            other => panic!("expected Text, got {other:?}"),
        }
        // Restore prior contents if they were text.
        if let Ok(ClipboardGetResp::Text { text }) = prev {
            let _ = clipboard_set(ClipboardSet::Text(text));
        }
    }
}
