// SPDX-License-Identifier: GPL-3.0-or-later
//! PKCE with the S256 method ([RFC 7636, section 4](https://www.rfc-editor.org/rfc/rfc7636#section-4)):
//! the verifier stays here until the registration sends it, the sign-in page only ever sees its
//! hash.

use decanter_core::secrets::Secret;
use sha2::{Digest as _, Sha256};

use crate::error::CallError;

/// 32 bytes encode to the 43 characters RFC 7636 sets as the verifier's minimum, which is what
/// nile sends too (analysis, section 2.1).
const VERIFIER_BYTES: usize = 32;

/// A fresh verifier from the operating system's CSPRNG.
pub(crate) fn verifier() -> Result<Secret, CallError> {
    let mut bytes = [0_u8; VERIFIER_BYTES];
    getrandom::fill(&mut bytes).map_err(|err| CallError::Random(err.to_string()))?;
    Ok(Secret::from(base64url(&bytes)))
}

/// `BASE64URL(SHA256(ASCII(verifier)))`, RFC 7636 section 4.2.
pub(crate) fn challenge(verifier: &Secret) -> String {
    base64url(&Sha256::digest(verifier.expose().as_bytes()))
}

/// Base64url without padding (RFC 7636, appendix A). Small enough to hand-roll rather than take a
/// dependency for two calls.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let symbol = |bits: u32| {
        let index = usize::try_from(bits & 0x3f_u32).unwrap_or_default();
        char::from(ALPHABET.get(index).copied().unwrap_or(b'A'))
    };
    let mut text = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for group in bytes.chunks(3) {
        let (first, second, third) = match *group {
            [first, second, third] => (first, Some(second), Some(third)),
            [first, second] => (first, Some(second), None),
            [first] => (first, None, None),
            _ => continue,
        };
        let bits = (u32::from(first) << 16_u32)
            | (u32::from(second.unwrap_or(0_u8)) << 8_u32)
            | u32::from(third.unwrap_or(0_u8));
        text.push(symbol(bits >> 18_u32));
        text.push(symbol(bits >> 12_u32));
        if second.is_some() {
            text.push(symbol(bits >> 6_u32));
        }
        if third.is_some() {
            text.push(symbol(bits));
        }
    }
    text
}
