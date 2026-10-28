//! Random tokens, the hashes they are stored as, and the comparison that does not leak how much matched.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub type Hash = [u8; 32];

/// 128 random bits as 32 lowercase hex characters.
///
/// # Errors
///
/// The operating system's random source is unavailable.
pub fn random_hex() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(hex(&bytes))
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[must_use]
pub fn sha256(value: &[u8]) -> Hash {
    Sha256::digest(value).into()
}

/// Hashing first makes both sides the same length, so the comparison time says nothing about the secret.
#[must_use]
pub fn matches(secret_hash: &Hash, candidate: &[u8]) -> bool {
    secret_hash.ct_eq(&sha256(candidate)).into()
}

/// `matches` for a hash stored as hex; a malformed stored value matches nothing.
#[must_use]
pub fn matches_hex(secret_hash_hex: &str, candidate: &[u8]) -> bool {
    let mut hash = [0_u8; 32];
    let valid = secret_hash_hex.len() == 64
        && hash.iter_mut().enumerate().all(|(i, byte)| {
            secret_hash_hex
                .get(i * 2..i * 2 + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .is_some_and(|value| {
                    *byte = value;
                    true
                })
        });
    let matched = matches(&hash, candidate);
    valid && matched
}

/// An id or fingerprint as it appears in a URL: lowercase hex of the exact length, nothing else.
#[must_use]
pub fn is_hex_token(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_tokens_are_32_hex_and_differ() {
        let (a, b) = (random_hex().unwrap_or_default(), random_hex().unwrap_or_default());
        assert!(is_hex_token(&a, 32));
        assert_ne!(a, b);
    }

    #[test]
    fn matches_only_the_exact_secret() {
        let stored = sha256(b"code");
        assert!(matches(&stored, b"code"));
        assert!(!matches(&stored, b"code "));
        assert!(!matches(&stored, b""));
    }

    #[test]
    fn hex_token_rejects_uppercase_and_wrong_length() {
        assert!(!is_hex_token("ABCDEF0123456789", 16));
        assert!(!is_hex_token("abcdef012345678", 16));
        assert!(is_hex_token("abcdef0123456789", 16));
    }
}
