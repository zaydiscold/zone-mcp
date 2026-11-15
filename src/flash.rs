//! Carrier 2, Flash: the phone shows a rapid sequence of QR codes and the computer's camera
//! reads them. No network is involved (PROTOCOL.md, "Carrier 2: Flash").
//!
//! - [`fountain`]: the frames and the code that rebuilds the payload from any of them.
//! - [`seal`]: what is inside: ChaCha20-Poly1305 around raw DEFLATE around JSON.
//! - [`qr`]: reading QR codes out of camera pictures, and drawing them.
//! - [`receive`]: the daemon's side: frames in, items in the library out.
//! - [`dev`]: `zone dev flash-encode` and `flash-decode`, for testing without a phone.
//!
//! This file holds the flash key. PROTOCOL.md derives it from the pairing secret, on both
//! sides, at pairing: `HKDF-SHA256(ikm = pairing secret bytes, salt = empty, info =
//! "zone-flash/1")`, 32 bytes. The computer keeps only a hash of the secret, so the key is
//! derived while the secret is still in hand: when the offer is made, and again when a phone
//! presents it. It is stored beside the offer and the paired phone.
//!
//! The key opens flashes. It cannot be turned back into the pairing secret.

pub mod fountain;
pub mod qr;
pub mod seal;

use sha2::{Digest, Sha256};

const INFO: &[u8] = b"zone-flash/1";
const BLOCK: usize = 64;

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(Sha256::digest(key).as_slice());
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.as_slice());
    let mut out = [0u8; 32];
    out.copy_from_slice(outer.finalize().as_slice());
    out
}

/// HKDF-SHA256 (RFC 5869), for outputs up to 32 bytes, which is all Flash needs.
fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    // An empty salt means a salt of 32 zero bytes.
    let zeros = [0u8; 32];
    let prk = hmac_sha256(if salt.is_empty() { &zeros } else { salt }, ikm);
    let mut first = Vec::with_capacity(info.len() + 1);
    first.extend_from_slice(info);
    first.push(1);
    hmac_sha256(&prk, &first)
}

/// The flash key for a pairing secret (64 hex characters), as 64 hex characters.
pub fn flash_key(secret_hex: &str) -> Option<String> {
    let secret = crate::paths::unhex(secret_hex.trim()).filter(|bytes| bytes.len() == 32)?;
    Some(crate::paths::hex(&hkdf_sha256(&secret, &[], INFO)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{hex, unhex};

    #[test]
    fn hkdf_matches_rfc_5869() {
        // Test case 1: the first 32 bytes of the 42 byte output.
        let ikm = [0x0b; 22];
        let salt: Vec<u8> = (0x00..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        assert_eq!(hex(&hkdf_sha256(&ikm, &salt, &info)), "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf");
        // Test case 3: empty salt and empty info.
        assert_eq!(hex(&hkdf_sha256(&ikm, &[], &[])), "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d");
    }

    #[test]
    fn hmac_handles_long_keys() {
        // RFC 4231 test case 6: a 131 byte key is hashed first.
        let key = [0xaa; 131];
        let mac = hmac_sha256(&key, b"Test Using Larger Than Block-Size Key - Hash Key First");
        assert_eq!(hex(&mac), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    }

    #[test]
    fn flash_key_vectors() {
        // Computed independently with Python's hmac and hashlib. The phone must get the same.
        assert_eq!(flash_key(&"ab".repeat(32)).unwrap(), "a00f0027d0be63dc4d0e64abf643d94be5f261dc0e4502d7028dc0e2c20f56fc");
        let counting: Vec<u8> = (0u8..32).collect();
        assert_eq!(flash_key(&hex(&counting)).unwrap(), "93f8bee15ebbb0af8b305483390f6aaa884a149c5af6a2fa0b36d98885b879bf");
        assert_eq!(unhex(&flash_key(&"AB".repeat(32)).unwrap()).unwrap().len(), 32);
        assert!(flash_key("abcd").is_none());
        assert!(flash_key(&"zz".repeat(32)).is_none());
    }
}
