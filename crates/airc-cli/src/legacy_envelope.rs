use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Read};
use std::path::Path;

use base64::{engine::general_purpose, Engine};
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::ChaCha20Poly1305;
use hkdf::Hkdf;
use serde_json::Value;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::legacy_identity;

const ENC_VERSION: &str = "v1";
const AEAD_INFO: &[u8] = b"airc-aead-v1";

pub fn wrap_stdin(recipient_pub: &str, identity_dir: &Path) -> Result<(), Box<dyn Error>> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let Ok(mut envelope) = serde_json::from_str::<Value>(trimmed) else {
        println!("{trimmed}");
        return Ok(());
    };
    if recipient_pub.is_empty() {
        println!("{}", serde_json::to_string(&envelope)?);
        return Ok(());
    }

    let sender_priv = legacy_identity::load_x25519_private(identity_dir)?;
    let recipient_pub = legacy_identity::b64_url_decode(recipient_pub)?;
    let recipient_pub: [u8; 32] = recipient_pub.try_into().map_err(|data: Vec<u8>| {
        format!(
            "recipient X25519 public key is {} bytes; expected 32",
            data.len()
        )
    })?;
    wrap_value(&mut envelope, sender_priv, recipient_pub)?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

fn wrap_value(
    envelope: &mut Value,
    sender_priv: [u8; 32],
    recipient_pub: [u8; 32],
) -> Result<(), Box<dyn Error>> {
    let msg = envelope.get("msg").and_then(Value::as_str).map_or_else(
        || {
            envelope
                .get("msg")
                .map_or_else(String::new, Value::to_string)
        },
        str::to_owned,
    );
    let ad = associated_data(envelope)?;
    let key = derive_pairwise_key(sender_priv, recipient_pub)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)?;
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            chacha20poly1305::aead::Payload {
                msg: msg.as_bytes(),
                aad: ad.as_bytes(),
            },
        )
        .map_err(|_| "legacy envelope encryption failed")?;

    let object = envelope
        .as_object_mut()
        .ok_or("legacy envelope must be a JSON object")?;
    object.insert(
        "msg".to_string(),
        Value::String(b64_url_no_pad(&ciphertext)),
    );
    object.insert("nonce".to_string(), Value::String(b64_url_no_pad(&nonce)));
    object.insert("enc".to_string(), Value::String(ENC_VERSION.to_string()));
    Ok(())
}

fn derive_pairwise_key(
    sender_priv: [u8; 32],
    recipient_pub: [u8; 32],
) -> Result<[u8; 32], Box<dyn Error>> {
    let secret = StaticSecret::from(sender_priv);
    let public = PublicKey::from(recipient_pub);
    let shared = secret.diffie_hellman(&public);
    let hkdf = Hkdf::<Sha256>::new(None, shared.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(AEAD_INFO, &mut key)
        .map_err(|_| "HKDF output length is invalid")?;
    Ok(key)
}

fn associated_data(envelope: &Value) -> Result<String, Box<dyn Error>> {
    let mut fields = BTreeMap::new();
    fields.insert("channel", string_field(envelope, "channel"));
    fields.insert("from", string_field(envelope, "from"));
    let kind = envelope
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .to_string();
    fields.insert("kind", kind);
    fields.insert("to", string_field(envelope, "to"));
    fields.insert("ts", string_field(envelope, "ts"));
    Ok(serde_json::to_string(&fields)?)
}

fn string_field(envelope: &Value, key: &str) -> String {
    envelope
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn b64_url_no_pad(bytes: &[u8]) -> String {
    general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn associated_data_matches_legacy_sorted_json() {
        let envelope = json!({
            "from": "alice",
            "to": "bob",
            "ts": "2026-05-19T00:00:00Z",
            "channel": "general",
            "kind": "heartbeat",
            "msg": "hello"
        });

        assert_eq!(
            associated_data(&envelope).unwrap(),
            r#"{"channel":"general","from":"alice","kind":"heartbeat","to":"bob","ts":"2026-05-19T00:00:00Z"}"#
        );
    }
}
