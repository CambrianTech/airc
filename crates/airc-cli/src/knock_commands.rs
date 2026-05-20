use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Command;

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::ChaCha20Poly1305;
use hkdf::Hkdf;
use rand_core::RngCore;
use serde_json::{json, Value};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

const HKDF_INFO: &[u8] = b"airc-knock-approve-v1";
const JSON_FENCE_START: &str = "```json";
const JSON_FENCE_END: &str = "```";

pub fn run_gen_keys() -> Result<(), Box<dyn Error>> {
    let secret = random_secret();
    let public = PublicKey::from(&secret);
    println!(
        "{}",
        serde_json::to_string(&json!({
            "priv": hex_encode(&secret.to_bytes()),
            "pub": hex_encode(public.as_bytes()),
        }))?
    );
    Ok(())
}

pub fn run_encrypt_for_knocker(knocker_pub: &str, plaintext: &str) -> Result<(), Box<dyn Error>> {
    let knocker_pub = hex32("--knocker-pub", knocker_pub)?;
    let approver_priv = random_secret();
    let approver_pub = PublicKey::from(&approver_priv);
    let key = derive_pairwise_key(approver_priv.to_bytes(), knocker_pub)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)?;
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| "airc knock encrypt: AEAD encryption failed")?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "ver": "v1",
            "approver_pub": hex_encode(approver_pub.as_bytes()),
            "nonce": hex_encode(&nonce),
            "ciphertext": hex_encode(&ciphertext),
        }))?
    );
    Ok(())
}

pub fn run_decrypt_from_approver(
    knocker_priv: &str,
    approver_pub: &str,
    nonce: &str,
    ciphertext: &str,
) -> Result<(), Box<dyn Error>> {
    let knocker_priv = hex32("--knocker-priv", knocker_priv)?;
    let approver_pub = hex32("--approver-pub", approver_pub)?;
    let nonce = hex_exact::<12>("--nonce", nonce)?;
    let ciphertext = hex_decode("--ciphertext", ciphertext)?;
    let key = derive_pairwise_key(knocker_priv, approver_pub)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)?;
    let plaintext = cipher
        .decrypt(&nonce.into(), ciphertext.as_slice())
        .map_err(|_| "airc knock decrypt: AEAD authentication failed")?;
    io::stdout().write_all(&plaintext)?;
    if !plaintext.ends_with(b"\n") {
        io::stdout().write_all(b"\n")?;
    }
    Ok(())
}

pub fn run_approval_field(field: &str) -> Result<(), Box<dyn Error>> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let value: Value = serde_json::from_str(&raw)?;
    if let Some(value) = value.get(field).and_then(Value::as_str) {
        println!("{value}");
    }
    Ok(())
}

pub fn run_identity_json(name: &str, state_dir: &Path) -> Result<(), Box<dyn Error>> {
    let identity = load_knock_identity(state_dir, name);
    println!("{}", serde_json::to_string(&identity)?);
    Ok(())
}

pub fn run_extract_knocker_pub() -> Result<(), Box<dyn Error>> {
    let mut markdown = String::new();
    io::stdin().read_to_string(&mut markdown)?;
    if let Some(pubkey) = extract_knocker_pub(&markdown) {
        println!("{pubkey}");
    }
    Ok(())
}

pub fn run_extract_approval() -> Result<(), Box<dyn Error>> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    if let Some(approval) = extract_latest_approval(&raw)? {
        println!("{}", serde_json::to_string(&approval)?);
    }
    Ok(())
}

fn random_secret() -> StaticSecret {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    StaticSecret::from(bytes)
}

fn derive_pairwise_key(my_priv: [u8; 32], their_pub: [u8; 32]) -> Result<[u8; 32], Box<dyn Error>> {
    let secret = StaticSecret::from(my_priv);
    let public = PublicKey::from(their_pub);
    let shared = secret.diffie_hellman(&public);
    let hkdf = Hkdf::<Sha256>::new(None, shared.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(HKDF_INFO, &mut key)
        .map_err(|_| "HKDF output length is invalid")?;
    Ok(key)
}

fn hex32(name: &str, value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = hex_exact::<32>(name, value)?;
    Ok(bytes)
}

fn hex_exact<const N: usize>(name: &str, value: &str) -> Result<[u8; N], Box<dyn Error>> {
    let bytes = hex_decode(name, value)?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!("{name} must decode to {N} bytes, got {}", bytes.len()).into()
    })
}

