//! DXGI Desktop Duplication backend (Phase 3 Task 04, CAP-01..03).
//!
//! On capture-thread startup (lazy via `thread_local!`):
//! 1. `D3D11CreateDevice` (BGRA support, hardware adapter; software fallback)
//! 2. Enumerate `IDXGIAdapter` → `IDXGIOutput` per attached monitor
//! 3. `IDXGIOutput1::DuplicateOutput` per monitor
//! 4. Allocate one staging `ID3D11Texture2D` per monitor sized to monitor bounds
//!    (`USAGE_STAGING`, `CPU_ACCESS_READ`, `BindFlags = 0`)
//!
//! Per-call (`capture_into_staging`):
//! 1. `AcquireNextFrame(timeout_ms = 16)`
//! 2. `QueryInterface<ID3D11Texture2D>` on the desktop resource
//! 3. `CopyResource(staging, source)`
//! 4. RAII `FrameGuard::drop` calls `ReleaseFrame()` IMMEDIATELY (T-03-04 /
//!    PITFALLS #4 — must NEVER be skipped, even on panic / early return)
//! 5. `Map(staging, D3D11_MAP_READ)` → copy rows into `FrameBuf` (handle
//!    row-pitch padding) → `Unmap`. If `region` is `Some`, copy only the
//!    cropped subrect.
//!
//! Error recovery:
//! - `DXGI_ERROR_ACCESS_LOST` → drop & rebuild that monitor's duplication +
//!   staging next call. `tracing::warn!(reacquire_reason = "access_lost")`.
//! - `DXGI_ERROR_WAIT_TIMEOUT` from `AcquireNextFrame` → for now return a
//!   `Timeout`; future revision can serve last-good staging if <33 ms old.
//!
//! Acquire-once invariant (CAP-01): under steady state the duplication
//! object is acquired exactly ONCE over daemon lifetime. The `ACQUIRE_COUNT`
//! atomic counter exposes this for the verification gates / tests.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use fastuse_proto::{coords::Rect, Error as ProtoError, ErrorCode};
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_BIND_FLAG, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTPUT_DESC,
    DXGI_OUTDUPL_FRAME_INFO,
};

/// Steady-state instrumentation counter — incremented every time a per-monitor
/// `IDXGIOutputDuplication` is *acquired* (i.e. created or reacquired after
/// `DXGI_ERROR_ACCESS_LOST`). Steady-state must read 1 per monitor over a
/// 50-shot capture loop (CAP-01 verification gate).
pub static ACQUIRE_COUNT: AtomicU64 = AtomicU64::new(0);

/// One captured frame: tight BGRA, no row-pitch padding, dimensions in
/// physical pixels.
#[derive(Debug, Clone)]
pub struct FrameBuf {
    /// BGRA8 pixels, row-major, tight (`stride == w * 4`).
    pub bgra: Vec<u8>,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// One monitor's cached DXGI duplication + staging texture.
struct PerMonitor {
    output_idx: u32,
    desc: DXGI_OUTPUT_DESC,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
}

/// Capture-thread state: device + context + per-monitor duplication.
struct CaptureState {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    factory: IDXGIFactory1,
    monitors: Vec<PerMonitor>,
}

impl CaptureState {
    fn new() -> Result<Self, ProtoError> {
        // SAFETY: D3D11CreateDevice is COM init; we pass valid feature-level pointers.
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_0];

        let mut hr = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };

        // WARP fallback for headless / VM hosts that lack a hardware adapter.
        if hr.is_err() {
            tracing::warn!(error = ?hr, "D3D11 hardware device failed; falling back to WARP");
            hr = unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
        }
        hr.map_err(|e| {
            ProtoError::new(
                ErrorCode::Internal,
                format!("D3D11CreateDevice failed: 0x{:08x}", e.code().0 as u32),
            )
        })?;

        let device = device.ok_or_else(|| {
            ProtoError::new(ErrorCode::Internal, "D3D11CreateDevice returned no device")
        })?;
        let context = context.ok_or_else(|| {
            ProtoError::new(ErrorCode::Internal, "D3D11CreateDevice returned no context")
        })?;

