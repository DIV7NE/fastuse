//! OCR cache keyed by `(frame_hash, region_rect)`. Reuses an OCR result for
//! ~500ms when the same window region's pixels haven't changed.

use dashmap::DashMap;
use fastuse_proto::coords::Rect;
use std::sync::OnceLock;
use std::time::Instant;

use super::cropped::OcrHit;

/// Cache key = frame content hash + region rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcrCacheKey {
    /// XXH3 (or similar fast non-crypto) hash of the cropped pixel buffer.
    pub frame_hash: u64,
    /// Region rect in physical pixels (virtual-desktop origin).
    pub region: PackedRect,
}

/// Hashable rect (Rect itself isn't Hash because of the integer types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackedRect {
    /// Origin x.
    pub x: i32,
    /// Origin y.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

impl From<Rect> for PackedRect {
    fn from(r: Rect) -> Self {
        Self { x: r.x, y: r.y, w: r.w, h: r.h }
    }
}

const CACHE_TTL_MS: u128 = 500;

struct Entry {
    hits: Vec<OcrHit>,
    inserted_at: Instant,
}

static CACHE: OnceLock<DashMap<OcrCacheKey, Entry>> = OnceLock::new();

fn cache() -> &'static DashMap<OcrCacheKey, Entry> {
    CACHE.get_or_init(DashMap::new)
}

/// Look up a cached result. Returns `Some` only if the entry is still
/// within TTL.
pub fn cache_lookup(key: OcrCacheKey) -> Option<Vec<OcrHit>> {
    let entry = cache().get(&key)?;
    if entry.inserted_at.elapsed().as_millis() > CACHE_TTL_MS {
        drop(entry);
        cache().remove(&key);
        return None;
    }
    Some(entry.hits.clone())
}

/// Insert a fresh result.
pub fn cache_store(key: OcrCacheKey, hits: Vec<OcrHit>) {
    cache().insert(key, Entry { hits, inserted_at: Instant::now() });
}

/// Cheap content hash for a pixel buffer. xxhash3 if available; otherwise
/// fall back to FNV-1a so the dependency stays optional.
pub fn frame_hash(pixels: &[u8]) -> u64 {
    // FNV-1a 64-bit. Adequate for cache-key partitioning; collisions only
    // cost a redundant OCR pass, not correctness.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pixels.iter().copied() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::redact::Redact;

    #[test]
    fn empty_buffer_hashes_to_offset_basis() {
        assert_eq!(frame_hash(&[]), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn different_buffers_hash_differently() {
        let a = frame_hash(&[1, 2, 3]);
        let b = frame_hash(&[1, 2, 4]);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_round_trip() {
        let key = OcrCacheKey {
            frame_hash: 12345,
            region: PackedRect { x: 0, y: 0, w: 100, h: 100 },
        };
        let hits = vec![OcrHit {
            text: Redact::new("Settings".to_string()),
            bounds: Rect { x: 10, y: 10, w: 50, h: 20 },
            confidence: 0.95,
        }];
        cache_store(key, hits.clone());
        let got = cache_lookup(key).expect("hit");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text.as_inner(), "Settings");
    }
}
