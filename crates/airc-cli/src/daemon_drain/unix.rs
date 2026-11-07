//! Unix: stdout and stderr become the pipe's write end by `dup2`, and are let go by pointing them
//! at `/dev/null`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};

/// This process's stdout and stderr, now the pipe.
pub(super) struct Redirected;

pub(super) fn redirect() -> io::Result<(File, Redirected)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (read, write) = (fds[0], fds[1]);
    // SAFETY: `read` is a descriptor this function just created; the read end is this process's
    // alone, so a child the daemon spawns never holds it.
    unsafe { libc::fcntl(read, libc::F_SETFD, libc::FD_CLOEXEC) };
    for target in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // SAFETY: `write` and `target` are open descriptors; dup2 replaces `target` atomically.
        if unsafe { libc::dup2(write, target) } < 0 {
            let error = io::Error::last_os_error();
            // SAFETY: both ends were created above and are owned by nothing else.
            unsafe {
                libc::close(write);
                libc::close(read);
            }
            return Err(error);
        }
    }
    // SAFETY: `write` is duplicated onto stdout and stderr; this copy is no longer needed.
    unsafe { libc::close(write) };
    // SAFETY: `read` is an open descriptor owned by nothing else; the File takes it.
    Ok((unsafe { File::from_raw_fd(read) }, Redirected))
}

impl Redirected {
    /// Point stdout and stderr at `/dev/null`, so this process no longer holds the pipe.
    pub(super) fn release(&mut self) {
        let Ok(null) = OpenOptions::new().write(true).open("/dev/null") else {
            return;
        };
        for target in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            // SAFETY: `null` and `target` are open descriptors; dup2 replaces `target`.
            unsafe { libc::dup2(null.as_raw_fd(), target) };
        }
    }
}
