//! `--drag-helper` mode: the OLE drag source.
//!
//! This is the same binary as the daemon, re-executed at Medium integrity by
//! [`fastuse_win::files::deelevate::spawn_medium_il`]. It never becomes the
//! daemon: `main` branches here before the singleton, the sentinel or the
//! pipe.
//!
//! Protocol, two newline-delimited JSON objects:
//!
//! ```text
//! daemon → helper (stdin) : one DragJob, one line
//! helper → daemon (stdout): {"ready":true}   once the window exists
//! helper → daemon (stdout): one DragOutcome  once the drag ends
//! ```
//!
//! The daemon MUST terminate the job with a newline. `read_line` blocks until
//! one arrives, so a job written without it — on a pipe the daemon keeps open —
//! wedges the helper *before* the ready line while the daemon waits for that
//! line. Nothing reports it: stderr is deliberately not connected by the
//! spawner, so nothing but those two lines can appear on the stream the daemon
//! parses. `MediumIlChild::wait` being unconditionally timed is what recovers
//! from that, and is the reason the null stderr is survivable.
//!
//! The daemon must not inject the button-down until it has read the ready
//! line: before that the 1×1 source window does not exist, and the click
//! lands on whatever the user had under the cursor.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use windows::core::implement;
use windows::Win32::Foundation::{
    GlobalFree, COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, E_NOTIMPL, HGLOBAL, HWND, LPARAM, LRESULT, S_OK, WPARAM,
};
use windows::Win32::System::Com::{
    CoInitializeEx, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA,
    COINIT_APARTMENTTHREADED, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::CFSTR_PREFERREDDROPEFFECT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, RegisterClassW,
    SetLayeredWindowAttributes, TranslateMessage, LWA_ALPHA, MSG, PM_REMOVE, WM_LBUTTONDOWN,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows_core::{Ref, BOOL, HRESULT, PCWSTR};

/// One drag job, daemon → helper over stdin.
#[derive(serde::Deserialize, serde::Serialize)]
pub struct DragJob {
    /// Already resolved and validated by the daemon.
    pub paths: Vec<String>,
    /// Where the 1×1 source window goes, virtual-desktop pixels.
    pub start_x: i32,
    /// See [`DragJob::start_x`].
    pub start_y: i32,
    /// Hard deadline; QueryContinueDrag cancels past it so a drag can never
    /// wedge holding the user's mouse button down.
    ///
    /// One budget covers everything after the ready line: the daemon's
    /// injection *and* the drag itself. Size it for both — a deadline spent on
    /// injection latency leaves `QueryContinueDrag` cancelling almost at once,
    /// which is reported as a cancel and not distinguishable from escape.
    pub deadline_ms: u32,
}

/// Outcome, helper → daemon over stdout.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DragOutcome {
    /// True when DoDragDrop returned DRAGDROP_S_DROP.
    pub dropped: bool,
    /// The DROPEFFECT the target reported.
    pub effect: u32,
    /// Present when the drag failed.
    pub error: Option<String>,
}

impl DragOutcome {
    fn failed(msg: impl Into<String>) -> Self {
        Self {
            dropped: false,
            effect: 0,
            error: Some(msg.into()),
        }
    }
}

/// Run the helper: read one job, offer it as an OLE drag source, print the
/// outcome. Always prints exactly one [`DragOutcome`] line, including on
/// failure, so the daemon never has to distinguish "crashed" from "refused".
pub fn run() {
    let outcome = match read_job() {
        Ok(job) => drag(&job),
        Err(e) => DragOutcome::failed(e),
    };
    let line = serde_json::to_string(&outcome).unwrap_or_else(|_| {
        r#"{"dropped":false,"effect":0,"error":"outcome not serializable"}"#.to_string()
    });
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn read_job() -> Result<DragJob, String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("reading the drag job from stdin failed: {e}"))?;
    serde_json::from_str(&line).map_err(|e| format!("the drag job is not valid JSON: {e}"))
}