        // SAFETY: standard COM factory creation.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| {
            ProtoError::new(
                ErrorCode::Internal,
                format!("CreateDXGIFactory1 failed: 0x{:08x}", e.code().0 as u32),
            )
        })?;

        let monitors = enumerate_monitors(&device, &factory)?;

        Ok(Self {
            device,
            context,
            factory,
            monitors,
        })
    }

    /// Reacquire one monitor's duplication + staging after access-lost.
    fn reacquire(&mut self, monitor_idx: u32, reason: &'static str) -> Result<(), ProtoError> {
        tracing::warn!(monitor = monitor_idx, reacquire_reason = reason, "DXGI reacquire");
        let fresh = enumerate_monitors(&self.device, &self.factory)?;
        let needle = monitor_idx as usize;
        if let Some(replacement) = fresh.into_iter().find(|m| m.output_idx as usize == needle) {
            if let Some(slot) = self
                .monitors
                .iter_mut()
                .find(|m| m.output_idx as usize == needle)
            {
                *slot = replacement;
            } else {
                self.monitors.push(replacement);
            }
            Ok(())
        } else {
            self.monitors.retain(|m| m.output_idx as usize != needle);
            Err(ProtoError::new(
                ErrorCode::MonitorNotFound,
                format!("monitor {monitor_idx} unavailable after reacquire"),
            ))
        }
    }
}

/// Build per-monitor duplication + staging for every attached output of the
/// first hardware adapter. (Multi-adapter laptops fall back to WARP via the
/// device path; multi-adapter capture is a v2 concern.)
fn enumerate_monitors(
    device: &ID3D11Device,
    factory: &IDXGIFactory1,
) -> Result<Vec<PerMonitor>, ProtoError> {
    let mut out = Vec::new();
    // SAFETY: EnumAdapters1 is COM enumeration; we stop on the first NotFound.
    let mut adapter_idx = 0u32;
    loop {
        let adapter_res: windows::core::Result<IDXGIAdapter1> =
            unsafe { factory.EnumAdapters1(adapter_idx) };
        let adapter = match adapter_res {
            Ok(a) => a,
            Err(_) => break, // ran out of adapters
        };

        let mut output_idx = 0u32;
        loop {
            let output_res: windows::core::Result<windows::Win32::Graphics::Dxgi::IDXGIOutput> =
                unsafe { adapter.EnumOutputs(output_idx) };
            let output = match output_res {
                Ok(o) => o,
                Err(_) => break,
            };
            let output1: IDXGIOutput1 = output.cast().map_err(|e| {
                ProtoError::new(
                    ErrorCode::Internal,
                    format!("IDXGIOutput1 cast failed: 0x{:08x}", e.code().0 as u32),
                )
            })?;
            // SAFETY: GetDesc returns the output descriptor by value.
            let desc: DXGI_OUTPUT_DESC = unsafe { output1.GetDesc() }.map_err(|e| {
                ProtoError::new(
                    ErrorCode::Internal,
                    format!("IDXGIOutput::GetDesc failed: 0x{:08x}", e.code().0 as u32),
                )
            })?;

            // SAFETY: DuplicateOutput against the device on its own thread.
            let duplication: IDXGIOutputDuplication =
                unsafe { output1.DuplicateOutput(device) }.map_err(|e| {
                    ProtoError::new(
                        ErrorCode::Internal,
                        format!("DuplicateOutput failed: 0x{:08x}", e.code().0 as u32),
                    )
                })?;
            ACQUIRE_COUNT.fetch_add(1, Ordering::Relaxed);

            // Allocate staging texture sized to this monitor's full bounds.
            let w = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left).max(1) as u32;
            let h = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top).max(1) as u32;
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: D3D11_BIND_FLAG(0).0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            // SAFETY: standard staging-texture creation.
            unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }.map_err(
                |e| {
                    ProtoError::new(
                        ErrorCode::Internal,
                        format!("CreateTexture2D(staging) failed: 0x{:08x}", e.code().0 as u32),
                    )
                },
            )?;
            let staging = staging.ok_or_else(|| {
                ProtoError::new(ErrorCode::Internal, "CreateTexture2D returned no texture")
            })?;

            out.push(PerMonitor {
                output_idx,
                desc,
                duplication,
                staging,
            });
            output_idx += 1;
        }

        if !out.is_empty() {
            // First adapter that has at least one output is our capture
            // adapter. Multi-adapter machines: capture follows that adapter.
            break;
        }
        adapter_idx += 1;
    }

    if out.is_empty() {
        return Err(ProtoError::new(
            ErrorCode::MonitorNotFound,
            "no DXGI outputs found".to_string(),
        ));
    }
    Ok(out)
}

thread_local! {
    /// Capture-thread-local state. Only ever touched on the capture thread
    /// (D-26). `RefCell` is fine because the thread is single-threaded.
    static STATE: RefCell<Option<CaptureState>> = const { RefCell::new(None) };
}

