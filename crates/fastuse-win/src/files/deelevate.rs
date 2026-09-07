//! Spawn a child at Medium integrity from our High-integrity daemon.
//!
//! OLE drag-drop reverses the direction of the data flow: the drop target
//! calls back into the source's `IDataObject`. A medium-IL Chrome cannot make
//! that call into a high-IL process, which is the same reason a file cannot be
//! dragged from an elevated Explorer into a normal application. So the drag
//! source has to live somewhere Chrome can call.
//!
//! The token comes from `explorer.exe` (approach B in the Task 7 spike), not
//! from lowering our own token's integrity label. Both produce a Medium-IL
//! child, but a relabelled copy of the daemon's token still carries
//! `BUILTIN\Administrators` as an enabled group, while the shell's token
//! carries it deny-only. The helper only needs to read the dragged files and
//! talk COM to a medium-IL target, so it gets the ordinary user token.

use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use fastuse_proto::{Error as ProtoError, ErrorCode};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TokenPrimary, SECURITY_ATTRIBUTES,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessWithTokenW, GetExitCodeProcess, OpenProcess, OpenProcessToken, TerminateProcess,
    WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::GetShellWindow;
use windows_core::{PCWSTR, PWSTR};

/// A running Medium-integrity child with its stdio wired to two anonymous
/// pipes.
///
/// This is not a [`std::process::Child`]: `std` offers no way to adopt a
/// process it did not itself create (`FromRawHandle` is implemented for
/// `Stdio`, not for `Child`), and [`CreateProcessWithTokenW`] is not what
/// `std::process::Command` calls. So the process handle is held directly.
///
/// Dropping this closes the process handle and both pipe ends; it does *not*
/// kill the child. Call [`MediumIlChild::kill`] for that.
#[derive(Debug)]
pub struct MediumIlChild {
    process: OwnedHandle,
    pid: u32,
    /// Write end of the child's stdin.
    pub stdin: File,
    /// Read end of the child's stdout. The child's stderr is not connected.
    pub stdout: File,
}

impl MediumIlChild {
    /// Process id of the child, for logging.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait up to `timeout_ms` for the child to exit.
    ///
    /// `Ok(Some(code))` on exit, `Ok(None)` on timeout. There is no untimed
    /// wait on purpose: a drag helper that has taken mouse capture and wedged
    /// must be reaped, not waited on forever.
    pub fn wait(&mut self, timeout_ms: u32) -> Result<Option<u32>, ProtoError> {
        // SAFETY: self.process is a live process handle owned by this struct.
        let waited = unsafe { WaitForSingleObject(raw(&self.process), timeout_ms) };
        if waited == WAIT_TIMEOUT {
            return Ok(None);
        }
        if waited != WAIT_OBJECT_0 {
            return Err(ProtoError::new(
                ErrorCode::HelperSpawnFailed,
                format!("waiting on drag helper failed: WAIT status {}", waited.0),
            ));
        }
        let mut code = 0u32;
        // SAFETY: same handle; `code` is a live local.
        unsafe { GetExitCodeProcess(raw(&self.process), &mut code) }
            .map_err(|e| win_err("GetExitCodeProcess", &e))?;
        Ok(Some(code))
    }

    /// Terminate the child. Best-effort: a child that already exited reports
    /// an error here, which is not interesting to the caller.
    pub fn kill(&mut self) {
        // SAFETY: self.process is a live process handle owned by this struct.
        let _ = unsafe { TerminateProcess(raw(&self.process), 1) };
    }
}

