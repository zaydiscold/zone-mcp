//! What a flash carries (PROTOCOL.md, "Carrier 2: Flash"): the JSON
//! `{"t":"flash","v":1,"phone":{...},"items":[...]}`, compressed with raw DEFLATE (RFC 1951,
//! no zlib or gzip header), then sealed with ChaCha20-Poly1305 under the flash key as
//! `nonce(12) || ciphertext || tag(16)`.

use anyhow::{Result, anyhow, bail};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde_json::{Value, json};

use crate::db::PhoneInfo;
use crate::item::Item;

pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
/// The most JSON a flash may inflate to. A real library is a few megabytes. This stops a
/// crafted payload from inflating without end.
pub const MAX_JSON_LEN: usize = 256 * 1024 * 1024;

/// A flash, opened and read.
pub struct Flash {
    pub phone: PhoneInfo,
    pub items: Vec<Item>,
    /// Items without an id, kind or time: they cannot be stored.
    pub rejected: usize,
    /// The items checked against the phone's own summary of them, when it sent one.
    pub check: super::check::Check,
}

fn cipher(key: &[u8; 32]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(key.into())
}

/// Compresses and seals `json` with a nonce the caller chose (random, outside of tests).
pub fn seal_with_nonce(key: &[u8; 32], nonce: &[u8; NONCE_LEN], json: &[u8]) -> Result<Vec<u8>> {
    let packed = miniz_oxide::deflate::compress_to_vec(json, 9);
    let sealed = cipher(key).encrypt(&Nonce::from(*nonce), packed.as_slice()).map_err(|_| anyhow!("the flash could not be sealed"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Compresses and seals `json` with a random nonce.
pub fn seal(key: &[u8; 32], json: &[u8]) -> Result<Vec<u8>> {
    seal_with_nonce(key, &crate::paths::random_bytes::<NONCE_LEN>(), json)
}

/// The compressed bytes inside, when `key` is the key the flash was sealed with. None for any
/// other key and for a payload that was damaged: the two cannot be told apart.
pub fn unseal(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let (nonce, rest) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    cipher(key).decrypt(&Nonce::from(nonce), rest).ok()
}

/// Undoes the raw DEFLATE.
pub fn inflate(packed: &[u8]) -> Result<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_with_limit(packed, MAX_JSON_LEN)
        .map_err(|_| anyhow!("the flash opened with the right key but its contents are not compressed the way the protocol says"))
}

/// The JSON of a flash, as the phone writes it.
pub fn flash_json(phone: &PhoneInfo, items: &[Value]) -> Vec<u8> {
    json!({
        "t": "flash",
        "v": 1,
        "phone": { "id": phone.id, "name": phone.name, "app": phone.app },
        "items": items,
    })
    .to_string()
    .into_bytes()
}

/// Reads the JSON of a flash. Errors never include any item's text.
pub fn parse(json: &[u8]) -> Result<Flash> {
    let value: Value = serde_json::from_slice(json).map_err(|_| anyhow!("the flash is not JSON"))?;
    if value.get("t").and_then(Value::as_str) != Some("flash") {
        bail!("the payload is not a flash");
    }
    match value.get("v").and_then(Value::as_i64) {
        Some(1) => {}
        Some(other) => bail!("the phone speaks flash version {other} and this zone knows version 1. Update zone."),
        None => bail!("the flash does not say which version it is"),
    }
    let phone_value = value.get("phone").cloned().unwrap_or(Value::Null);
    let text = |key: &str| phone_value.get(key).and_then(Value::as_str).map(|s| s.chars().take(200).collect::<String>());
    let phone = PhoneInfo { id: text("id"), name: text("name"), app: text("app") };
    let entries = value.get("items").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut items = Vec::new();
    let mut rejected = 0;
    for entry in entries {
        match Item::from_json(entry) {
            Ok(item) => items.push(item),
            Err(_) => rejected += 1,
        }
    }
    let check = super::check::examine(&value, entries);
    Ok(Flash { phone, items, rejected, check })
}

/// The flash key as bytes, from the 64 hex characters it is stored as.
pub fn key_bytes(key_hex: &str) -> Option<[u8; 32]> {
    crate::paths::unhex(key_hex.trim())?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::hex;

    fn test_key() -> [u8; 32] {
        key_bytes(&crate::flash::flash_key(&"ab".repeat(32)).unwrap()).unwrap()
    }

    #[test]
    fn the_cipher_matches_rfc_8439() {
        // The key, nonce and text of RFC 8439 section 2.8.2, without its associated data.
        let key: [u8; 32] = (0x80..0xa0).collect::<Vec<u8>>().try_into().unwrap();
        let nonce: [u8; 12] = [0x07, 0, 0, 0, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47];
        let plain = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let sealed = cipher(&key).encrypt(&Nonce::from(nonce), plain.as_slice()).unwrap();
        // The ciphertext does not depend on the associated data, so it is the RFC's.
        assert_eq!(hex(&sealed[..16]), "d31a8d34648e60db7b86afbc53ef7ec2");
        assert_eq!(sealed.len(), plain.len() + TAG_LEN);
    }

    #[test]
    fn seals_and_opens() {
        let key = test_key();
        assert_eq!(hex(&key), "a00f0027d0be63dc4d0e64abf643d94be5f261dc0e4502d7028dc0e2c20f56fc");
        let items = crate::fakephone::synthetic_items(0, 60);
        let phone = PhoneInfo { id: Some("PHONE-1".into()), name: Some("Test phone".into()), app: Some("1.0.3 (1)".into()) };
        let json = flash_json(&phone, &items);
        let sealed = seal(&key, &json).unwrap();
        assert!(sealed.len() < json.len() / 2, "compressed: {} of {}", sealed.len(), json.len());

        let packed = unseal(&key, &sealed).unwrap();
        assert_eq!(inflate(&packed).unwrap(), json);
        let flash = parse(&json).unwrap();
        assert_eq!((flash.items.len(), flash.rejected), (60, 0));
        assert_eq!(flash.phone.id.as_deref(), Some("PHONE-1"));

        // Another key, a flipped bit and a cut payload all fail the same way.
        let mut other = key;
        other[0] ^= 1;
        assert!(unseal(&other, &sealed).is_none());
        let mut damaged = sealed.clone();
        damaged[40] ^= 0x10;
        assert!(unseal(&key, &damaged).is_none());
        assert!(unseal(&key, &sealed[..sealed.len() - 1]).is_none());
        assert!(unseal(&key, &sealed[..20]).is_none());
    }

    #[test]
    fn a_fixed_vector_other_implementations_can_check() {
        // key = flash key of the secret 0xab x32, nonce = 00 01 .. 0b, JSON as below. The
        // deflate bytes differ between compressors, so the vector is checked by opening.
        let key = test_key();
        let nonce: [u8; 12] = (0u8..12).collect::<Vec<u8>>().try_into().unwrap();
        let json = br#"{"t":"flash","v":1,"phone":{"id":"P","name":"n","app":"a"},"items":[]}"#;
        let sealed = seal_with_nonce(&key, &nonce, json).unwrap();
        assert_eq!(&sealed[..12], &nonce);
        assert_eq!(inflate(&unseal(&key, &sealed).unwrap()).unwrap(), json);
        // A stored (uncompressed) deflate block, as any compressor may emit, also opens.
        let mut stored = vec![0x01, json.len() as u8, 0, !(json.len() as u8), 0xff];
        stored.extend_from_slice(json);
        assert_eq!(inflate(&stored).unwrap(), json);
    }

    #[test]
    fn reads_what_it_can_and_counts_the_rest() {
        let json = br#"{"t":"flash","v":1,"phone":{"id":"P"},"future":true,"items":[
            {"id":"A","kind":"note","created_at":1,"updated_at":1,"text":"x"},
            {"kind":"note","text":"no id"}, 7,
            {"id":"B","kind":"dictation","deleted":true,"updated_at":2}]}"#;
        let flash = parse(json).unwrap();
        assert_eq!((flash.items.len(), flash.rejected), (2, 2));
        assert!(flash.items[1].deleted);
        assert!(flash.phone.name.is_none());

        assert!(parse(b"not json").is_err());
        assert!(parse(br#"{"t":"hello","v":1}"#).is_err());
        let newer = parse(br#"{"t":"flash","v":2,"items":[]}"#).err().unwrap().to_string();
        assert!(newer.contains("version 2"));
        // Nothing but zero bytes is not a deflate stream that ends.
        assert!(inflate(&[0xff; 40]).is_err());
    }
}
