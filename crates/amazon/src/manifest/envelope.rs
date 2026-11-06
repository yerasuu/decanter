// SPDX-License-Identifier: GPL-3.0-or-later
//! The length-prefixed `ManifestHeader`, the body's decompression and its signature: what turns
//! the downloaded bytes into a body that Amazon signed.
//!
//! Tags (nile's descriptor): `ManifestHeader { CompressionSettings compression = 1; Hash hash = 2;
//! Signature signature = 3 }`, `CompressionSettings { CompressionAlgorithm algorithm = 1 }` with
//! `none = 0, lzma = 1`, and `Signature { SignatureAlgorithm algorithm = 1; bytes value = 2 }` with
//! `sha256_with_rsa = 0`. The header's `hash` is skipped: nile never reads it and what it covers is
//! unknown, while the signature covers the whole decompressed body.

use std::io::Read as _;

use lzma_rust2::{LzmaReader, XzReader};
use rsa::RsaPublicKey;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::{DecodePublicKey as _, Document};
use rsa::signature::Verifier as _;
use sha2::{Digest as _, Sha256};

use super::wire::Fields;
use super::{MAX_HEADER_LEN, MAX_MANIFEST_LEN, ManifestError};

/// The key Amazon signs manifests with: a 2048-bit RSA public key, copied from `AMZ_RSA_KEY` in
/// nile 1.2.0's `nile/models/manifest.py` (a protocol fact, not nile's code). Amazon publishes no
/// key of its own, so this one is as good as nile's reading of the launcher.
const AMAZON_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA6fSRMUi3VpTtv9P4+KvM
AcAIP4SYbTQfB1ns7vyUjsj8nrF2lGNtQTtGLnrNmM2ElZ2R7VmQtNiRtPMxToIW
Rajin0H0OyzGrHA8P6w96Mj4q1JeORCzJeVFgLOBClCCMmB+5bJBWnJcq/sEMwu9
gGynCeiYNLt7ZMVpL1GOsNjl+yLk7OMMGpMj1JWCVFfgYE9Lud1QZJllFAWhRBoT
wTctAUZTikObFUoBm+KEiCsKIcay4WOybvJwxTNBUl2GL8c+ihrT2ntLPpb9aIJE
/gXU3Ihl5oXe/0P/QN0CRu/ybXWLiGzIYqKIok4nepkdo8V3gWR55K801pOuck0B
awIDAQAB
-----END PUBLIC KEY-----";

/// SHA-256 of [`AMAZON_KEY_PEM`]'s DER (`SubjectPublicKeyInfo`), checked before every use so that
/// an edit of the key that does not also change this pin fails loudly instead of trusting a new key.
const AMAZON_KEY_SHA256: [u8; 32] = [
    0xde, 0x41, 0xd1, 0xea, 0x6d, 0x6f, 0x2f, 0x54, 0x2a, 0xda, 0xf5, 0x12, 0xf2, 0x02, 0x82, 0x41, 0x2e, 0xd3, 0x44,
    0x86, 0x8f, 0x7c, 0x61, 0xe1, 0x74, 0xe5, 0xa8, 0x3b, 0x90, 0x1a, 0x0c, 0x43,
];

/// The xz container's magic. Python's `lzma.decompress`, which nile calls with its default
/// `FORMAT_AUTO`, takes either an xz stream or a legacy `.lzma` one, so both are read here.
const XZ_MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0x00];

/// What the LZMA decoder may reserve for its dictionary, in kibibytes. A declared dictionary above it is
/// refused before anything is allocated; under it, the window grows only with the decoded output,
/// which [`MAX_MANIFEST_LEN`] caps. 256 mebibytes is past xz's and 7-Zip's largest presets (64).
const MAX_DICTIONARY_KIB: u32 = 256 << 10;

struct Header<'msg> {
    compression: u64,
    signature: Option<SignatureField<'msg>>,
}

struct SignatureField<'msg> {
    algorithm: u64,
    value: &'msg [u8],
}