fn drag(job: &DragJob) -> DragOutcome {
    // DoDragDrop needs an STA with OLE initialized, not just COM. OleInitialize
    // implies CoInitializeEx(APARTMENTTHREADED), but calling both keeps the
    // apartment model explicit at the top of the only thread that matters.
    // SAFETY: this is the first COM call on this thread of a fresh process.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // SAFETY: as above; paired with OleUninitialize below.
    if let Err(e) = unsafe { OleInitialize(None) } {
        return DragOutcome::failed(format!("OleInitialize failed: {e}"));
    }

    // Build the payload before the window exists, so an unusable job fails
    // before the ready line and therefore before the daemon injects a real
    // mouse click onto the user's desktop.
    let data = match HdropData::new(&job.paths) {
        Ok(d) => IDataObject::from(d),
        Err(e) => {
            // SAFETY: OleInitialize succeeded above.
            unsafe { OleUninitialize() };
            return DragOutcome::failed(e);
        }
    };

    let hwnd = match create_source_window(job.start_x, job.start_y) {
        Ok(h) => h,
        Err(e) => {
            // SAFETY: OleInitialize succeeded above.
            unsafe { OleUninitialize() };
            return DragOutcome::failed(e);
        }
    };

    // Only now is there a window at (start_x, start_y) to receive the click.
    {
        let mut out = std::io::stdout();
        let _ = writeln!(out, r#"{{"ready":true}}"#);
        let _ = out.flush();
    }

    let outcome = pump_until_drag(job, &data);

    // SAFETY: `hwnd` was created on this thread and is still alive.
    let _ = unsafe { DestroyWindow(hwnd) };
    // SAFETY: paired with the OleInitialize above.
    unsafe { OleUninitialize() };
    outcome
}

/// Pump this thread's queue until the injected button-down arrives, then run
/// the drag from inside it.
///
/// Calling `DoDragDrop` from the button-down the window actually received —
/// rather than blindly after the injection — is the sequence the API is built
/// around, and it makes the mouse-capture handoff correct by construction.
fn pump_until_drag(job: &DragJob, data: &IDataObject) -> DragOutcome {
    let deadline = Instant::now() + Duration::from_millis(u64::from(job.deadline_ms));
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a live local; a null hwnd filter takes every
        // message posted to this thread.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_LBUTTONDOWN {
                return do_drag(data, deadline);
            }
            // SAFETY: `msg` was just filled by PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if Instant::now() >= deadline {
            return DragOutcome::failed(
                "no left-button-down reached the drag source window before the deadline",
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn do_drag(data: &IDataObject, deadline: Instant) -> DragOutcome {
    let source: IDropSource = DragSource { deadline }.into();
    let mut effect = DROPEFFECT_COPY;
    // SAFETY: both interfaces are live for the duration of the modal call,
    // which returns before they are dropped.
    let hr = unsafe { DoDragDrop(data, &source, DROPEFFECT_COPY, &mut effect) };
    if hr == DRAGDROP_S_DROP {
        DragOutcome {
            dropped: true,
            effect: effect.0,
            error: None,
        }
    } else if hr == DRAGDROP_S_CANCEL {
        DragOutcome::failed("the drag was cancelled before a drop")
    } else {
        DragOutcome::failed(format!("DoDragDrop failed: {hr:?}"))
    }
}

/// The source window has no behaviour of its own; the drag runs from the
/// message pump, which reads `WM_LBUTTONDOWN` before dispatching here.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: forwarding the arguments the window manager just handed us.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

fn create_source_window(x: i32, y: i32) -> Result<HWND, String> {
    let class = windows_core::w!("fastuse_drag_source");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        lpszClassName: class,
        ..Default::default()
    };
    // SAFETY: `wc` is a live local; a duplicate-class failure is caught by
    // CreateWindowExW below, which is the only caller.
    unsafe { RegisterClassW(&wc) };

    // SAFETY: every pointer argument is a live local or a static string.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            class,
            PCWSTR::null(),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            1,
            1,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("creating the drag source window failed: {e}"))?;

    // Alpha 1, not 0: a fully transparent layered window can fall out of
    // hit-testing, and this window exists precisely to be hit.
    // SAFETY: `hwnd` was just created on this thread.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA) }
        .map_err(|e| format!("making the drag source window layered failed: {e}"))?;
    Ok(hwnd)
}

/// The drop source. Owns nothing but the deadline.
#[implement(IDropSource)]
struct DragSource {
    deadline: Instant,
}

impl IDropSource_Impl for DragSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        continue_drag_hr(
            fescapepressed.as_bool(),
            grfkeystate & MK_LBUTTON != MODIFIERKEYS_FLAGS(0),
            Instant::now() >= self.deadline,
        )
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// The `QueryContinueDrag` decision, without the COM plumbing.
///
/// The deadline branch is what stops a drag wedging with the user's mouse
/// button held down: nothing else in the loop can end a drag whose target
/// never releases.
fn continue_drag_hr(escape: bool, left_down: bool, expired: bool) -> HRESULT {
    if escape || expired {
        DRAGDROP_S_CANCEL
    } else if !left_down {
        DRAGDROP_S_DROP
    } else {
        S_OK
    }
}

/// The drag payload: `CF_HDROP` plus the registered `Preferred DropEffect`.
///
/// Targets that only look at `CF_HDROP` work without the second format, but
/// Explorer and Chrome both read `Preferred DropEffect` to decide whether the
/// gesture was a copy or a move, and a source that omits it can get a move.
#[implement(IDataObject)]
struct HdropData {
    hdrop: Vec<u8>,
    preferred_effect_cf: u16,
}

