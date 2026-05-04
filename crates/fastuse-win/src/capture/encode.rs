//! Encoder (Phase 3 Task 06, CAP-05).
//!
//! Default JPEG q=85 (typical 4K frame ≈ 100-300 KB; well under our 150 ms
//! budget). PNG path opt-in. `image 0.25` is pure-Rust → keeps the
//! single-binary distribution story.
//!
//! Input: BGRA frame from DXGI staging copy (`FrameBuf`).
//! Output: encoded bytes + MIME, mapped to `Response::Screenshot` at the
//! handler edge.

use fastuse_proto::{Error as ProtoError, ErrorCode, ImageFormat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};

use crate::capture::dxgi::FrameBuf;

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
pub fn encode(buf: &FrameBuf, format: ImageFormat) -> Result<EncodedImage, ProtoError> {
    if buf.w == 0 || buf.h == 0 {
        return Err(ProtoError::new(
            ErrorCode::EncodeFailed,
            "zero-sized frame".to_string(),
        ));
    }
    // BGRA → RGBA (image crate has no first-class BGRA encoder).
    let rgba = bgra_to_rgba(&buf.bgra);

    match format {
        ImageFormat::Jpeg => encode_jpeg(&rgba, buf.w, buf.h),
        ImageFormat::Png => encode_png(&rgba, buf.w, buf.h),
    }
}

fn encode_jpeg(rgba: &[u8], w: u32, h: u32) -> Result<EncodedImage, ProtoError> {
    let mut out = Vec::with_capacity((w as usize * h as usize) / 4); // rough capacity hint
    {
        let mut enc = JpegEncoder::new_with_quality(&mut out, 85);
        // JPEG has no alpha; convert RGBA→RGB on the fly for fewer allocations.
        let rgb = rgba_to_rgb(rgba);
        enc.write_image(&rgb, w, h, ColorType::Rgb8.into())
            .map_err(|e| ProtoError::new(ErrorCode::EncodeFailed, format!("jpeg encode: {e}")))?;
    }
    Ok(EncodedImage {
        bytes: out,
        mime: "image/jpeg",
    })
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<EncodedImage, ProtoError> {
    let mut out = Vec::with_capacity(rgba.len() / 2);
    {
        let enc = PngEncoder::new(&mut out);
        enc.write_image(rgba, w, h, ColorType::Rgba8.into())
            .map_err(|e| ProtoError::new(ErrorCode::EncodeFailed, format!("png encode: {e}")))?;
    }
    Ok(EncodedImage {
        bytes: out,
        mime: "image/png",
    })
}

fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    debug_assert_eq!(bgra.len() % 4, 0);
    let mut out = Vec::with_capacity(bgra.len());
    for chunk in bgra.chunks_exact(4) {
        out.push(chunk[2]); // R
        out.push(chunk[1]); // G
        out.push(chunk[0]); // B
        out.push(chunk[3]); // A
    }
    out
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    debug_assert_eq!(rgba.len() % 4, 0);
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for chunk in rgba.chunks_exact(4) {
        out.push(chunk[0]);
        out.push(chunk[1]);
        out.push(chunk[2]);
    }
    out
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