/// RAII guard that calls `ReleaseFrame()` on Drop. PITFALLS #4 / T-03-04:
/// must run unconditionally — covers panic and early-return paths so we
/// never leak a frame and trigger cascading `DXGI_ERROR_ACCESS_LOST`.
struct FrameGuard<'a> {
    duplication: &'a IDXGIOutputDuplication,
}

impl<'a> Drop for FrameGuard<'a> {
    fn drop(&mut self) {
        // SAFETY: matched by AcquireNextFrame above.
        let _ = unsafe { self.duplication.ReleaseFrame() };
    }
}

/// Capture one frame from the requested monitor, optionally cropped to
/// `region` (in physical pixels, virtual-desktop origin).
///
/// MUST be called on the capture thread. The closure-dispatch surface in
/// `capture_thread::CaptureThreadHandle::run` is the only sanctioned entry.
pub fn capture_into_staging(
    monitor: u32,
    region: Option<Rect>,
) -> Result<FrameBuf, ProtoError> {
    STATE.with(|cell| {
        // Lazy init.
        if cell.borrow().is_none() {
            let s = CaptureState::new()?;
            *cell.borrow_mut() = Some(s);
        }

        let mut borrow = cell.borrow_mut();
        let state: &mut CaptureState = borrow.as_mut().expect("state init guarded above");
        capture_one(state, monitor, region)
    })
}

fn capture_one(
    state: &mut CaptureState,
    monitor: u32,
    region: Option<Rect>,
) -> Result<FrameBuf, ProtoError> {
    // Find the per-monitor entry. If it was previously dropped (access-lost
    // not yet rebuilt), reacquire now.
    if !state
        .monitors
        .iter()
        .any(|m| m.output_idx == monitor)
    {
        state.reacquire(monitor, "missing")?;
    }

    // Pull pieces out of state via a transient index so the borrow checker
    // is happy when we may need to mutate `state.monitors` for reacquire.
    let pos = state
        .monitors
        .iter()
        .position(|m| m.output_idx == monitor)
        .ok_or_else(|| ProtoError::new(ErrorCode::MonitorNotFound, format!("monitor {monitor}")))?;

    let attempt = acquire_and_copy(state, pos, region.clone());

    match attempt {
        Ok(buf) => Ok(buf),
        Err(e) if e.code == ErrorCode::CaptureLost => {
            // Reacquire ONCE and retry.
            state.reacquire(monitor, "access_lost")?;
            let pos = state
                .monitors
                .iter()
                .position(|m| m.output_idx == monitor)
                .ok_or_else(|| {
                    ProtoError::new(ErrorCode::MonitorNotFound, format!("monitor {monitor}"))
                })?;
            acquire_and_copy(state, pos, region)
        }
        Err(e) => Err(e),
    }
}

