//! Build a Windows SECURITY_DESCRIPTOR with a DACL granting only the current
//! user SID `FILE_ALL_ACCESS`. Used for the named-pipe server.

use std::ffi::c_void;
use std::ptr::null_mut;

use windows::core::PWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, GENERIC_ALL, HANDLE,
};
use windows::Win32::Security::Authorization::{
    SetEntriesInAclW, EXPLICIT_ACCESS_W, SET_ACCESS, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows::Win32::Security::{
    ACE_FLAGS, GetTokenInformation, InitializeSecurityDescriptor, SetSecurityDescriptorDacl,
    TokenUser, ACL, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Owned Windows security descriptor + the DACL it points at + the SID buffer
/// it references. All three must outlive any HANDLE created with the SD.
pub struct OwnedSecurityDescriptor {
    sd: Box<SECURITY_DESCRIPTOR>,
    dacl: *mut ACL,
    _sid_buf: Vec<u8>,
    pub sa: SECURITY_ATTRIBUTES,
}

// SAFETY: SECURITY_ATTRIBUTES contains raw pointers but the OwnedSecurityDescriptor
// owns the buffers they reference; the struct is logically owned data.
unsafe impl Send for OwnedSecurityDescriptor {}

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.dacl.is_null() {
            // SAFETY: dacl was allocated by SetEntriesInAclW; LocalFree is the matching free.
            unsafe {
                let _ = windows::Win32::Foundation::LocalFree(Some(
                    windows::Win32::Foundation::HLOCAL(self.dacl as *mut _),
                ));
            }
        }
    }
}

/// Build a SECURITY_ATTRIBUTES granting only the current user SID full access.
pub fn current_user_only() -> std::io::Result<OwnedSecurityDescriptor> {
    // 1. Get current user SID into a heap buffer (kept alive by OwnedSecurityDescriptor).
    let sid_buf = current_user_sid_buf()?;
    // SAFETY: sid_buf holds a TOKEN_USER struct followed by the SID payload.
    // Vec<u8> is only byte-aligned, but TOKEN_USER contains pointers that
    // require natural alignment — use read_unaligned to avoid UB (CR-03).
    // The PSID pointer it returns refers into the same heap buffer, which
    // OwnedSecurityDescriptor keeps alive in `_sid_buf`.
    let sid_ptr = unsafe {
        let p = sid_buf.as_ptr() as *const TOKEN_USER;
        std::ptr::read_unaligned(p).User.Sid
    };

    // 2. Build EXPLICIT_ACCESS_W granting GENERIC_ALL to that SID.
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL.0,
        grfAccessMode: SET_ACCESS,
        grfInheritance: ACE_FLAGS(0), // NO_INHERITANCE
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: Default::default(),
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: PWSTR(sid_ptr.0 as *mut _),
        },
    };

    // 3. SetEntriesInAclW allocates a new ACL (must LocalFree on drop).
    let mut new_dacl: *mut ACL = null_mut();
    // SAFETY: ea is valid for the duration of the call; new_dacl receives a heap pointer.
    let rc = unsafe { SetEntriesInAclW(Some(&[ea]), None, &mut new_dacl) };
    if rc.0 != 0 {
        return Err(std::io::Error::from_raw_os_error(rc.0 as i32));
    }

    // 4. InitializeSecurityDescriptor on a heap-pinned struct.
    let mut sd: Box<SECURITY_DESCRIPTOR> = Box::new(unsafe { std::mem::zeroed() });
    let psd = PSECURITY_DESCRIPTOR(sd.as_mut() as *mut _ as *mut c_void);
    // SAFETY: sd is fresh zeroed memory; revision is the documented constant.
    let res = unsafe { InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) };
    if res.is_err() {
        let err = unsafe { GetLastError().0 };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }
    // SAFETY: psd points at our owned SD; new_dacl is the heap ACL we just built.
    let res = unsafe { SetSecurityDescriptorDacl(psd, true, Some(new_dacl), false) };
    if res.is_err() {
        let err = unsafe { GetLastError().0 };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }

    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_mut() as *mut _ as *mut c_void,
        bInheritHandle: false.into(),
    };

    Ok(OwnedSecurityDescriptor {
        sd,
        dacl: new_dacl,
        _sid_buf: sid_buf,
        sa,
    })
}

fn current_user_sid_buf() -> std::io::Result<Vec<u8>> {
    let mut token_handle = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle.
    let res = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) };
    if res.is_err() {
        let err = unsafe { GetLastError().0 };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }
    let mut needed: u32 = 0;
    let _ = unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut needed) };
    let last = unsafe { GetLastError() };
    if last != ERROR_INSUFFICIENT_BUFFER {
        unsafe {
            let _ = CloseHandle(token_handle);
        }
        return Err(std::io::Error::from_raw_os_error(last.0 as i32));
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: buf has at least `needed` bytes.
    let res = unsafe {
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
    };
    let err = if res.is_err() {
        Some(unsafe { GetLastError().0 })
    } else {
        None
    };
    unsafe {
        let _ = CloseHandle(token_handle);
    }
    if let Some(e) = err {
        return Err(std::io::Error::from_raw_os_error(e as i32));
    }
    Ok(buf)
}

/// Marker — silences `dacl` field "never read" warning while keeping it owned.
#[allow(dead_code)]
fn _ensure_dacl_kept(o: &OwnedSecurityDescriptor) -> *mut ACL {
    o.dacl
}

/// Marker — silences `sd` field "never read" warning while keeping it owned.
#[allow(dead_code)]
fn _ensure_sd_kept(o: &OwnedSecurityDescriptor) -> *const SECURITY_DESCRIPTOR {
    &*o.sd
}

/// Get the raw SECURITY_ATTRIBUTES pointer for tokio's
/// `create_with_security_attributes_raw`.
pub fn sa_ptr(o: &mut OwnedSecurityDescriptor) -> *mut c_void {
    &mut o.sa as *mut SECURITY_ATTRIBUTES as *mut c_void
}
