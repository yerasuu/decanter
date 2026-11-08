// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's file manifest (`manifest.proto` next to the download URL), read and verified (kb #1611,
//! stage S3 of docs/amazon-nile-integration-analysis.md, section 4.2).
//!
//! The layout is a big-endian `u32` header length, a `ManifestHeader` protobuf and the body, the
//! `Manifest` protobuf of package `tv.twitch.fuel.sds`, compressed as the header says. Tag numbers
//! and enum values are the ones of the descriptor nile embeds (`nile/proto/sds_proto2_pb2.py`,
//! nile 1.2.0), taken as facts; the reader is decanter's own. UNVERIFIED against a live manifest:
//! none has been read yet (kb #1519).
//!
//! Everything in the manifest is untrusted until the signature over the decompressed body checks
//! out, and the paths stay untrusted after it: a signed manifest can still name `..\..\x`, so each
//! one becomes a [`ManifestPath`] here or the whole manifest is refused.

mod body;
mod envelope;
mod plan;
mod wire;

use core::fmt;

pub use decanter_runtime::install::{ManifestPath, PathFault};
pub use plan::{Plan, plan};

/// The whole answer, so that a body which is not compressed cannot be larger than a decompressed one.
pub(crate) const MAX_RAW_LEN: usize = MAX_HEADER_LEN + MAX_MANIFEST_LEN + 4;
/// A header holds two enums, a hash and a 256-byte signature: a few hundred bytes.
const MAX_HEADER_LEN: usize = 64 << 10;
/// The decompressed body. A game of 100,000 files with 200-byte paths takes about 30 mebibytes.
const MAX_MANIFEST_LEN: usize = 64 << 20;
/// Past what any game ships; caps the memory a body of empty entries could cost in parsed records.
const MAX_FILES: usize = 500_000;
const MAX_DIRS: usize = 500_000;

/// A verified manifest of one package.
#[derive(Debug)]
pub struct Manifest {
    files: Vec<ManifestFile>,
    dirs: Vec<ManifestPath>,
    total_size: u64,
}

impl Manifest {
    #[inline]
    #[must_use]
    pub fn files(&self) -> &[ManifestFile] {
        &self.files
    }

    /// The directories the package lists, empty ones included; files' parents are not always among them.
    #[inline]
    #[must_use]
    pub fn dirs(&self) -> &[ManifestPath] {
        &self.dirs
    }

    /// The sum of every file's size, checked to fit a `u64` when the manifest was read.
    #[inline]
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }
}

/// One file of the package. `mode`, `created`, `hidden` and `system` are skipped: nile reads them and
/// applies none, and Amazon publishes Windows builds only, where they would not change what runs.
#[derive(Debug)]
pub struct ManifestFile {
    path: ManifestPath,
    size: u64,
    sha256: Sha256Digest,
}

impl ManifestFile {
    #[inline]
    #[must_use]
    pub const fn path(&self) -> &ManifestPath {
        &self.path
    }

    #[inline]
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// The file's content hash, which is also its name under the download URL (`files/<hex>`).
    #[inline]
    #[must_use]
    pub const fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }
}

/// A SHA-256 digest; [`fmt::Display`] spells it in lowercase hex, as the download URL wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// Why a manifest was refused. Every case refuses the whole manifest: a partial one could install
/// a game with files missing and no way to tell.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("the Amazon manifest's {what} is larger than {limit} bytes")]
    TooLarge { what: &'static str, limit: usize },
    #[error("the Amazon manifest lists more than {limit} {what}")]
    TooMany { what: &'static str, limit: usize },
    /// The bytes are not the protobuf messages they should be.
    #[error("the Amazon manifest is malformed: {0}")]
    Malformed(&'static str),
    #[error("the Amazon manifest is compressed with algorithm {0}, which decanter does not know")]
    UnknownCompression(u64),
    #[error("the Amazon manifest's body does not decompress as LZMA")]
    Decompression(#[source] std::io::Error),
    #[error("the Amazon manifest carries no signature")]
    Unsigned,
    #[error("the Amazon manifest is signed with algorithm {0}, which decanter does not know")]
    UnknownSignature(u64),
    /// decanter's own copy of Amazon's key does not load or no longer matches its pin: a broken build.
    #[error("the embedded Amazon manifest key is unusable: {0}")]
    Key(String),
    #[error("the Amazon manifest's signature does not match its content")]
    BadSignature,
    #[error("the Amazon manifest holds no package")]
    NoPackage,
    /// nile installs `packages[0]` and drops the rest without a word; which of several packages a
    /// game needs is unknown, so decanter refuses rather than guess.
    #[error("the Amazon manifest holds more than one package, which decanter does not install")]
    SeveralPackages,
    #[error("file {path:?} of the Amazon manifest has no hash")]
    MissingHash { path: String },
    /// `shake128` is in the enum, but nile cannot compute it (no digest length is passed), so no
    /// manifest it installs uses it.
    #[error("file {path:?} of the Amazon manifest is hashed with {}, decanter verifies sha256 only", hash_name(*.algorithm))]
    UnsupportedHash { path: String, algorithm: u64 },
    #[error("file {path:?} of the Amazon manifest has a {len}-byte hash, not a 32-byte sha256")]
    BadHashLength { path: String, len: usize },
    #[error("file {path:?} of the Amazon manifest has a negative size")]
    NegativeSize { path: String },
    #[error("the sizes of the Amazon manifest's files add up past 2^64 bytes")]
    TotalSizeOverflow,
    #[error("path {path:?} of the Amazon manifest is refused: {fault}")]
    InvalidPath { path: String, fault: PathFault },
    #[error("path {path:?} appears twice in the Amazon manifest, ignoring case")]
    DuplicatePath { path: String },
    #[error("path {path:?} of the Amazon manifest is both a file and the directory of another entry")]
    PathConflict { path: String },
}

fn hash_name(algorithm: u64) -> String {
    if algorithm == 1 {
        "shake128".to_owned()
    } else {
        format!("algorithm {algorithm}")
    }
}

/// Reads `raw`, the bytes of `manifest.proto` as downloaded, and returns the manifest once its
/// signature against Amazon's key holds and every path is safe to join under an install directory.
///
/// # Errors
///
/// Any [`ManifestError`]: nothing of a manifest that fails a check is returned.
#[inline]
pub fn parse_signed_manifest(raw: &[u8]) -> Result<Manifest, ManifestError> {
    if raw.len() > MAX_RAW_LEN {
        return Err(ManifestError::TooLarge {
            what: "download",
            limit: MAX_RAW_LEN,
        });
    }
    let body = envelope::open(raw)?;
    let manifest = body::decode(&body)?;
    tracing::debug!(
        files = manifest.files.len(),
        dirs = manifest.dirs.len(),
        total_size = manifest.total_size,
        "verified an amazon manifest"
    );
    Ok(manifest)
}

/// A raw path as an error may quote it: at most 120 characters, which `{:?}` then escapes.
fn shown(raw: &str) -> String {
    raw.chars().take(120).collect()
}
