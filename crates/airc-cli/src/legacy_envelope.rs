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

    let mut envelope: Value = serde_json::from_str(trimmed)?;
    if recipient_pub.is_empty() {
        return Err("envelope wrap requires --recipient-pub".into());
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

pub fn unwrap_stdin(sender_pub: &str, identity_dir: &Path) -> Result<(), Box<dyn Error>> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let mut envelope: Value = serde_json::from_str(trimmed)?;
    if !is_encrypted(&envelope) {
        println!("{}", serde_json::to_string(&envelope)?);
        return Ok(());
    }
    if sender_pub.is_empty() {
        return Err("envelope unwrap requires --sender-pub for encrypted input".into());
    }

    let recipient_priv = legacy_identity::load_x25519_private(identity_dir)?;
    let sender_pub = legacy_identity::b64_url_decode(sender_pub)?;
    let sender_pub: [u8; 32] = sender_pub.try_into().map_err(|data: Vec<u8>| {
        format!(
            "sender X25519 public key is {} bytes; expected 32",
            data.len()
        )
    })?;
    unwrap_value(&mut envelope, recipient_priv, sender_pub)?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

pub fn unwrap_value(
    envelope: &mut Value,
    recipient_priv: [u8; 32],
    sender_pub: [u8; 32],
) -> Result<(), Box<dyn Error>> {
    let enc = envelope
        .get("enc")
        .and_then(Value::as_str)
        .ok_or("encrypted envelope has no enc field")?;
    if enc != ENC_VERSION {
        return Err(format!("unsupported envelope enc version: {enc}").into());
    }
    let ciphertext = envelope
        .get("msg")
        .and_then(Value::as_str)
        .ok_or("encrypted envelope msg must be a string")?;
    let nonce = envelope
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or("encrypted envelope missing nonce")?;
    let ciphertext = legacy_identity::b64_url_decode(ciphertext)?;
    let nonce = legacy_identity::b64_url_decode(nonce)?;
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|data: Vec<u8>| format!("envelope nonce is {} bytes; expected 12", data.len()))?;
    let ad = associated_data(envelope)?;
    let key = derive_pairwise_key(recipient_priv, sender_pub)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)?;
    let plaintext = cipher
        .decrypt(
            &nonce.into(),
            chacha20poly1305::aead::Payload {
                msg: ciphertext.as_slice(),
                aad: ad.as_bytes(),
            },
        )
        .map_err(|_| "envelope decryption failed")?;
    let msg = String::from_utf8(plaintext)?;

    let object = envelope
        .as_object_mut()
        .ok_or("envelope must be a JSON object")?;
    object.insert("msg".to_string(), Value::String(msg));
    object.remove("enc");
    object.remove("nonce");
    Ok(())
}

pub fn is_encrypted(envelope: &Value) -> bool {
    envelope.get("enc").and_then(Value::as_str).is_some()
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
        .map_err(|_| "envelope encryption failed")?;

    let object = envelope
        .as_object_mut()
        .ok_or("envelope must be a JSON object")?;
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

    #[test]
    fn wrap_then_unwrap_restores_plaintext() {
        let sender_priv = [1u8; 32];
        let recipient_priv = [2u8; 32];
        let recipient_pub = *PublicKey::from(&StaticSecret::from(recipient_priv)).as_bytes();
        let sender_pub = *PublicKey::from(&StaticSecret::from(sender_priv)).as_bytes();
        let mut envelope = json!({
            "from": "alice",
            "to": "bob",
            "ts": "2026-05-19T00:00:00Z",
            "channel": "general",
            "msg": "hello encrypted"
        });

        wrap_value(&mut envelope, sender_priv, recipient_pub).unwrap();
        assert!(is_encrypted(&envelope));
        unwrap_value(&mut envelope, recipient_priv, sender_pub).unwrap();

        assert_eq!(envelope["msg"], "hello encrypted");
        assert!(envelope.get("enc").is_none());
        assert!(envelope.get("nonce").is_none());
    }
}
