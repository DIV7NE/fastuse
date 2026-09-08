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

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};

/// CF_DIBV5 clipboard format constant.
const CF_DIBV5: u32 = 17;

/// `LCS_sRGB` FourCC literal — 0x73524742 = "sRGB".
/// In `windows 0.62` this lives in `Win32::UI::ColorSystem` as `LCSCSTYPE(1934772034_i32)`.
/// Using the raw u32 avoids pulling in `Win32_UI_ColorSystem` feature for a single constant.
const LCS_SRGB: u32 = 0x7352_4742_u32;

/// Standard clipboard format: CF_UNICODETEXT.
const CF_UNICODETEXT: u32 = 13;

/// Standard clipboard format: CF_HDROP.
const CF_HDROP: u32 = 15;

/// `DROPEFFECT_COPY`. Published under `CFSTR_PREFERREDDROPEFFECT` so targets
/// copy rather than MOVE - without it Explorer relocates the user's source
/// file on paste, which is data loss, not a UX wart.
const DROPEFFECT_COPY: u32 = 1;

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
                // SAFETY: clipboard is already open at this point.
                let avail = unsafe { IsClipboardFormatAvailable(CF_DIBV5) };
                if avail.is_ok() {
                    match decode_cf_dibv5_png() {
                        Ok((png_bytes, w, h)) => {
                            use base64::Engine;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
                            tracing::info!(format = "image", w, h, png_bytes = png_bytes.len(), "clipboard_get image ok");
                            return Ok(ClipboardGetResp::Image {
                                mime: "image/png".into(),
                                base64: Redact::new(b64),
                                w,
                                h,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(format = "image", err = %e, "clipboard_get CF_DIBV5 decode failed");
                        }
                    }
                } else {
                    tracing::info!(format = "image", "clipboard_get: CF_DIBV5 not available");
                }
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
            let raw_bytes = bytes.into_inner();
            tracing::info!(
                format = "image",
                payload_size_bytes = raw_bytes.len(),
                w,
                h,
                mime = %mime,
                "clipboard_set image"
            );
            let img_format = match mime.as_str() {
                "image/png" => image::ImageFormat::Png,
                "image/jpeg" | "image/jpg" => image::ImageFormat::Jpeg,
                other => {
                    return Err(FastuseError::Internal(format!(
                        "clipboard_set: unsupported mime type: {other}"
                    )));
                }
            };
            let rgba_image = image::load_from_memory_with_format(&raw_bytes, img_format)
                .map_err(|e| FastuseError::Internal(format!("clipboard_set image decode: {e}")))?
                .to_rgba8();
            let actual_w = rgba_image.width();
            let actual_h = rgba_image.height();
            if actual_w != w || actual_h != h {
                return Err(FastuseError::Internal(format!(
                    "clipboard_set: w/h mismatch: claimed {w}x{h}, decoded {actual_w}x{actual_h}"
                )));
            }
            let rgba_raw = rgba_image.into_raw();
            run_on_clipboard_thread(move || write_cf_dibv5(&rgba_raw, w, h))
        }
        ClipboardSet::Files { paths, .. } => {
            // Paths arrive already resolved: the dispatch layer canonicalizes
            // before calling, so a bad path never reaches EmptyClipboard.
            let paths = paths.into_inner();
            tracing::info!(format = "files", path_count = paths.len(), "clipboard_set files");
            run_on_clipboard_thread(move || write_cf_hdrop(&paths))
        }
    }
}

