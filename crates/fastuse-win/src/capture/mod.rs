//! Phase 3 capture module: DXGI Desktop Duplication backend, encoder, and
//! screenshot/screenshot_region handlers.
//!
//! D-26 invariant: every D3D11 / DXGI call lives on the single MTA capture
//! thread (see `capture_thread.rs`). Tokio workers never touch this surface.
//!
//! Architecture:
//! - `CaptureState` (Task 04) owns `ID3D11Device`, the per-monitor
//!   `IDXGIOutputDuplication`, and the per-monitor staging `ID3D11Texture2D`.
//!   It lives in a `thread_local!` initialized lazily on the capture thread,
//!   so the `run<F>` closure-dispatch surface (`CaptureThreadHandle::run`)
//!   can borrow it without enum-variant proliferation (D-28).
//! - On `DXGI_ERROR_ACCESS_LOST` we drop the per-monitor entry and rebuild
//!   on next call (CAP-03). `tracing::warn!(reacquire_reason = ?)` logs the
//!   cause.
//! - `FrameBuf` is the in-process result of one capture (BGRA, tight
//!   row-pitch, dimensions). The encoder (`encode.rs`) takes &FrameBuf and
//!   produces JPEG / PNG bytes.

pub mod dxgi;
pub mod encode;
pub mod screenshot;

pub use dxgi::{capture_into_staging, force_lose_for_test, FrameBuf};
pub use encode::{encode, encode_jpeg_rgba, EncodedImage};
pub use screenshot::{
    handle_screenshot, handle_screenshot_region, handle_screenshot_v2, handle_zoom_v2,
    ScreenshotV2Raw,
};
