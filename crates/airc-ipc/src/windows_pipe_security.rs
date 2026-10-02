//! Local IPC belongs to the launching Windows account, not its token's default
//! owner (which may be Administrators). Never lower the object's integrity
//! level to let an unelevated client control an elevated daemon. Setup must
//! launch the daemon with the user's limited interactive token.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct LocalAllocation(*mut std::ffi::c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: this owns a non-null allocation returned by a LocalAlloc API.
        unsafe { LocalFree(self.0) };
    }
}

fn current_user_sid() -> io::Result<String> {
    let mut raw_token = ptr::null_mut();
    // SAFETY: the pseudo process handle is valid and raw_token is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken transferred an owned valid handle on success.
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    let mut bytes = 0;
    // SAFETY: a null buffer/zero length is the documented size query.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut bytes,
        )
    };
    if bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    // Word alignment accommodates TOKEN_USER and the following SID storage.
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    // SAFETY: buffer is aligned, writable and at least bytes long; token is live.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful TokenUser query initialized this aligned TOKEN_USER.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = ptr::null_mut();
    // SAFETY: SID points inside the still-live token information buffer.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(text.cast());
    let mut len = 0;
    // SAFETY: the API returned a null-terminated UTF-16 string, owned above.
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(
            text, len,
        )))
    }
}

pub(crate) fn create(name: &str, first_instance: bool) -> io::Result<NamedPipeServer> {
    let sid = current_user_sid()?;
    // Only this account and SYSTEM. No Everyone/Anonymous/other logged-in users.
    // Deliberately no mandatory-label override: Windows keeps no-write-up.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})\0")
        .encode_utf16()
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: sddl is null terminated; the API initializes descriptor on success.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: both attributes and its descriptor stay live throughout creation.
    // CreateNamedPipe copies the descriptor; the resulting handle owns no pointer.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    #[tokio::test]
    async fn kernel_pipe_dacl_contains_only_system_and_current_user() {
        let name = format!(r"\\.\pipe\airc-security-test-{}", uuid::Uuid::new_v4());
        let server = create(&name, true).unwrap();
        let mut descriptor = ptr::null_mut();
        // SAFETY: server owns the live kernel handle; all optional outputs are
        // null except descriptor, which receives an allocated security record.
        let result = unsafe {
            GetSecurityInfo(
                server.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(result, 0);
        let _descriptor = LocalAllocation(descriptor);
        let mut text = ptr::null_mut();
        let mut count = 0;
        // SAFETY: descriptor remains valid, and text/count are writable outputs.
        assert_ne!(
            // SAFETY: descriptor remains live and both output pointers are valid.
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut count,
                )
            },
            0
        );
        let _text = LocalAllocation(text.cast());
        // SAFETY: count includes the terminator in the API-owned UTF-16 buffer.
        let actual = unsafe {
            String::from_utf16_lossy(std::slice::from_raw_parts(text, count as usize - 1))
        };
        // The kernel maps generic-all into the pipe's file-all rights. The
        // returned string length can include extra null termination storage.
        let actual = actual.trim_end_matches('\0');
        let expected = format!("D:P(A;;FA;;;SY)(A;;FA;;;{})", current_user_sid().unwrap());
        assert_eq!(
            actual, expected,
            "read back actual kernel ACL, not a fixture string"
        );
    }
}
