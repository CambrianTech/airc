//! Identity persistence — load + save a `PeerKeypair` to a file.
//!
//! MVP storage: raw 32-byte Ed25519 secret in a file at `--identity-file
//! <path>`. The file is created with 0600 (owner-only) permissions
//! when the runtime is on Unix.
//!
//! NOT for production secrets: a real implementation belongs behind
//! SQLCipher, an OS keychain, or a hardware enclave (see the substrate
//! `feedback_blobs_never_in_db` rule + the storage discussion in
//! `airc-protocol::keypair`'s docs). The CLI's MVP file-storage is
//! adequate for cross-process e2e demos but flagged as such.

use std::path::Path;

use airc_protocol::PeerKeypair;

pub fn load_or_generate(path: &Path) -> std::io::Result<PeerKeypair> {
    if path.exists() {
        load(path)
    } else {
        let keypair = PeerKeypair::generate();
        save(path, &keypair)?;
        Ok(keypair)
    }
}

pub fn load(path: &Path) -> std::io::Result<PeerKeypair> {
    let bytes = std::fs::read(path)?;
    if bytes.len() != 32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "identity file {} has {} bytes, expected 32 (raw Ed25519 secret)",
                path.display(),
                bytes.len()
            ),
        ));
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&bytes);
    Ok(PeerKeypair::from_secret_bytes(&secret))
}

pub fn save(path: &Path, keypair: &PeerKeypair) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, keypair.secret_bytes())?;
    set_owner_only_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> std::io::Result<()> {
    // Windows: rely on default user-profile ACLs for the directory.
    // A real cross-platform implementation would set Windows ACLs to
    // restrict to current user only — out of scope for MVP.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_through_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.key");
        let original = PeerKeypair::generate();
        save(&path, &original).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.secret_bytes(), original.secret_bytes());
    }

    #[test]
    fn load_or_generate_creates_then_reuses() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auto.key");
        let first = load_or_generate(&path).unwrap();
        let second = load_or_generate(&path).unwrap();
        // The second call must read what the first wrote, not
        // generate a fresh key — otherwise a CLI rerun would
        // change identity.
        assert_eq!(first.secret_bytes(), second.secret_bytes());
    }

    #[test]
    fn rejects_wrong_size_identity_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.key");
        std::fs::write(&path, [0u8; 20]).unwrap();
        let result = load(&path);
        assert!(result.is_err());
    }
}
