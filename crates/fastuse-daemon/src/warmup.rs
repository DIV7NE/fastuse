//! Warmup fan-out: touches D3D11, DXGI dup, UIA root, foreground cache,
//! monitor enum. Idempotent — running twice is cheap (subsequent calls
//! hit the warm caches).

use std::time::Instant;

use fastuse_proto::Response;
use fastuse_win::capture_thread::CaptureThreadHandle;
use fastuse_win::uia_pool::UiaPoolHandle;

pub fn run(uia: Option<&UiaPoolHandle>, capture: Option<&CaptureThreadHandle>) -> Response {
    let total = Instant::now();

    // Monitors
    let m_start = Instant::now();
    let _ = fastuse_win::window::monitors::list_monitors();
    let monitors_us = m_start.elapsed().as_micros() as u64;

    // Capture — touch DXGI duplication path
    let c_start = Instant::now();
    if let Some(cap) = capture {
        let _ = fastuse_win::capture::handle_screenshot(
            cap,
            None,
            Some(fastuse_proto::ImageFormat::Jpeg),
        );
    }
    let capture_us = c_start.elapsed().as_micros() as u64;

    // UIA root + foreground HWND cache
    let u_start = Instant::now();
    if let Some(pool) = uia {
        let _ = pool.run(|uia_ref| {
            // Resolve foreground HWND and populate the UIA element cache.
            if let Some(hwnd) = fastuse_win::uia::automation::foreground_hwnd() {
                let _ = fastuse_win::uia::cache::get_or_fetch(uia_ref, hwnd);
            }
            Ok::<_, fastuse_proto::Error>(())
        });
    }
    let uia_us = u_start.elapsed().as_micros() as u64;

    Response::Warmup {
        capture_us,
        uia_us,
        monitors_us,
        total_us: total.elapsed().as_micros() as u64,
    }
}
