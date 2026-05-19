use std::error::Error;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{Signer, SigningKey};
use rand_core::{OsRng, RngCore};
use serde_json::Value;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

const X25519_PRIV: &str = "x25519_priv";
const X25519_PUB: &str = "x25519_pub";
const ED25519_PRIV: &str = "private.pem";
const ED25519_PUB: &str = "public.pem";

pub fn bootstrap_x25519(identity_dir: &Path) -> Result<String, Box<dyn Error>> {
    let (priv_path, pub_path) = x25519_paths(identity_dir);
    if priv_path.exists() && pub_path.exists() {
        return Ok(b64_url_no_pad(&read_exact_32(
            &pub_path,
            "X25519 public key",
        )?));
    }

    let secret = StaticSecret::random_from_rng(OsRng);
    let public = X25519PublicKey::from(&secret);
    atomic_write(&priv_path, secret.to_bytes().as_slice(), private_mode())?;
    atomic_write(&pub_path, public.as_bytes(), public_mode())?;
    Ok(b64_url_no_pad(public.as_bytes()))
}

pub fn bootstrap_ed25519(identity_dir: &Path) -> Result<(), Box<dyn Error>> {
    let priv_path = identity_dir.join(ED25519_PRIV);
    let pub_path = identity_dir.join(ED25519_PUB);
    if priv_path.exists() && pub_path.exists() {
        return Ok(());
    }

    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let signing = SigningKey::from_bytes(&seed);
    let priv_pem = signing.to_pkcs8_pem(Default::default())?;
    let pub_pem = signing
        .verifying_key()
        .to_public_key_pem(Default::default())?;
    atomic_write(&priv_path, priv_pem.as_bytes(), private_mode())?;
    atomic_write(&pub_path, pub_pem.as_bytes(), public_mode())?;
    Ok(())
}

pub fn peer_pub(peers_dir: &Path, peer_name: &str) -> Result<Option<String>, Box<dyn Error>> {
    let path = peers_dir.join(format!("{peer_name}.json"));
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value: Value = serde_json::from_str(&raw)?;
    let Some(encoded) = value.get(X25519_PUB).and_then(Value::as_str) else {
        return Ok(None);
    };
    let decoded = b64_url_decode(encoded)?;
    if decoded.len() != 32 {
        return Ok(None);
    }
    Ok(Some(b64_url_no_pad(&decoded)))
}

pub fn peer_x25519_public_raw(
    peers_dir: &Path,
    peer_name: &str,
) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
    let Some(encoded) = peer_pub(peers_dir, peer_name)? else {
        return Ok(None);
    };
    let decoded = b64_url_decode(&encoded)?;
    let pubkey: [u8; 32] = decoded.try_into().map_err(|data: Vec<u8>| {
        format!(
            "peer X25519 public key for {peer_name} is {} bytes; expected 32",
            data.len()
        )
    })?;
    Ok(Some(pubkey))
}

pub fn sign_ed25519_stdin(identity_dir: &Path) -> Result<String, Box<dyn Error>> {
    let mut data = Vec::new();
    io::stdin().read_to_end(&mut data)?;
    let pem = fs::read_to_string(identity_dir.join(ED25519_PRIV))?;
    let signing = SigningKey::from_pkcs8_pem(&pem)?;
    let signature = signing.sign(&data);
    Ok(general_purpose::STANDARD.encode(signature.to_bytes()))
}

pub fn load_x25519_private(identity_dir: &Path) -> Result<[u8; 32], Box<dyn Error>> {
    read_exact_32(&identity_dir.join(X25519_PRIV), "X25519 private key")
}

fn x25519_paths(identity_dir: &Path) -> (PathBuf, PathBuf) {
    (
        identity_dir.join(X25519_PRIV),
        identity_dir.join(X25519_PUB),
    )
}

fn read_exact_32(path: &Path, label: &str) -> Result<[u8; 32], Box<dyn Error>> {
    let raw = fs::read(path)?;
    let bytes: [u8; 32] = raw.try_into().map_err(|data: Vec<u8>| {
        format!(
            "{label} at {} is {} bytes; expected 32",
            path.display(),
            data.len()
        )
    })?;
    Ok(bytes)
}

fn b64_url_no_pad(bytes: &[u8]) -> String {
    general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64_url_decode(input: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(general_purpose::URL_SAFE_NO_PAD.decode(input)?)
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    set_permissions(&tmp, mode)?;
    fs::rename(tmp, path)
}

#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

const fn private_mode() -> u32 {
    0o600
}

const fn public_mode() -> u32 {
    0o644
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25519_bootstrap_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let first = bootstrap_x25519(dir.path()).unwrap();
        let second = bootstrap_x25519(dir.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(b64_url_decode(&first).unwrap().len(), 32);
    }

    #[test]
    fn peer_pub_reads_legacy_peer_record() {
        let dir = tempfile::tempdir().unwrap();
        let encoded = b64_url_no_pad(&[7u8; 32]);
        fs::write(
            dir.path().join("alice.json"),
            format!(r#"{{"x25519_pub":"{encoded}"}}"#),
        )
        .unwrap();

        assert_eq!(peer_pub(dir.path(), "alice").unwrap(), Some(encoded));
        assert_eq!(peer_pub(dir.path(), "bob").unwrap(), None);
    }
}