/// Publish `paths` as `CF_HDROP` plus a preferred drop effect of COPY.
///
/// Both formats go on in one Open/Empty/Close window: emptying between the two
/// `SetClipboardData` calls would discard the first.
fn write_cf_hdrop(paths: &[String]) -> Result<(), FastuseError> {
    let hdrop_bytes = crate::files::hdrop::build_hdrop(paths);
    let effect_bytes = DROPEFFECT_COPY.to_le_bytes();

    // SAFETY: open with NULL owner; paired close on every path.
    unsafe {
        OpenClipboard(Some(HWND::default()))
            .map_err(|e| FastuseError::Io(format!("OpenClipboard (hdrop): {e}")))?;
    }
    let result: Result<(), FastuseError> = (|| {
        // SAFETY: required before SetClipboardData per docs.
        unsafe {
            EmptyClipboard().map_err(|e| FastuseError::Io(format!("EmptyClipboard: {e}")))?;
        }
        let h_files = alloc_moveable(&hdrop_bytes)?;
        // SAFETY: hand HGLOBAL ownership to the clipboard; do not free it.
        unsafe {
            SetClipboardData(CF_HDROP, Some(HANDLE(h_files.0 as _)))
                .map_err(|e| FastuseError::Io(format!("SetClipboardData CF_HDROP: {e}")))?;
        }
        // SAFETY: registering an existing name returns the existing format id.
        let fmt = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
        if fmt == 0 {
            return Err(FastuseError::Io(
                "RegisterClipboardFormatW(Preferred DropEffect) returned 0".into(),
            ));
        }
        let h_effect = alloc_moveable(&effect_bytes)?;
        // SAFETY: ownership transfers to the clipboard, as above.
        unsafe {
            SetClipboardData(fmt, Some(HANDLE(h_effect.0 as _)))
                .map_err(|e| FastuseError::Io(format!("SetClipboardData drop effect: {e}")))?;
        }
        Ok(())
    })();
    // A failure between the two SetClipboardData calls would otherwise leave
    // CF_HDROP published with no drop effect, and Explorer reads an
    // effect-less file list as a MOVE — the user's source file disappears on
    // the next manual paste. Leave nothing behind rather than that.
    if result.is_err() {
        // SAFETY: the clipboard is still open; EmptyClipboard frees whatever
        // we already handed over.
        unsafe {
            let _ = EmptyClipboard();
        }
    }
    // SAFETY: matched OpenClipboard.
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// Copy `bytes` into a fresh `GMEM_MOVEABLE` block for clipboard handoff.
fn alloc_moveable(bytes: &[u8]) -> Result<HGLOBAL, FastuseError> {
    // SAFETY: GMEM_MOVEABLE is what SetClipboardData requires.
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
        .map_err(|e| FastuseError::Io(format!("GlobalAlloc: {e}")))?;
    // SAFETY: lock to fill the moveable block.
    let dst = unsafe { GlobalLock(hmem) } as *mut u8;
    if dst.is_null() {
        return Err(FastuseError::Io("GlobalLock returned null".into()));
    }
    // SAFETY: dst is valid for bytes.len() bytes, freshly allocated.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        let _ = GlobalUnlock(hmem);
    }
    Ok(hmem)
}

/// Read the clipboard's `CF_HDROP` back as plain strings.
///
/// Exists for the round-trip test: the publish path has no other observable
/// output, so without this the only check would be "it did not error".
#[doc(hidden)]
pub fn read_back_hdrop_for_test() -> Result<Vec<String>, FastuseError> {
    run_on_clipboard_thread(|| {
        // SAFETY: opened with a NULL owner; paired with CloseClipboard below.
        unsafe { OpenClipboard(Some(HWND::default())) }
            .map_err(|e| FastuseError::Io(format!("OpenClipboard: {e}")))?;
        let result = (|| -> Result<Vec<String>, FastuseError> {
            // SAFETY: handle stays owned by the clipboard; we only read it.
            let h = unsafe { GetClipboardData(CF_HDROP) }
                .map_err(|e| FastuseError::Io(format!("GetClipboardData(CF_HDROP): {e}")))?;
            let hdrop = HDROP(h.0);
            // SAFETY: count query form - u32::MAX index, NULL buffer.
            let n = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let mut buf = [0u16; 260];
                // SAFETY: buf outlives the call; len is its element count.
                let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf[..])) } as usize;
                out.push(String::from_utf16_lossy(&buf[..written]));
            }
            Ok(out)
        })();
        // SAFETY: paired with the OpenClipboard above.
        unsafe {
            let _ = CloseClipboard();
        }
        result
    })
}

