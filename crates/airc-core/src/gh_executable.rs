//! Shared GitHub CLI discovery, including installations newer than the caller's PATH.
//! Selection never changes the process environment or runs an authentication probe.
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;

pub fn override_path() -> Option<PathBuf> {
    std::env::var_os("AIRC_GH_BIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// An explicit override remains authoritative even if unavailable. Let the caller
/// report its real spawn error instead of silently using another account's tool.
pub fn resolve() -> PathBuf {
    override_path().unwrap_or_else(|| {
        candidates()
            .into_iter()
            .find(|path| executable_file(path))
            // Preserve the historical OS-search failure path when nothing was
            // discovered. Only the explicit candidates exclude relative entries.
            .unwrap_or_else(|| PathBuf::from("gh"))
    })
}

fn executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn append_path(paths: &mut Vec<PathBuf>, value: &std::ffi::OsStr) {
    for directory in std::env::split_paths(value) {
        // A missing PATH entry must not turn into executable discovery in cwd.
        if !directory.as_os_str().is_empty() && directory.is_absolute() {
            paths.push(directory.join(if cfg!(windows) { "gh.exe" } else { "gh" }));
        }
    }
}

pub fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        append_path(&mut paths, &path);
    }
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            let root = Path::new(&root);
            if root.is_absolute() {
                paths.push(root.join("Programs/GitHub CLI/bin/gh.exe"));
                paths.push(root.join("Programs/GitHub CLI/gh.exe"));
            }
        }
        for path in windows::registered_paths() {
            append_path(&mut paths, &path);
        }
        // These are the layouts supported by our public installer and gh's MSI.
        for root in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(root) {
                let root = Path::new(&root);
                if root.is_absolute() {
                    paths.push(root.join("GitHub CLI/gh.exe"));
                }
            }
        }
    }
    #[cfg(not(windows))]
    paths.extend(["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"].map(PathBuf::from));
    paths
}

#[cfg(windows)]
mod windows {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: *mut c_void,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            bytes: *mut u32,
        ) -> i32;
    }
    pub(super) fn registered_paths() -> Vec<OsString> {
        [
            (-2147483647isize, "Environment"),
            (
                -2147483646isize,
                "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
            ),
        ]
        .into_iter()
        .filter_map(|(key, subkey)| read_path(key, subkey))
        .collect()
    }
    fn read_path(key: isize, subkey: &str) -> Option<OsString> {
        let key_name: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
        let value: Vec<u16> = "Path".encode_utf16().chain(Some(0)).collect();
        // One bounded buffer; RegGetValue expands REG_EXPAND_SZ without writing env.
        let mut data = vec![0u16; 32768];
        let mut bytes = (data.len() * 2) as u32;
        // SAFETY: predefined registry handle, terminated UTF-16 strings and writable
        // buffer of the advertised length. No opened handle needs to be closed.
        let status = unsafe {
            RegGetValueW(
                key as *mut c_void,
                key_name.as_ptr(),
                value.as_ptr(),
                0x00000002 | 0x00000004,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != 0 || bytes as usize > data.len() * 2 || !bytes.is_multiple_of(2) {
            return None;
        }
        let used = bytes as usize / 2;
        let end = data[..used].iter().position(|ch| *ch == 0).unwrap_or(used);
        Some(OsString::from_wide(&data[..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn discovery_skips_nonexecutable_file_before_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("airc-gh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let blocked = root.join("not executable");
        let usable = root.join("executable");
        std::fs::write(&blocked, "fixture").unwrap();
        std::fs::write(&usable, "fixture").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&usable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let selected = [blocked.clone(), usable.clone()]
            .into_iter()
            .find(|p| executable_file(p));
        std::fs::remove_file(blocked).unwrap();
        std::fs::remove_file(&usable).unwrap();
        std::fs::remove_dir(root).unwrap();
        assert_eq!(selected, Some(usable));
    }

    #[test]
    fn path_discovery_excludes_empty_relative_and_current_directory_entries() {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\Tools With Spaces")
        } else {
            PathBuf::from("/tools with spaces")
        };
        let path = std::env::join_paths([
            PathBuf::new(),
            PathBuf::from("."),
            PathBuf::from("relative"),
            root.clone(),
        ])
        .unwrap();
        let mut candidates = Vec::new();
        append_path(&mut candidates, &path);
        assert_eq!(
            candidates,
            [root.join(if cfg!(windows) { "gh.exe" } else { "gh" })]
        );
    }
}
