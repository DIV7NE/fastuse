//! `EnumProcesses` + `OpenProcess` + `QueryFullProcessImageNameW` enumeration.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fastuse_proto::{ProcFilter, ProcessInfo};
use once_cell::sync::Lazy;

use windows::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};

const CACHE_TTL: Duration = Duration::from_secs(1);

static CACHE: Lazy<Mutex<Option<(Instant, Vec<ProcessInfo>)>>> = Lazy::new(|| Mutex::new(None));

/// Invalidate the process-list cache. Called from `kill_process`.
pub fn invalidate_cache() {
    if let Ok(mut g) = CACHE.lock() {
        *g = None;
    }
}

/// Enumerate running processes. Cached for [`CACHE_TTL`] (1s) to amortize
/// the syscall storm.
#[tracing::instrument(skip(filter))]
pub fn list_processes(filter: Option<ProcFilter>) -> Vec<ProcessInfo> {
    let mut all = cached_full_list();
    if let Some(f) = filter {
        if let Some(needle) = f.name_contains.as_ref() {
            let n = needle.to_ascii_lowercase();
            all.retain(|p| p.name.to_ascii_lowercase().contains(&n));
        }
        if matches!(f.visible_only, Some(true)) {
            all.retain(|p| p.main_hwnd.is_some());
        }
    }
    all
}

fn cached_full_list() -> Vec<ProcessInfo> {
    if let Ok(g) = CACHE.lock() {
        if let Some((t, list)) = g.as_ref() {
            if t.elapsed() < CACHE_TTL {
                return list.clone();
            }
        }
    }
    let fresh = enumerate_processes();
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

fn enumerate_processes() -> Vec<ProcessInfo> {
    // Grow buffer until EnumProcesses returns less than capacity.
    let mut buf: Vec<u32> = vec![0; 1024];
    loop {
        let mut needed: u32 = 0;
        let cap_bytes = (buf.len() * std::mem::size_of::<u32>()) as u32;
        // SAFETY: buf has cap_bytes capacity; needed is u32 receiver.
        let res = unsafe { EnumProcesses(buf.as_mut_ptr(), cap_bytes, &mut needed) };
        if res.is_err() {
            return Vec::new();
        }
        let returned = (needed / std::mem::size_of::<u32>() as u32) as usize;
        if returned < buf.len() {
            buf.truncate(returned);
            break;
        }
        // Buffer was full — grow and retry to ensure we got everything.
        buf = vec![0; buf.len() * 2];
    }

    let mut out: Vec<ProcessInfo> = Vec::with_capacity(buf.len());
    for &pid in &buf {
        if pid == 0 {
            continue;
        }
        if let Some(info) = process_info_for(pid) {
            out.push(info);
        }
    }
    out
}

fn process_info_for(pid: u32) -> Option<ProcessInfo> {
    // SAFETY: OpenProcess with a benign access right; we always Close on drop.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let exe_path = read_image_name(handle);
    // SAFETY: handle owned, single Close.
    unsafe {
        let _ = CloseHandle(handle);
    }
    let exe_path = exe_path?;
    let name = Path::new(&exe_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&exe_path)
        .to_string();
    Some(ProcessInfo {
        pid,
        name,
        exe_path: Some(exe_path),
        main_hwnd: None,
    })
}

fn read_image_name(handle: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; MAX_PATH as usize];
    let mut size: u32 = buf.len() as u32;
    // SAFETY: handle is valid; buf big enough; size in/out.
    let r = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    if r.is_err() || size == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..size as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_finds_explorer() {
        // Drop cache first so we always re-enumerate fresh.
        invalidate_cache();
        let list = list_processes(None);
        assert!(!list.is_empty(), "expected at least 1 process");
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        // explorer.exe should always be present in an interactive session.
        // If we're running headless (CI), this may fail — keep informational.
        let has_explorer = names.iter().any(|n| n.eq_ignore_ascii_case("explorer"));
        // Don't assert hard — CI/headless boxes may not have it. Just print.
        if !has_explorer {
            eprintln!("note: explorer.exe not found (headless environment?)");
        }
    }

    #[test]
    fn name_filter_works() {
        invalidate_cache();
        let all = list_processes(None);
        if let Some(any) = all.first() {
            let needle = any.name.clone();
            let filtered = list_processes(Some(ProcFilter {
                name_contains: Some(needle.clone()),
                visible_only: None,
            }));
            assert!(!filtered.is_empty());
            for p in &filtered {
                assert!(p.name.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()));
            }
        }
    }

    #[test]
    fn cache_returns_quickly_on_repeat() {
        invalidate_cache();
        let _ = list_processes(None);
        let t = Instant::now();
        let _ = list_processes(None);
        assert!(t.elapsed() < Duration::from_millis(50), "cached call too slow");
    }
}
