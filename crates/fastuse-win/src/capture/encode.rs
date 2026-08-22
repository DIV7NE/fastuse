//! Encoder (Phase 3 Task 06, CAP-05).
//!
//! Default JPEG q=85 (typical 4K frame ≈ 100-300 KB; well under our 150 ms
//! budget). PNG path opt-in. `image 0.25` is pure-Rust → keeps the
//! single-binary distribution story.
//!
//! Input: BGRA frame from DXGI staging copy (`FrameBuf`).
//! Output: encoded bytes + MIME, mapped to `Response::Screenshot` at the
//! handler edge.

use std::cell::RefCell;

use fastuse_proto::{Error as ProtoError, ErrorCode, ImageFormat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};

use crate::capture::dxgi::FrameBuf;

// WR-09: thread-local scratch buffer reused across encodes to keep
// allocator pressure low (4K BGRA is ~33MB; per-call alloc/free of that
// flushes the allocator's thread cache and produces p99 spikes).
thread_local! {
    static RGBA_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static RGB_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUT_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Encoded image plus its MIME type. Bytes ride the wire wrapped in
/// `Redact<Vec<u8>>` (T-03-03) — never logged.
#[derive(Debug, Clone)]
pub struct EncodedImage {
    /// Encoded bytes (JPEG or PNG).
    pub bytes: Vec<u8>,
    /// `image/jpeg` or `image/png`.
    pub mime: &'static str,
}

/// Encode a BGRA `FrameBuf` to JPEG (default q=85) or PNG.
///
/// WR-09: Reuses thread-local scratch buffers for the BGRA→RGBA pass and
/// for the encoder output to avoid allocating ~14 MB per 1080p frame.
/// JPEG path skips the RGBA→RGB conversion — `JpegEncoder` accepts
/// `ColorType::Rgba8` and discards alpha internally.
pub fn encode(buf: &FrameBuf, format: ImageFormat) -> Result<EncodedImage, ProtoError> {
    if buf.w == 0 || buf.h == 0 {
        return Err(ProtoError::new(
            ErrorCode::EncodeFailed,
            "zero-sized frame".to_string(),
        ));
    }
    // BGRA → RGBA in-place into the thread-local scratch buffer.
    let bytes = RGBA_SCRATCH.with(|s| -> Result<Vec<u8>, ProtoError> {
        let mut rgba = s.borrow_mut();
        bgra_to_rgba_into(&buf.bgra, &mut rgba);
        match format {
            ImageFormat::Jpeg => encode_jpeg(&rgba, buf.w, buf.h),
            ImageFormat::Png => encode_png(&rgba, buf.w, buf.h),
        }
    })?;
    let mime = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
    };
    Ok(EncodedImage { bytes, mime })
}

/// Encode pre-converted RGBA bytes directly to JPEG (quality 85).
///
/// Used by the v2 screenshot/zoom path, which resizes BGRA→RGBA before
/// calling the encoder. Callers must ensure `rgba.len() == w * h * 4`.
pub fn encode_jpeg_rgba(rgba: &[u8], w: u32, h: u32) -> Result<EncodedImage, ProtoError> {
    let bytes = encode_jpeg(rgba, w, h)?;
    Ok(EncodedImage { bytes, mime: "image/jpeg" })
}

/// Encode pre-converted RGBA bytes in the requested format. Same contract as
/// [`encode_jpeg_rgba`]; used by the v2 path so `--format png` is honoured
/// rather than silently downgraded to JPEG.
pub fn encode_rgba(
    rgba: &[u8],
    w: u32,
    h: u32,
    format: ImageFormat,
) -> Result<EncodedImage, ProtoError> {
    match format {
        ImageFormat::Jpeg => encode_jpeg_rgba(rgba, w, h),
        ImageFormat::Png => Ok(EncodedImage { bytes: encode_png(rgba, w, h)?, mime: "image/png" }),
    }
}

