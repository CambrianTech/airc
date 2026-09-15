//! Wait for the process that acknowledged stop, without terminating it. Windows
//! must release the mapped executable before installer/task maintenance begins.
#[cfg(windows)]
pub struct DaemonExit(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
}

#[cfg(windows)]
impl DaemonExit {
    pub fn capture(pid_file: &std::path::Path) -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        let pid: u32 = std::fs::read_to_string(pid_file)?
            .trim()
            .parse()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if pid == 0 || pid == std::process::id() {
            return Err(std::io::Error::other(
                "invalid daemon PID; refusing update stop",
            ));
        }
        // SAFETY: OpenProcess only obtains a non-inheritable wait handle. No
        // terminate/write rights are requested. OwnedHandle closes it on drop.
        let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: OpenProcess returned a non-null owned handle, transferred
        // exactly once to OwnedHandle so it is closed when this guard drops.
        Ok(Self(unsafe {
            std::os::windows::io::OwnedHandle::from_raw_handle(handle)
        }))
    }

    pub fn wait(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        let milliseconds = u32::try_from(timeout.as_millis())
            .map_err(|_| std::io::Error::other("daemon shutdown timeout is too large"))?;
        // SAFETY: this owned process handle stays open for the entire wait.
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), milliseconds) } {
            0 => Ok(()),
            0x102 => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon acknowledged stop but its process has not exited; update not installed",
            )),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}