impl HdropData {
    fn new(paths: &[String]) -> Result<Self, String> {
        if paths.is_empty() {
            return Err("paths: at least one path is required".to_string());
        }
        // SAFETY: the format name is a static wide string.
        let cf = unsafe { RegisterClipboardFormatW(CFSTR_PREFERREDDROPEFFECT) };
        if cf == 0 {
            return Err("RegisterClipboardFormatW(Preferred DropEffect) failed".to_string());
        }
        Ok(Self {
            hdrop: fastuse_win::files::hdrop::build_hdrop(paths),
            preferred_effect_cf: cf as u16,
        })
    }

    /// Whether `fmt` names one of our two formats.
    ///
    /// `dwAspect` and `lindex` are deliberately ignored: targets probe with
    /// values we have no aspect-specific rendering for, and refusing those
    /// loses drops for no gain.
    ///
    /// Separate from [`HdropData::payload_for`] because `QueryGetData` fires
    /// repeatedly while the cursor moves and must not build a payload to
    /// answer yes or no.
    fn serves(&self, fmt: &FORMATETC) -> bool {
        fmt.tymed & TYMED_HGLOBAL.0 as u32 != 0
            && (fmt.cfFormat == CF_HDROP.0 || fmt.cfFormat == self.preferred_effect_cf)
    }

    /// The bytes for `fmt`, or `None` if we do not serve it.
    fn payload_for(&self, fmt: &FORMATETC) -> Option<Vec<u8>> {
        if !self.serves(fmt) {
            None
        } else if fmt.cfFormat == CF_HDROP.0 {
            Some(self.hdrop.clone())
        } else {
            Some(DROPEFFECT_COPY.0.to_le_bytes().to_vec())
        }
    }
}

