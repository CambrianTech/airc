//! Windows: stdout and stderr become the pipe's write end by `SetStdHandle` (Rust's stdio reads
//! the standard handle on every write), and are let go by clearing them and closing that end.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Pipes::CreatePipe;

/// This process's stdout and stderr, now the pipe; `write` is its end until released.
pub(super) struct Redirected {
    write: Option<OwnedHandle>,
}

pub(super) fn redirect() -> io::Result<(File, Redirected)> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-params are valid; null security attributes make neither end inheritable.
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe returned two open handles owned by nothing else.
    let (read, write) = unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    };
    for id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `write` is an open handle that outlives the redirection (held until release).
        if unsafe { SetStdHandle(id, write.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((File::from(read), Redirected { write: Some(write) }))
}

impl Redirected {
    /// Clear stdout and stderr and close this process's write end, so the drain reads to the end.
    pub(super) fn release(&mut self) {
        for id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: a null standard handle is valid; writes to it fail instead of reaching the pipe.
            unsafe { SetStdHandle(id, std::ptr::null_mut()) };
        }
        self.write.take();
    }
}