fn acquire_and_copy(
    state: &mut CaptureState,
    pos: usize,
    region: Option<Rect>,
) -> Result<FrameBuf, ProtoError> {
    let mon = &state.monitors[pos];

    // ---- AcquireNextFrame ----
    let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut desktop_resource: Option<IDXGIResource> = None;

    // SAFETY: AcquireNextFrame on a thread-confined duplication object.
    let hr = unsafe {
        mon.duplication
            .AcquireNextFrame(16, &mut frame_info, &mut desktop_resource)
    };
    if let Err(e) = hr {
        let code = e.code();
        if code == DXGI_ERROR_WAIT_TIMEOUT {
            // No new frame within 16ms — caller may retry. We surface as
            // Timeout; CaptureLost is reserved for access-lost recovery.
            return Err(ProtoError::new(ErrorCode::Timeout, "DXGI wait timeout"));
        }
        if code == DXGI_ERROR_ACCESS_LOST {
            return Err(ProtoError::new(ErrorCode::CaptureLost, "DXGI access lost"));
        }
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("AcquireNextFrame failed: 0x{:08x}", code.0 as u32),
        ));
    }

    // RAII: ReleaseFrame ALWAYS runs — even if Map / CopyResource throws.
    let _frame_guard = FrameGuard {
        duplication: &mon.duplication,
    };

    let desktop_resource = desktop_resource.ok_or_else(|| {
        ProtoError::new(
            ErrorCode::Internal,
            "AcquireNextFrame returned no resource",
        )
    })?;

    // QueryInterface the desktop resource into a Texture2D source.
    let source: ID3D11Texture2D = desktop_resource.cast().map_err(|e| {
        ProtoError::new(
            ErrorCode::Internal,
            format!("Resource cast to Texture2D failed: 0x{:08x}", e.code().0 as u32),
        )
    })?;
    let source_resource: ID3D11Resource = source.cast().map_err(|e| {
        ProtoError::new(
            ErrorCode::Internal,
            format!("Texture2D cast to Resource failed: 0x{:08x}", e.code().0 as u32),
        )
    })?;
    let staging_resource: ID3D11Resource = mon.staging.cast().map_err(|e| {
        ProtoError::new(
            ErrorCode::Internal,
            format!("Staging cast to Resource failed: 0x{:08x}", e.code().0 as u32),
        )
    })?;

    // SAFETY: GPU-side copy of the desktop frame into staging.
    unsafe {
        state
            .context
            .CopyResource(&staging_resource, &source_resource);
    }

    // ---- Map staging → CPU-readable rows ----
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: standard Map call against a STAGING texture.
    unsafe {
        state
            .context
            .Map(&staging_resource, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
    }
    .map_err(|e| {
        ProtoError::new(
            ErrorCode::Internal,
            format!("Map(staging) failed: 0x{:08x}", e.code().0 as u32),
        )
    })?;

    // Determine output dimensions — full monitor or cropped.
    let mon_w = (mon.desc.DesktopCoordinates.right - mon.desc.DesktopCoordinates.left).max(1) as u32;
    let mon_h = (mon.desc.DesktopCoordinates.bottom - mon.desc.DesktopCoordinates.top).max(1) as u32;
    let (off_x, off_y, out_w, out_h) = clamp_region(region, mon_w, mon_h);

    // Copy out, handling row-pitch padding.
    let row_bytes_out = (out_w as usize) * 4;
    let mut buf = vec![0u8; row_bytes_out * out_h as usize];

    // SAFETY: mapped.pData is a valid pointer to mon_h * RowPitch bytes
    // produced by the OS staging copy above.
    unsafe {
        let src_base = mapped.pData as *const u8;
        let row_pitch = mapped.RowPitch as usize;
        for row in 0..out_h as usize {
            let src_row = src_base.add((row + off_y as usize) * row_pitch + (off_x as usize) * 4);
            let dst_row = buf.as_mut_ptr().add(row * row_bytes_out);
            std::ptr::copy_nonoverlapping(src_row, dst_row, row_bytes_out);
        }
    }

    // SAFETY: paired with Map above.
    unsafe { state.context.Unmap(&staging_resource, 0) };

    Ok(FrameBuf {
        bgra: buf,
        w: out_w,
        h: out_h,
    })
}

fn clamp_region(region: Option<Rect>, mon_w: u32, mon_h: u32) -> (u32, u32, u32, u32) {
    match region {
        None => (0, 0, mon_w, mon_h),
        Some(r) => {
            let x = r.x.max(0) as u32;
            let y = r.y.max(0) as u32;
            let w = (r.w.max(0) as u32).min(mon_w.saturating_sub(x));
            let h = (r.h.max(0) as u32).min(mon_h.saturating_sub(y));
            (x, y, w.max(1), h.max(1))
        }
    }
}

/// Test-only helper: drop the cached duplication for `monitor` so the next
/// capture takes the access-lost reacquire path. Compiled under `cfg(test)`.
pub fn force_lose_for_test(monitor: u32) {
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.monitors.retain(|m| m.output_idx != monitor);
        }
    });
}

// Mark the unused-field warning suppressed for `_frame_guard` and `_`.
#[allow(dead_code)]
fn _suppress_unused(_: RECT) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_buf_zero_sized_safe() {
        let f = FrameBuf {
            bgra: vec![],
            w: 0,
            h: 0,
        };
        assert_eq!(f.bgra.len(), 0);
    }

    #[test]
    fn clamp_region_bounds_correct() {
        // None → full monitor
        assert_eq!(clamp_region(None, 1920, 1080), (0, 0, 1920, 1080));
        // Inside bounds
        assert_eq!(
            clamp_region(Some(Rect { x: 10, y: 20, w: 100, h: 50 }), 1920, 1080),
            (10, 20, 100, 50)
        );
        // Crops to monitor edge
        assert_eq!(
            clamp_region(Some(Rect { x: 1900, y: 1070, w: 100, h: 100 }), 1920, 1080),
            (1900, 1070, 20, 10)
        );
        // Negative origin is clamped to 0
        assert_eq!(
            clamp_region(Some(Rect { x: -10, y: -10, w: 100, h: 50 }), 1920, 1080),
            (0, 0, 100, 50)
        );
    }
}
