//! At-rest credential encryption, wire-compatible with upstream `atRestCrypto.js`.
//! Envelope: JSON string `{"v":1,"iv":b64,"tag":b64,"data":b64}`, AES-256-GCM, key `PRO_CREDENTIAL_KEY`.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

pub const SENSITIVE_FIELDS: &[&str] =
    &["apiKey", "accessToken", "refreshToken", "idToken", "secret", "clientSecret", "token"];

pub fn key() -> Option<[u8; 32]> {
    let raw = std::env::var("PRO_CREDENTIAL_KEY").ok()?;
    let raw = raw.trim();
    if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).ok()?;
        }
        return Some(out);
    }
    B64.decode(raw).ok()?.try_into().ok()
}

fn cipher(k: &[u8; 32]) -> Option<LessSafeKey> {
    Some(LessSafeKey::new(UnboundKey::new(&AES_256_GCM, k).ok()?))
}

pub fn is_envelope(s: &str) -> bool {
    s.starts_with("enc:v1:")
        || serde_json::from_str::<Value>(s).is_ok_and(|p| {
            p["v"] == 1 && p["iv"].is_string() && p["tag"].is_string() && p["data"].is_string()
        })
}

pub fn encrypt_with(k: &[u8; 32], plain: &str) -> Option<String> {
    let mut iv = [0u8; 12];
    SystemRandom::new().fill(&mut iv).ok()?;
    let mut buf = plain.as_bytes().to_vec();
    cipher(k)?
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut buf)
        .ok()?;
    let tag = buf.split_off(buf.len() - 16);
    Some(json!({"v":1,"iv":B64.encode(iv),"tag":B64.encode(tag),"data":B64.encode(buf)}).to_string())
}

/// Returns the input unchanged when it is not an envelope, the key is absent, or auth fails.
pub fn decrypt_with(k: &[u8; 32], value: &str) -> Option<String> {
    let (iv, tag, data) = if let Some(rest) = value.strip_prefix("enc:v1:") {
        // Legacy format written by earlier Rust builds: enc:v1:iv:tag:data
        let mut p = rest.split(':');
        (p.next()?.to_owned(), p.next()?.to_owned(), p.next()?.to_owned())
    } else {
        let p: Value = serde_json::from_str(value).ok()?;
        (p["iv"].as_str()?.to_owned(), p["tag"].as_str()?.to_owned(), p["data"].as_str()?.to_owned())
    };
    let iv: [u8; 12] = B64.decode(iv).ok()?.try_into().ok()?;
    let mut buf = B64.decode(data).ok()?;
    buf.extend(B64.decode(tag).ok()?);
    let out = cipher(k)?
        .open_in_place(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut buf)
        .ok()?;
    String::from_utf8(out.to_vec()).ok()
}

fn map_fields(data: &mut Value, f: &dyn Fn(&str) -> Option<String>) {
    let walk = |o: &mut serde_json::Map<String, Value>| {
        for name in SENSITIVE_FIELDS {
            if let Some(Value::String(s)) = o.get_mut(*name)
                && let Some(n) = f(s)
            {
                *s = n;
            }
        }
    };
    let Some(o) = data.as_object_mut() else { return };
    walk(o);
    if let Some(Value::Object(spec)) = o.get_mut("providerSpecificData") {
        walk(spec);
    }
}

/// Decrypts sensitive fields in a `providerConnections.data` object in place.
pub fn decrypt_fields(data: &mut Value) {
    if let Some(k) = key() {
        map_fields(data, &|s| decrypt_with(&k, s));
    }
}

/// Encrypts plaintext sensitive fields in place when a key is configured. Returns fields changed.
pub fn encrypt_fields(data: &mut Value) -> usize {
    let Some(k) = key() else { return 0 };
    let n = std::cell::Cell::new(0);
    map_fields(data, &|s| {
        (!s.is_empty() && !is_envelope(s)).then(|| encrypt_with(&k, s)).flatten().inspect(|_| n.set(n.get() + 1))
    });
    n.get()
}

/// Parses a `data` column and decrypts it. Use at every site that consumes secrets.
pub fn parse_data(raw: &str) -> Option<Value> {
    let mut v: Value = serde_json::from_str(raw).ok()?;
    decrypt_fields(&mut v);
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_node_envelope() {
        let k = [7u8; 32];
        let enc = encrypt_with(&k, "sk-secret").unwrap();
        assert!(is_envelope(&enc));
        assert_eq!(decrypt_with(&k, &enc).as_deref(), Some("sk-secret"));
        assert_eq!(decrypt_with(&[8u8; 32], &enc), None);
        // Envelope produced by Node atRestCrypto.encryptValue with key 0x07*32.
        let node = r#"{"v":1,"iv":"AAAAAAAAAAAAAAAA","tag":"5RjfOSyQbDk7Slard/ALWA==","data":"ErOJ3JT2bg=="}"#;
        assert!(is_envelope(node));
        assert_eq!(decrypt_with(&k, node).as_deref(), Some("sk-node"));
        assert!(!is_envelope("sk-plain"));
    }
}
