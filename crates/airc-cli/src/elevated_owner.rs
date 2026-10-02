//! Installer-only recovery of a same-account elevated daemon, bound to one pipe.
use airc_ipc::{
    codec::{read_frame, write_frame},
    transport::IpcStream,
    Request, Response,
};
use std::{ffi::c_void, path::Path, time::Duration};
type Error = Box<dyn std::error::Error>;
type Handle = *mut c_void;
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        buffer: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn LocalFree(memory: Handle) -> Handle;
    fn GetCurrentProcess() -> Handle;
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        buffer: Handle,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: Handle, value: *mut *mut u16) -> i32;
}
struct Owned(Handle);
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: exclusively owned successful process/token handle, closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn checked(value: i32) -> Result<(), Error> {
    if value == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}
fn token(process: Handle) -> Result<Owned, Error> {
    let mut handle = std::ptr::null_mut();
    // SAFETY: live process handle or current pseudo-handle; output is writable.
    unsafe {
        checked(OpenProcessToken(process, 8, &mut handle))?;
    }
    Ok(Owned(handle))
}
fn information(token: &Owned, class: u32) -> Result<Vec<usize>, Error> {
    let mut size = 0;
    // SAFETY: documented null/zero buffer requests only the required size.
    unsafe {
        GetTokenInformation(token.0, class, std::ptr::null_mut(), 0, &mut size);
    }
    if size == 0 || size > 65536 {
        return Err("Token information unavailable".into());
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: aligned owned buffer has at least size bytes; token remains live.
    unsafe {
        checked(GetTokenInformation(
            token.0,
            class,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        ))?;
    }
    Ok(buffer)
}
fn sid(token: &Owned, class: u32) -> Result<String, Error> {
    let data = information(token, class)?;
    let mut text = std::ptr::null_mut();
    // SAFETY: User/Integrity SID points into the live token buffer. Windows
    // allocates the terminated string; LocalFree releases it after copying.
    unsafe {
        checked(ConvertSidToStringSidW(data[0] as Handle, &mut text))?;
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(result?)
    }
}
fn eligible(
    caller: &str,
    observer: &str,
    owner: &str,
    elevated: bool,
    integrity: &str,
) -> Result<(), Error> {
    if caller != observer || caller != owner {
        return Err("Recovery refuses a different or unknown account SID".into());
    }
    let level = integrity
        .strip_prefix("S-1-16-")
        .and_then(|v| v.parse::<u32>().ok());
    if !elevated || level != Some(12288) {
        return Err("Recovery requires an observed elevated high-integrity owner".into());
    }
    Ok(())
}
fn owned_image(image: &Path, installed: &Path) -> Result<(), Error> {
    let installed = installed.canonicalize()?;
    let image = image.canonicalize()?;
    let parent = installed
        .parent()
        .ok_or("Installed binary has no directory")?;
    let name = image
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Unknown image filename")?;
    // Historical updater (before cc5aadb) used airc_exe.with_file_name(format!("airc.old-{before}")).
    let legacy = name.strip_prefix("airc.old-").is_some_and(|sha| {
        (7..=40).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
    });
    let transaction = image
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(".airc-update-"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    if image == installed
        || (image.parent() == Some(parent) && legacy)
        || (name == "airc.exe"
            && transaction
            && image.parent().and_then(Path::parent) == Some(parent))
    {
        Ok(())
    } else {
        Err("Pipe owner image is outside the installed AIRC binary/backup paths; publish the candidate through public setup before explicit recovery (a build-target binary is not the installed destination)".into())
    }
}
async fn verify_image_build(socket: &Path, pid: u32) -> Result<(), Error> {
    // Keep the original Stop connection held. Status uses a second connection
    // whose native server PID must match that held owner, never a PID file.
    let mut status = IpcStream::connect(socket).await?;
    if status.server_process_id()? != pid {
        return Err("Status endpoint changed ownership".into());
    }
    write_frame(&mut status, &Request::Status).await?;
    let response: Option<Response> = read_frame(&mut status).await?;
    let Some(Response::Status(status)) = response else {
        return Err("Bound owner did not return canonical AIRC status".into());
    };
    if status.ipc_protocol_version != Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION)) {
        return Err("Owner protocol is unknown or incompatible".into());
    }
    let _sha = status
        .build_commit
        .filter(|sha| (7..=40).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Owner build revision is unknown")?;
    // Historical `before` was the checkout revision, not the displaced binary's
    // build. A stale installed daemon can legitimately have a different SHA.
    // Identity/provenance are established from the held pipe, process token and
    // installed directory above, never by equating these unrelated revisions.
    Ok(())
}
pub(crate) async fn recover(socket: &Path, caller: &str, installed: &Path) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(15),async {
        let mut pipe=IpcStream::connect(socket).await?;
        let pid=pipe.server_process_id()?;
        // SAFETY: scalar PID is obtained from the held native pipe; no user pointers are passed.
        let process=unsafe {OpenProcess(0x1000|0x100000,0,pid)};
        if process.is_null(){return Err(std::io::Error::last_os_error().into());}
        let process=Owned(process);
        let owner=token(process.0)?;
        // SAFETY: GetCurrentProcess returns the documented pseudo-handle and takes no pointers.
        let observer=token(unsafe {GetCurrentProcess()})?;
        eligible(caller,&sid(&observer,1)?,&sid(&owner,1)?,information(&owner,20)?[0]&0xffffffff!=0,&sid(&owner,25)?)?;
        let mut image=vec![0u16;32768]; let mut size=image.len() as u32;
        // SAFETY: image buffer is writable for size UTF-16 units and process handle remains owned.
        unsafe {checked(QueryFullProcessImageNameW(process.0,0,image.as_mut_ptr(),&mut size))?;}
        let image=std::path::PathBuf::from(String::from_utf16(&image[..size as usize])?);
        owned_image(&image,installed)?;
        verify_image_build(socket,pid).await?;
        // SAFETY: process owns a live query/synchronize handle for this endpoint owner.
        if pipe.server_process_id()?!=pid || unsafe {WaitForSingleObject(process.0,0)}!=258 {return Err("Bound daemon changed or exited before recovery".into());}
        let owner = token(process.0)?;
        eligible(caller,&sid(&observer,1)?,&sid(&owner,1)?,information(&owner,20)?[0]&0xffffffff!=0,&sid(&owner,25)?)?;
        write_frame(&mut pipe,&Request::Stop).await?;
        let response:Option<Response>=read_frame(&mut pipe).await?;
        if !matches!(response,Some(Response::Ok)){return Err("Bound daemon did not acknowledge graceful Stop".into());}
        loop {
            // SAFETY: process handle is held throughout the bounded asynchronous wait.
            match unsafe {WaitForSingleObject(process.0,0)} {
                0=>break,
                258=>tokio::time::sleep(Duration::from_millis(50)).await,
                _=>return Err(std::io::Error::last_os_error().into()),
            }
        }
        println!("Stopped same-account elevated AIRC endpoint owner {pid}; normal-token adoption may proceed");
        Ok::<(),Error>(())
    }).await.map_err(|_|"Elevated owner recovery timed out; no force termination was attempted")?
}
pub(crate) async fn access_denied(socket: &Path) -> Result<bool, Error> {
    match tokio::time::timeout(Duration::from_secs(5), IpcStream::connect(socket)).await? {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn native_observer_token_and_install_provenance() {
        // SAFETY: no-argument current-process pseudo-handle API.
        let token = super::token(unsafe { super::GetCurrentProcess() }).unwrap();
        assert!(super::sid(&token, 1).unwrap().starts_with("S-1-"));
        assert!(super::sid(&token, 25).unwrap().starts_with("S-1-16-"));
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("airc.exe");
        std::fs::write(&installed, b"fixture").unwrap();
        for (name, allowed) in [
            ("airc.old-8eaa592", true),
            ("airc.old-unknown", false),
            ("other-airc.exe", false),
        ] {
            let image = temp.path().join(name);
            std::fs::write(&image, b"fixture").unwrap();
            assert_eq!(super::owned_image(&image, &installed).is_ok(), allowed);
        }
        let foreign = tempfile::tempdir().unwrap();
        let image = foreign.path().join("airc.old-8eaa592");
        std::fs::write(&image, b"fixture").unwrap();
        assert!(super::owned_image(&image, &installed).is_err());
    }
    #[test]
    fn refuses_ambiguous_identity_and_tokens() {
        assert!(super::eligible(
            "S-1-5-21-1",
            "S-1-5-21-1",
            "S-1-5-21-1",
            true,
            "S-1-16-12288"
        )
        .is_ok());
        for (observer, owner, elevated, integrity) in [
            ("other", "S-1-5-21-1", true, "S-1-16-12288"),
            ("S-1-5-21-1", "other", true, "S-1-16-12288"),
            ("S-1-5-21-1", "S-1-5-21-1", false, "S-1-16-12288"),
            ("S-1-5-21-1", "S-1-5-21-1", true, "UNKNOWN"),
        ] {
            assert!(super::eligible("S-1-5-21-1", observer, owner, elevated, integrity).is_err());
        }
    }
}