/// The decompressed body of `raw`, returned only once its signature holds, so that no byte of an
/// unsigned body reaches the protobuf reader.
pub(super) fn open(raw: &[u8]) -> Result<Vec<u8>, ManifestError> {
    let (len, rest) = raw
        .split_first_chunk::<4>()
        .ok_or(ManifestError::Malformed("shorter than its header length"))?;
    #[expect(
        clippy::big_endian_bytes,
        reason = "the header's length prefix is big-endian on the wire"
    )]
    let header_len = usize::try_from(u32::from_be_bytes(*len)).unwrap_or(usize::MAX);
    if header_len > MAX_HEADER_LEN {
        return Err(ManifestError::TooLarge {
            what: "header",
            limit: MAX_HEADER_LEN,
        });
    }
    let (header_bytes, compressed) = rest
        .split_at_checked(header_len)
        .ok_or(ManifestError::Malformed("header runs past the end of the download"))?;
    let header = read_header(header_bytes)?;
    let signature = header.signature.ok_or(ManifestError::Unsigned)?;
    let body = decompress(header.compression, compressed)?;
    verify(&signature, &body)?;
    Ok(body)
}

fn read_header(bytes: &[u8]) -> Result<Header<'_>, ManifestError> {
    let mut header = Header {
        compression: 0,
        signature: None,
    };
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        match number {
            1 => header.compression = read_compression(value.bytes("compression settings")?)?,
            3 => header.signature = Some(read_signature(value.bytes("signature")?)?),
            _ => {}
        }
    }
    Ok(header)
}

/// proto2 gives an absent enum its first value, so a header without compression settings means `none`.
fn read_compression(bytes: &[u8]) -> Result<u64, ManifestError> {
    let mut algorithm = 0;
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        if number == 1 {
            algorithm = value.varint("compression algorithm")?;
        }
    }
    Ok(algorithm)
}

fn read_signature(bytes: &[u8]) -> Result<SignatureField<'_>, ManifestError> {
    let mut signature = SignatureField {
        algorithm: 0,
        value: &[],
    };
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        match number {
            1 => signature.algorithm = value.varint("signature algorithm")?,
            2 => signature.value = value.bytes("signature value")?,
            _ => {}
        }
    }
    Ok(signature)
}

fn decompress(algorithm: u64, compressed: &[u8]) -> Result<Vec<u8>, ManifestError> {
    match algorithm {
        0 if compressed.len() > MAX_MANIFEST_LEN => Err(too_large()),
        0 => Ok(compressed.to_vec()),
        1 if compressed.starts_with(&XZ_MAGIC) => {
            read_capped(XzReader::new_mem_limit(compressed, true, MAX_DICTIONARY_KIB))
        }
        1 => {
            let mut reader = LzmaReader::new_mem_limit(compressed, MAX_DICTIONARY_KIB, None)
                .map_err(ManifestError::Decompression)?;
            // The default stops at the declared size without checking the stream ends there;
            // liblzma, which nile goes through, refuses such a stream.
            reader.set_relaxed_end_condition(false);
            read_capped(reader)
        }
        other => Err(ManifestError::UnknownCompression(other)),
    }
}

/// Reads at most one byte past [`MAX_MANIFEST_LEN`], so that a body that inflates further is
/// refused after that much work rather than decoded to the end.
fn read_capped(reader: impl std::io::Read) -> Result<Vec<u8>, ManifestError> {
    let mut body = Vec::new();
    reader
        .take(u64::try_from(MAX_MANIFEST_LEN).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut body)
        .map_err(ManifestError::Decompression)?;
    if body.len() > MAX_MANIFEST_LEN {
        return Err(too_large());
    }
    Ok(body)
}

const fn too_large() -> ManifestError {
    ManifestError::TooLarge {
        what: "decompressed body",
        limit: MAX_MANIFEST_LEN,
    }
}

/// RSA PKCS#1 v1.5 with SHA-256 over the decompressed body, as nile checks it.
fn verify(signature: &SignatureField<'_>, body: &[u8]) -> Result<(), ManifestError> {
    if signature.algorithm != 0 {
        return Err(ManifestError::UnknownSignature(signature.algorithm));
    }
    let (_, der) = Document::from_pem(AMAZON_KEY_PEM).map_err(|err| ManifestError::Key(err.to_string()))?;
    if Sha256::digest(der.as_bytes()).as_slice() != AMAZON_KEY_SHA256 {
        return Err(ManifestError::Key(
            "the key no longer matches its pinned SHA-256".to_owned(),
        ));
    }
    let key = RsaPublicKey::from_public_key_der(der.as_bytes()).map_err(|err| ManifestError::Key(err.to_string()))?;
    let value = Signature::try_from(signature.value).map_err(|_malformed| ManifestError::BadSignature)?;
    VerifyingKey::<rsa::sha2::Sha256>::new(key)
        .verify(body, &value)
        .map_err(|_mismatch| ManifestError::BadSignature)
}