fn encode_jpeg(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, ProtoError> {
    // image 0.25's JpegEncoder requires Rgb8 input; drop alpha into a
    // thread-local scratch (not per-call) to avoid the per-frame alloc.
    RGB_SCRATCH.with(|rs| -> Result<Vec<u8>, ProtoError> {
        let mut rgb = rs.borrow_mut();
        rgba_to_rgb_into(rgba, &mut rgb);
        OUT_SCRATCH.with(|s| {
            let mut out = s.borrow_mut();
            out.clear();
            out.reserve((w as usize * h as usize) / 4);
            {
                let mut enc = JpegEncoder::new_with_quality(&mut *out, 85);
                enc.write_image(&rgb, w, h, ColorType::Rgb8.into()).map_err(|e| {
                    ProtoError::new(ErrorCode::EncodeFailed, format!("jpeg encode: {e}"))
                })?;
            }
            Ok(std::mem::take(&mut *out))
        })
    })
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, ProtoError> {
    OUT_SCRATCH.with(|s| {
        let mut out = s.borrow_mut();
        out.clear();
        out.reserve(rgba.len() / 2);
        {
            let enc = PngEncoder::new(&mut *out);
            enc.write_image(rgba, w, h, ColorType::Rgba8.into())
                .map_err(|e| ProtoError::new(ErrorCode::EncodeFailed, format!("png encode: {e}")))?;
        }
        Ok(std::mem::take(&mut *out))
    })
}

/// BGRA→RGBA pass into a caller-provided buffer, reused across calls.
fn bgra_to_rgba_into(bgra: &[u8], out: &mut Vec<u8>) {
    debug_assert_eq!(bgra.len() % 4, 0);
    out.clear();
    out.reserve(bgra.len());
    for chunk in bgra.chunks_exact(4) {
        out.push(chunk[2]); // R
        out.push(chunk[1]); // G
        out.push(chunk[0]); // B
        out.push(chunk[3]); // A
    }
}

/// RGBA→RGB pass into a caller-provided buffer, reused across calls.
fn rgba_to_rgb_into(rgba: &[u8], out: &mut Vec<u8>) {
    debug_assert_eq!(rgba.len() % 4, 0);
    out.clear();
    out.reserve(rgba.len() / 4 * 3);
    for chunk in rgba.chunks_exact(4) {
        out.push(chunk[0]);
        out.push(chunk[1]);
        out.push(chunk[2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_bgra(w: u32, h: u32) -> FrameBuf {
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                bgra[i] = (x % 256) as u8;     // B
                bgra[i + 1] = (y % 256) as u8; // G
                bgra[i + 2] = ((x + y) % 256) as u8; // R
                bgra[i + 3] = 255;
            }
        }
        FrameBuf { bgra, w, h }
    }

    #[test]
    fn jpeg_round_trip_preserves_dimensions() {
        let buf = synthetic_bgra(64, 48);
        let img = encode(&buf, ImageFormat::Jpeg).unwrap();
        assert_eq!(img.mime, "image/jpeg");
        assert!(img.bytes.len() > 100, "jpeg too small: {}", img.bytes.len());
        // JPEG SOI marker is FFD8 FFE0 (or FFE1)
        assert_eq!(&img.bytes[..2], &[0xff, 0xd8]);
        // Decode to verify dimensions
        let decoded = image::load_from_memory(&img.bytes).expect("decode jpeg");
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 48);
    }

    #[test]
    fn png_round_trip_preserves_dimensions_and_alpha() {
        let buf = synthetic_bgra(32, 32);
        let img = encode(&buf, ImageFormat::Png).unwrap();
        assert_eq!(img.mime, "image/png");
        // PNG signature
        assert_eq!(&img.bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        let decoded = image::load_from_memory(&img.bytes).expect("decode png");
        assert_eq!(decoded.width(), 32);
        assert_eq!(decoded.height(), 32);
    }

    #[test]
    fn zero_sized_returns_encode_failed() {
        let buf = FrameBuf {
            bgra: vec![],
            w: 0,
            h: 0,
        };
        let r = encode(&buf, ImageFormat::Jpeg);
        assert!(matches!(r, Err(e) if e.code == ErrorCode::EncodeFailed));
    }
}