fn hex_decode(name: &str, value: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let value = value.trim();
    if !value.len().is_multiple_of(2) {
        return Err(format!("{name}: hex input has odd length").into());
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]).ok_or_else(|| format!("{name}: not valid hex"))?;
        let low = hex_nibble(pair[1]).ok_or_else(|| format!("{name}: not valid hex"))?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(CHARS[(byte >> 4) as usize] as char);
        out.push(CHARS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn extract_knocker_pub(markdown: &str) -> Option<String> {
    json_fences(markdown)
        .filter_map(|block| serde_json::from_str::<Value>(block).ok())
        .find_map(|value| {
            value
                .get("knocker_pub")
                .and_then(Value::as_str)
                .filter(|pubkey| !pubkey.is_empty())
                .map(str::to_owned)
        })
}

fn load_knock_identity(state_dir: &Path, name: &str) -> Value {
    let identity_path = state_dir.join("identity.json");
    let identity = fs::read_to_string(identity_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);

    json!({
        "name": name,
        "pronouns": string_field(&identity, "pronouns").unwrap_or_default(),
        "role": string_field(&identity, "role").unwrap_or_default(),
        "bio": string_field(&identity, "bio").unwrap_or_default(),
        "gh_login": string_field(&identity, "gh_login")
            .or_else(query_gh_login)
            .unwrap_or_default(),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn query_gh_login() -> Option<String> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let login = String::from_utf8(output.stdout).ok()?;
    let login = login.trim();
    if login.is_empty() {
        None
    } else {
        Some(login.to_owned())
    }
}

fn extract_latest_approval(raw_comments_json: &str) -> Result<Option<Value>, Box<dyn Error>> {
    let value: Value = serde_json::from_str(raw_comments_json)?;
    let mut latest = None;
    if let Some(comments) = value.get("comments").and_then(Value::as_array) {
        for body in comments
            .iter()
            .filter_map(|comment| comment.get("body").and_then(Value::as_str))
        {
            for block in json_fences(body) {
                let Ok(candidate) = serde_json::from_str::<Value>(block) else {
                    continue;
                };
                if is_approval_envelope(&candidate) {
                    latest = Some(candidate);
                }
            }
        }
    }
    Ok(latest)
}

fn is_approval_envelope(value: &Value) -> bool {
    value.get("ver").and_then(Value::as_str) == Some("v1")
        && value.get("approver_pub").and_then(Value::as_str).is_some()
        && value.get("nonce").and_then(Value::as_str).is_some()
        && value.get("ciphertext").and_then(Value::as_str).is_some()
}

fn json_fences(markdown: &str) -> impl Iterator<Item = &str> {
    markdown.split(JSON_FENCE_START).skip(1).filter_map(|tail| {
        let content = tail.strip_prefix('\n').unwrap_or(tail);
        content
            .split_once(JSON_FENCE_END)
            .map(|(block, _)| block.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_are_hex_32_byte_pair() {
        let secret = random_secret();
        let public = PublicKey::from(&secret);

        assert_eq!(
            hex_decode("priv", &hex_encode(&secret.to_bytes()))
                .unwrap()
                .len(),
            32
        );
        assert_eq!(
            hex_decode("pub", &hex_encode(public.as_bytes()))
                .unwrap()
                .len(),
            32
        );
    }

    #[test]
    fn ecdh_derives_same_key_for_both_parties() {
        let a = random_secret();
        let b = random_secret();
        let a_pub = PublicKey::from(&a);
        let b_pub = PublicKey::from(&b);

        assert_eq!(
            derive_pairwise_key(a.to_bytes(), *b_pub.as_bytes()).unwrap(),
            derive_pairwise_key(b.to_bytes(), *a_pub.as_bytes()).unwrap()
        );
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext() {
        let knocker = random_secret();
        let knocker_pub = PublicKey::from(&knocker);
        let approver = random_secret();
        let approver_pub = PublicKey::from(&approver);
        let key = derive_pairwise_key(approver.to_bytes(), *knocker_pub.as_bytes()).unwrap();
        let cipher = ChaCha20Poly1305::new_from_slice(&key).unwrap();
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut ciphertext = cipher.encrypt(&nonce, b"secret".as_slice()).unwrap();
        let last = ciphertext.last_mut().unwrap();
        *last ^= 0x01;

        let result = run_decrypt_from_approver(
            &hex_encode(&knocker.to_bytes()),
            &hex_encode(approver_pub.as_bytes()),
            &hex_encode(&nonce),
            &hex_encode(&ciphertext),
        );

        assert!(result.is_err());
    }

    #[test]
    fn extracts_knocker_pub_from_markdown_json_fence() {
        let markdown = r#"hello
```json
{"ver":"v1","knocker_pub":"abc123"}
```
"#;

        assert_eq!(extract_knocker_pub(markdown).as_deref(), Some("abc123"));
    }

    #[test]
    fn identity_json_reads_existing_fields_and_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("identity.json"),
            r#"{"pronouns":"they/them","role":"agent","bio":"works","gh_login":"octo"}"#,
        )
        .unwrap();

        let value = load_knock_identity(dir.path(), "clio");

        assert_eq!(value["name"], "clio");
        assert_eq!(value["pronouns"], "they/them");
        assert_eq!(value["role"], "agent");
        assert_eq!(value["bio"], "works");
        assert_eq!(value["gh_login"], "octo");
    }

    #[test]
    fn identity_json_defaults_when_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();

        let value = load_knock_identity(dir.path(), "fresh");

        assert_eq!(value["name"], "fresh");
        assert_eq!(value["pronouns"], "");
        assert_eq!(value["role"], "");
        assert_eq!(value["bio"], "");
    }

    #[test]
    fn extracts_latest_approval_from_comments_json() {
        let comments = json!({
            "comments": [
                {"body": "```json\n{\"ver\":\"v1\",\"approver_pub\":\"old\",\"nonce\":\"n1\",\"ciphertext\":\"c1\"}\n```"},
                {"body": "text\n```json\n{\"ver\":\"v1\",\"approver_pub\":\"new\",\"nonce\":\"n2\",\"ciphertext\":\"c2\"}\n```"}
            ]
        })
        .to_string();

        let approval = extract_latest_approval(&comments).unwrap().unwrap();

        assert_eq!(approval["approver_pub"], "new");
        assert_eq!(approval["nonce"], "n2");
        assert_eq!(approval["ciphertext"], "c2");
    }
}