impl IDataObject_Impl for HdropData_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows_core::Result<STGMEDIUM> {
        // SAFETY: COM guarantees a valid pointer for the duration of the call.
        let fmt = unsafe { pformatetcin.as_ref() }.ok_or_else(|| {
            windows_core::Error::from_hresult(windows::Win32::Foundation::E_POINTER)
        })?;
        let bytes = self
            .payload_for(fmt)
            .ok_or_else(|| windows_core::Error::from_hresult(DV_E_FORMATETC))?;
        // A fresh HGLOBAL per call: the consumer calls ReleaseStgMedium, which
        // GlobalFrees it. Handing back a stored handle twice is a double free.
        let hglobal = alloc_hglobal(&bytes)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _f: *const FORMATETC, _m: *mut STGMEDIUM) -> windows_core::Result<()> {
        Err(windows_core::Error::from_hresult(E_NOTIMPL))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: COM guarantees a valid pointer for the duration of the call.
        match unsafe { pformatetc.as_ref() } {
            Some(fmt) if self.serves(fmt) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _in: *const FORMATETC, _out: *mut FORMATETC) -> HRESULT {
        // We have no device-specific renderings, so there is nothing to
        // canonicalize; callers treat E_NOTIMPL as "use what you asked for".
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _f: *const FORMATETC,
        _m: *const STGMEDIUM,
        _release: BOOL,
    ) -> windows_core::Result<()> {
        Err(windows_core::Error::from_hresult(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, _dir: u32) -> windows_core::Result<IEnumFORMATETC> {
        // Both known targets (Explorer, Chromium) call QueryGetData, not
        // EnumFormatEtc. Implement IEnumFORMATETC over the two entries only if
        // a real target is found to refuse the drop without it.
        Err(windows_core::Error::from_hresult(E_NOTIMPL))
    }

    fn DAdvise(
        &self,
        _f: *const FORMATETC,
        _advf: u32,
        _sink: Ref<IAdviseSink>,
    ) -> windows_core::Result<u32> {
        Err(windows_core::Error::from_hresult(E_NOTIMPL))
    }

    fn DUnadvise(&self, _connection: u32) -> windows_core::Result<()> {
        Err(windows_core::Error::from_hresult(E_NOTIMPL))
    }

    fn EnumDAdvise(&self) -> windows_core::Result<IEnumSTATDATA> {
        Err(windows_core::Error::from_hresult(E_NOTIMPL))
    }
}

fn alloc_hglobal(bytes: &[u8]) -> windows_core::Result<HGLOBAL> {
    // SAFETY: a plain allocation request.
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }?;
    // SAFETY: `h` was just allocated and is not yet locked elsewhere.
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        // SAFETY: `h` is live and unlocked; we are the only owner.
        let _ = unsafe { GlobalFree(Some(h)) };
        return Err(windows_core::Error::from_thread());
    }
    // SAFETY: `p` points at `bytes.len()` writable bytes just allocated.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len()) };
    // SAFETY: paired with the GlobalLock above.
    let _ = unsafe { GlobalUnlock(h) };
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_and_outcome_round_trip() {
        let job = DragJob {
            paths: vec![r"C:\Windows\win.ini".to_string()],
            start_x: 400,
            start_y: 401,
            deadline_ms: 10_000,
        };
        let back: DragJob = serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(back.paths, job.paths);
        assert_eq!((back.start_x, back.start_y), (400, 401));
        assert_eq!(back.deadline_ms, 10_000);

        let out = DragOutcome {
            dropped: true,
            effect: 1,
            error: None,
        };
        let text = serde_json::to_string(&out).unwrap();
        // The daemon parses this line; the field names are the wire contract.
        assert!(text.contains(r#""dropped":true"#), "got {text}");
        let back: DragOutcome = serde_json::from_str(&text).unwrap();
        assert!(back.dropped && back.effect == 1 && back.error.is_none());
    }

    #[test]
    fn a_job_line_is_accepted_in_the_form_the_daemon_writes_it() {
        let line =
            r#"{"paths":["C:\\Windows\\win.ini"],"start_x":400,"start_y":400,"deadline_ms":10000}"#;
        let job: DragJob = serde_json::from_str(line).unwrap();
        assert_eq!(job.paths, vec![r"C:\Windows\win.ini".to_string()]);
    }

    /// The ownership contract `GetData` has to honour: a fresh HGLOBAL every
    /// call, because the consumer's ReleaseStgMedium frees it. Reusing one
    /// stored handle would be a double free, and this is the only test that
    /// drives the real COM entry point.
    #[test]
    fn get_data_allocates_a_fresh_hglobal_every_call() {
        let obj = IDataObject::from(HdropData::new(&[r"C:\Windows\win.ini".to_string()]).unwrap());
        let fmt = FORMATETC {
            cfFormat: CF_HDROP.0,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        // SAFETY: `obj` is live and `fmt` is a live local.
        let (a, b) = unsafe { (obj.GetData(&fmt).unwrap(), obj.GetData(&fmt).unwrap()) };
        assert_eq!(a.tymed, TYMED_HGLOBAL.0 as u32);
        // SAFETY: the union is HGLOBAL because tymed says TYMED_HGLOBAL.
        let (ha, hb) = unsafe { (a.u.hGlobal, b.u.hGlobal) };
        assert!(
            !ha.is_invalid() && !hb.is_invalid(),
            "GetData returned a null HGLOBAL"
        );
        assert_ne!(
            ha.0, hb.0,
            "two calls handed back the same HGLOBAL: a double free"
        );
        // SAFETY: we own both; nothing else has taken them. GlobalFree returns
        // NULL on success, which the binding reports as an Err, so the result
        // is not a useful signal here.
        unsafe {
            let _ = GlobalFree(Some(ha));
            let _ = GlobalFree(Some(hb));
        }

        // And an unserved format is refused rather than allocated.
        let text = FORMATETC {
            cfFormat: 1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        // SAFETY: as above.
        assert!(unsafe { obj.GetData(&text) }.is_err());
    }

    #[test]
    fn drag_continues_only_while_the_button_is_down_and_time_remains() {
        assert_eq!(continue_drag_hr(false, true, false), S_OK);
        assert_eq!(continue_drag_hr(false, false, false), DRAGDROP_S_DROP);
        assert_eq!(continue_drag_hr(true, true, false), DRAGDROP_S_CANCEL);
        // The deadline outranks a still-held button — that is the whole point
        // of having one.
        assert_eq!(continue_drag_hr(false, true, true), DRAGDROP_S_CANCEL);
        // ...and outranks a release, so an expired drag never reports a drop.
        assert_eq!(continue_drag_hr(false, false, true), DRAGDROP_S_CANCEL);
    }

    #[test]
    fn only_the_two_advertised_formats_are_served() {
        let d = HdropData::new(&[r"C:\Windows\win.ini".to_string()]).unwrap();
        let hdrop = FORMATETC {
            cfFormat: CF_HDROP.0,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        assert_eq!(d.payload_for(&hdrop).unwrap(), d.hdrop);

        let effect = FORMATETC {
            cfFormat: d.preferred_effect_cf,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        assert_eq!(d.payload_for(&effect).unwrap(), vec![1, 0, 0, 0]);

        // CF_TEXT is not ours.
        let text = FORMATETC {
            cfFormat: 1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        assert!(d.payload_for(&text).is_none());

        // Right format, wrong medium: TYMED_ISTREAM only.
        let stream = FORMATETC {
            cfFormat: CF_HDROP.0,
            tymed: 4,
            ..Default::default()
        };
        assert!(d.payload_for(&stream).is_none());
    }
}