/// Read the `Preferred DropEffect` DWORD back.
#[doc(hidden)]
pub fn read_back_preferred_effect_for_test() -> Result<u32, FastuseError> {
    run_on_clipboard_thread(|| {
        // SAFETY: opened with a NULL owner; paired with CloseClipboard below.
        unsafe { OpenClipboard(Some(HWND::default())) }
            .map_err(|e| FastuseError::Io(format!("OpenClipboard: {e}")))?;
        let result = (|| -> Result<u32, FastuseError> {
            // SAFETY: same registered name the publish path uses.
            let fmt = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
            // SAFETY: handle stays owned by the clipboard.
            let h = unsafe { GetClipboardData(fmt) }
                .map_err(|e| FastuseError::Io(format!("GetClipboardData(effect): {e}")))?;
            let hg = HGLOBAL(h.0 as *mut _);
            // SAFETY: the blob is a single DWORD; unlocked immediately after.
            let p = unsafe { GlobalLock(hg) } as *const u32;
            if p.is_null() {
                return Err(FastuseError::Io("GlobalLock returned null".into()));
            }
            // SAFETY: p points at a 4-byte block written by the publish path.
            let v = unsafe { *p };
            // SAFETY: paired with the GlobalLock above.
            unsafe {
                let _ = GlobalUnlock(hg);
            }
            Ok(v)
        })();
        // SAFETY: paired with the OpenClipboard above.
        unsafe {
            let _ = CloseClipboard();
        }
        result
    })
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

// PCWSTR import kept alive for potential future use in clipboard format name lookups.
#[allow(dead_code)]
const _PCWSTR_KEEPALIVE: Option<PCWSTR> = None;

// ---------------------------------------------------------------------------
// DIBV5 codec helpers
// ---------------------------------------------------------------------------

/// Raw RGBA pixels decoded from a CF_DIBV5 clipboard entry.
struct DibImage {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

/// Read `CF_DIBV5` from an already-open clipboard, decode pixels to RGBA, and
/// PNG-encode the result. Returns `(png_bytes, width, height)`.
///
/// SAFETY: caller must have called `OpenClipboard` on this thread and must call
/// `CloseClipboard` after this returns (success or error).
fn decode_cf_dibv5_png() -> Result<(Vec<u8>, u32, u32), FastuseError> {
    use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

    let dib = unsafe { read_dibv5_locked()? };
    let mut png: Vec<u8> = Vec::with_capacity(dib.rgba.len() / 2);
    PngEncoder::new(&mut png)
        .write_image(&dib.rgba, dib.w, dib.h, ExtendedColorType::Rgba8)
        .map_err(|e| FastuseError::Internal(format!("PNG encode from CF_DIBV5: {e}")))?;
    Ok((png, dib.w, dib.h))
}

/// Lock the CF_DIBV5 global handle and copy pixels to a `DibImage`.
///
/// # Safety
/// Clipboard must be open on the calling thread.
unsafe fn read_dibv5_locked() -> Result<DibImage, FastuseError> {
    // SAFETY: CF_DIBV5 = 17, clipboard is open.
    let h: HANDLE = unsafe {
        GetClipboardData(CF_DIBV5)
            .map_err(|e| FastuseError::Io(format!("GetClipboardData CF_DIBV5: {e}")))?
    };
    let hg = HGLOBAL(h.0 as *mut _);
    let size = unsafe { GlobalSize(hg) };
    if size < std::mem::size_of::<BITMAPV5HEADER>() {
        return Err(FastuseError::Internal(format!(
            "CF_DIBV5 block too small: {size} bytes"
        )));
    }
    // SAFETY: lock before reading.
    let ptr = unsafe { GlobalLock(hg) } as *const u8;
    if ptr.is_null() {
        return Err(FastuseError::Io("GlobalLock returned null for CF_DIBV5".into()));
    }
    let result = (|| -> Result<DibImage, FastuseError> {
        // SAFETY: ptr is valid for at least `size` bytes; aligned by GlobalAlloc.
        let header: &BITMAPV5HEADER = unsafe { &*(ptr as *const BITMAPV5HEADER) };
        let w = header.bV5Width as u32;
        let h_signed = header.bV5Height;
        let h_abs = h_signed.unsigned_abs();
        // Positive bV5Height means bottom-up (classic DIB); negative = top-down.
        let bottom_up = h_signed > 0;
        let bpp = header.bV5BitCount as u32;
        if bpp != 32 {
            return Err(FastuseError::Internal(format!(
                "CF_DIBV5 unsupported bit depth: {bpp} (only 32 supported)"
            )));
        }
        let header_size = header.bV5Size as usize;
        let stride = (w * 4) as usize;
        let mut rgba = vec![0u8; h_abs as usize * stride];
        // SAFETY: header_size is within the locked region; pixel data follows header.
        let src_base = unsafe { ptr.add(header_size) };
        for row in 0..h_abs as usize {
            let src_row_offset = if bottom_up {
                (h_abs as usize - 1 - row) * stride
            } else {
                row * stride
            };
            for col in 0..w as usize {
                // SAFETY: bounds-checked by w * h_abs * 4 <= size.
                let s = unsafe { src_base.add(src_row_offset + col * 4) };
                let d = unsafe { rgba.as_mut_ptr().add(row * stride + col * 4) };
                // CF_DIBV5 BGRA → RGBA.
                unsafe {
                    *d.add(0) = *s.add(2); // B→R
                    *d.add(1) = *s.add(1); // G
                    *d.add(2) = *s.add(0); // R→B
                    *d.add(3) = *s.add(3); // A
                }
            }
        }
        Ok(DibImage { rgba, w, h: h_abs })
    })();
    // SAFETY: balanced with GlobalLock.
    unsafe {
        let _ = GlobalUnlock(hg);
    }
    result
}

/// Encode `rgba` pixels as a CF_DIBV5 HGLOBAL and place it on the clipboard.
///
/// Opens and closes the clipboard internally (clipboard must be closed when called).
fn write_cf_dibv5(rgba: &[u8], w: u32, h: u32) -> Result<(), FastuseError> {
    let header_size = std::mem::size_of::<BITMAPV5HEADER>();
    let pixel_size = (w * h * 4) as usize;
    let total = header_size + pixel_size;

    // SAFETY: GMEM_MOVEABLE allocation for clipboard handoff.
    let hmem = unsafe {
        GlobalAlloc(GMEM_MOVEABLE, total)
            .map_err(|e| FastuseError::Io(format!("GlobalAlloc CF_DIBV5: {e}")))?
    };
    // Lock, fill header + pixels, then unlock before handing to clipboard.
    let dst = unsafe { GlobalLock(hmem) } as *mut u8;
    if dst.is_null() {
        return Err(FastuseError::Io("GlobalLock null (write_cf_dibv5)".into()));
    }
    unsafe {
        let header = &mut *(dst as *mut BITMAPV5HEADER);
        *header = std::mem::zeroed();
        header.bV5Size = header_size as u32;
        header.bV5Width = w as i32;
        header.bV5Height = -(h as i32); // negative = top-down
        header.bV5Planes = 1;
        header.bV5BitCount = 32;
        header.bV5Compression = BI_BITFIELDS;
        header.bV5SizeImage = pixel_size as u32;
        header.bV5RedMask = 0x00FF_0000_u32;
        header.bV5GreenMask = 0x0000_FF00_u32;
        header.bV5BlueMask = 0x0000_00FF_u32;
        header.bV5AlphaMask = 0xFF00_0000_u32;
        // LCS_sRGB FourCC = 0x73524742. bV5CSType is u32 in windows 0.62.
        header.bV5CSType = LCS_SRGB;

        let pixels = dst.add(header_size);
        for row in 0..h as usize {
            for col in 0..w as usize {
                let s = rgba.as_ptr().add(row * (w as usize * 4) + col * 4);
                let d = pixels.add(row * (w as usize * 4) + col * 4);
                // RGBA → BGRA for CF_DIBV5.
                *d.add(0) = *s.add(2); // R→B
                *d.add(1) = *s.add(1); // G
                *d.add(2) = *s.add(0); // B→R
                *d.add(3) = *s.add(3); // A
            }
        }
        let _ = GlobalUnlock(hmem);
    }

    // SAFETY: open with NULL owner; paired close on every path.
    unsafe {
        OpenClipboard(Some(HWND::default()))
            .map_err(|e| FastuseError::Io(format!("OpenClipboard (write_cf_dibv5): {e}")))?;
        let _ = EmptyClipboard();
        SetClipboardData(CF_DIBV5, Some(HANDLE(hmem.0 as _)))
            .map_err(|e| FastuseError::Io(format!("SetClipboardData CF_DIBV5: {e}")))?;
        let _ = CloseClipboard();
    }
    Ok(())
}

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
