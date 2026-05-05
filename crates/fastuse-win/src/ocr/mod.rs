//! OCR orchestration. The dedicated MTA thread is in `ocr_thread`; this
//! module owns the cropping policy and frame-hash cache.

pub mod cache;
pub mod cropped;

pub use cache::{cache_lookup, cache_store, frame_hash, OcrCacheKey, PackedRect};
pub use cropped::{ocr_cropped_progressive, OcrHit};