/// Spawn `exe` with `args` at Medium integrity, stdin and stdout piped.
///
/// Fails with [`ErrorCode::HelperSpawnFailed`] if `explorer.exe` is not
/// running — there is then no shell token to borrow, and silently falling back
/// to a High-IL child would produce a drag that no browser can complete.
pub fn spawn_medium_il(exe: &Path, args: &[String]) -> Result<MediumIlChild, ProtoError> {
    // SAFETY: GetShellWindow takes no arguments and is always callable.
    let pid = shell_pid(unsafe { GetShellWindow() })?;

    // SAFETY: pid comes from the window manager; OpenProcess validates it.
    let shell = own(
        unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) }
            .map_err(|e| win_err("OpenProcess(explorer.exe)", &e))?,
    );

    let mut shell_token = HANDLE::default();
    // SAFETY: `shell` is live for the call; `shell_token` is a live local.
    unsafe { OpenProcessToken(raw(&shell), TOKEN_DUPLICATE, &mut shell_token) }
        .map_err(|e| win_err("OpenProcessToken(explorer.exe)", &e))?;
    let shell_token = own(shell_token);

    // CreateProcessWithTokenW documents TOKEN_QUERY, TOKEN_DUPLICATE and
    // TOKEN_ASSIGN_PRIMARY as the required rights on the primary token.
    let mut primary = HANDLE::default();
    // SAFETY: `shell_token` is live for the call; `primary` is a live local.
    unsafe {
        DuplicateTokenEx(
            raw(&shell_token),
            TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    }
    .map_err(|e| win_err("DuplicateTokenEx", &e))?;
    let primary = own(primary);

    // Both pipes are created inheritable. Inheritance is not actually what
    // moves them: CreateProcessWithTokenW creates the process through the
    // Secondary Logon service, which is not the child's parent, so it
    // duplicates the STARTUPINFOW standard handles out of *our* process by
    // value instead. That is why our copies of the child's ends must still be
    // open when the create call runs, and are closed only afterwards.
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let (child_stdin, our_stdin) = anon_pipe(&sa)?;
    let (our_stdout, child_stdout) = anon_pipe(&sa)?;

    let app: Vec<u16> = wide(exe.as_os_str().to_string_lossy().as_ref());
    let mut cmdline = wide(&command_line(exe, args));

    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: raw(&child_stdin),
        hStdOutput: raw(&child_stdout),
        // Deliberately null: the helper's protocol is newline-delimited JSON
        // on stdout, and folding stderr into it would corrupt that stream.
        hStdError: HANDLE::default(),
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    // SAFETY: every pointer argument is a live local; `cmdline` is a mutable
    // buffer as the API requires (it writes a NUL into it).
    let created = unsafe {
        CreateProcessWithTokenW(
            raw(&primary),
            Default::default(),
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    };
    drop(child_stdin);
    drop(child_stdout);
    created.map_err(|e| win_err("CreateProcessWithTokenW", &e))?;

    // SAFETY: CreateProcessWithTokenW returned both handles to us.
    unsafe { CloseHandle(pi.hThread) }.map_err(|e| win_err("CloseHandle(hThread)", &e))?;

    Ok(MediumIlChild {
        process: own(pi.hProcess),
        pid: pi.dwProcessId,
        stdin: File::from(our_stdin),
        stdout: File::from(our_stdout),
    })
}

/// Process id behind the shell window.
///
/// Split out from [`spawn_medium_il`] so the "shell is not running" branch is
/// reachable in a test: `GetShellWindow` cannot be made to return null in
/// process.
fn shell_pid(shell_window: HWND) -> Result<u32, ProtoError> {
    let no_shell = || {
        ProtoError::new(
            ErrorCode::HelperSpawnFailed,
            "cannot de-elevate the drag helper: the Windows shell (explorer.exe) is not running, \
             so there is no medium-integrity token to borrow"
                .to_string(),
        )
    };
    if shell_window.is_invalid() {
        return Err(no_shell());
    }
    let mut pid = 0u32;
    // SAFETY: `shell_window` is non-null and `pid` is a live local.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
            shell_window,
            Some(&mut pid),
        )
    };
    if pid == 0 {
        return Err(no_shell());
    }
    Ok(pid)
}

/// Quote `exe` and append `args`, so a path containing a space cannot be
/// reparsed into a different executable.
fn command_line(exe: &Path, args: &[String]) -> String {
    let mut s = format!("\"{}\"", exe.display());
    for a in args {
        s.push(' ');
        s.push_str(a);
    }
    s
}

fn anon_pipe(sa: &SECURITY_ATTRIBUTES) -> Result<(OwnedHandle, OwnedHandle), ProtoError> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: all three pointers are live locals.
    unsafe { CreatePipe(&mut read, &mut write, Some(sa), 0) }
        .map_err(|e| win_err("CreatePipe", &e))?;
    Ok((own(read), own(write)))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win_err(what: &str, e: &windows_core::Error) -> ProtoError {
    ProtoError::new(ErrorCode::HelperSpawnFailed, format!("{what} failed: {e}"))
}

/// Take ownership of a Win32 handle so every early return closes it.
///
/// Every call site has just checked the producing Win32 call for failure, so
/// `h` is non-null and needs nothing but `CloseHandle` — which is exactly what
/// `OwnedHandle` requires.
fn own(h: HANDLE) -> OwnedHandle {
    // SAFETY: the caller has just received this handle from a Win32 call that
    // reported success, and does not use it again.
    unsafe { OwnedHandle::from_raw_handle(h.0 as _) }
}

fn raw(h: &OwnedHandle) -> HANDLE {
    HANDLE(h.as_raw_handle() as _)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_shell_window_is_a_spawn_failure_not_a_fallback() {
        let err = shell_pid(HWND(std::ptr::null_mut())).unwrap_err();
        assert_eq!(err.code, ErrorCode::HelperSpawnFailed);
        assert!(
            err.message.contains("explorer.exe"),
            "message must name the shell: {}",
            err.message
        );
    }

    #[test]
    fn the_real_shell_window_resolves_to_a_pid() {
        // SAFETY: GetShellWindow takes no arguments.
        let hwnd = unsafe { GetShellWindow() };
        if hwnd.is_invalid() {
            return; // no shell on this machine; the negative test covers it
        }
        assert!(shell_pid(hwnd).unwrap() != 0);
    }

    #[test]
    fn the_exe_path_is_quoted_so_a_space_cannot_split_it() {
        let cmd = command_line(
            Path::new(r"C:\Program Files\fastuse\fastuse-daemon.exe"),
            &["--drag-helper".to_string()],
        );
        assert_eq!(
            cmd,
            "\"C:\\Program Files\\fastuse\\fastuse-daemon.exe\" --drag-helper"
        );
    }
}
