//! Bearer tokens (256-bit CSPRNG, only SHA-256 hashes at rest), QR nonces and PINs.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 32 random bytes, base64url without padding: 43 characters (Python's `token_urlsafe(32)`).
pub fn mint_token() -> String {
    let mut raw = [0u8; 32];
    OsRng.fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

/// Hex SHA-256 of a token: the only form a token is stored in.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Uniform `000000`..`999999` (rejection sampling, no modulo bias).
pub fn generate_pin() -> String {
    const LIMIT: u32 = u32::MAX - (u32::MAX % 1_000_000);
    loop {
        let n = OsRng.next_u32();
        if n < LIMIT {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = mint_token();
        assert_eq!(t.len(), 43);
        assert!(t
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(t, mint_token());
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(ct_eq(&hash_token(&t), &hash_token(&t)));
        assert!(!ct_eq("a", "ab"));
        let pin = generate_pin();
        assert!(pin.len() == 6 && pin.bytes().all(|b| b.is_ascii_digit()));
    }
}
