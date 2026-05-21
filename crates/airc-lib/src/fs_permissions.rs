use std::path::Path;

#[cfg(unix)]
pub(crate) fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub(crate) fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
