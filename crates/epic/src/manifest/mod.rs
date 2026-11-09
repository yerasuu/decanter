// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's manifest: the build's files, each the concatenation of parts of numbered chunks, in the
//! binary form of spec 6.1 to 6.4 or the older JSON form of spec 6.5.
//!
//! The binary grammar was not available to the spec's author, so section 6 is the only source: a
//! field it describes ambiguously refuses the manifest with a [`ManifestError`] instead of being
//! guessed at, and every count is capped before it sizes anything. Encrypted manifests and chunks
//! (preloads, spec 5.4) are refused, since decanter carries no AES-GCM. Paths stay untrusted until
//! each one is a [`ManifestPath`]; a manifest with one bad path is refused whole.

mod binary;
mod json;
mod reader;
pub(crate) mod write;

use core::fmt;
use std::collections::{HashMap, HashSet};

use decanter_runtime::install::{ManifestPath, PathFault, path};

/// The download, compressed or not: tens of mebibytes at most for the largest games (spec, 13.13).
pub(crate) const MAX_RAW_LEN: usize = 128 << 20;
/// The inflated body, bounding the inflate whatever its header declares (spec, 13.13).
const MAX_BODY_LEN: usize = 256 << 20;
/// The highest manifest version spec 6.1 knows; a later one may lay its header or columns out
/// otherwise.
const MAX_VERSION: u32 = 24;
const MAX_FILES: usize = 500_000;
/// Two tebibytes of 1 MiB chunks.
const MAX_CHUNKS: usize = 2_000_000;
const MAX_PARTS: usize = 8_000_000;
const MAX_STRING_UNITS: usize = 32 << 10;
const MAX_TAGS_PER_FILE: usize = 1_024;
const MAX_PREREQ_IDS: usize = 1_024;
const MAX_CUSTOM_FIELDS: usize = 16_384;
/// The largest chunk data the spec allows (7.2, step 3), which every chunk part must fit in.
pub(crate) const MAX_WINDOW: u32 = 1 << 20;
/// A chunk file is its header and its payload, at worst a little larger than its window when the
/// data did not compress.
pub(crate) const MAX_CHUNK_FILE: usize = 4 << 20;

/// A 128-bit id as four `u32` words, the form both manifest kinds reduce to (spec, 6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid(pub(crate) [u32; 4]);

impl Guid {
    /// The wire form: each word little-endian, in order.
    pub(crate) fn to_le_bytes(self) -> [u8; 16] {
        let mut out = [0_u8; 16];
        for (slot, word) in out.chunks_exact_mut(4).zip(self.0) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    const fn is_zero(self) -> bool {
        matches!(self.0, [0, 0, 0, 0])
    }
}

/// `%08X` of each word, the chunk name of feature levels below 22 (spec, 6.4 form b).
impl fmt::Display for Guid {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|word| write!(f, "{word:08X}"))
    }
}

/// A SHA-1 digest; [`fmt::Display`] spells it in lowercase hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha1Digest(pub(crate) [u8; 20]);

impl Sha1Digest {
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self(sha1_smol::Sha1::from(bytes).digest().bytes())
    }
}

impl fmt::Display for Sha1Digest {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// One chunk: a window of at most 1 MiB of data, fetched as one file from the CDN.
#[derive(Debug)]
pub struct Chunk {
    pub(crate) guid: Guid,
    /// Spec 7.2's rolling hash of the chunk's data.
    pub(crate) rolling_hash: u64,
    /// The SHA-1 of the chunk's data; a JSON manifest may not list it.
    pub(crate) sha1: Option<Sha1Digest>,
    pub(crate) group: u8,
    /// The size of the chunk's data once decoded.
    pub(crate) window_size: u32,
    /// Zero, or the id of the key the chunk is encrypted with (feature level 22 and later).
    secret: Guid,
}

/// A run of bytes of one chunk's data, `[offset, offset + size)`, at the next position of a file.
#[derive(Debug, Clone, Copy)]
pub struct ChunkPart {
    /// The chunk's index in [`Manifest::chunks`].
    pub(crate) chunk: usize,
    pub(crate) offset: u32,
    pub(crate) size: u32,
}

#[derive(Debug)]
pub struct ManifestFile {
    path: ManifestPath,
    sha1: Sha1Digest,
    size: u64,
    parts: Vec<ChunkPart>,
}

impl ManifestFile {
    #[inline]
    #[must_use]
    pub const fn path(&self) -> &ManifestPath {
        &self.path
    }

    /// The SHA-1 of the whole file (spec, 6.3 file manifest list).
    #[inline]
    #[must_use]
    pub const fn sha1(&self) -> &Sha1Digest {
        &self.sha1
    }

    /// Not stored in the manifest: the sum of its parts' sizes.
    #[inline]
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn parts(&self) -> &[ChunkPart] {
        &self.parts
    }
}

/// A verified manifest.
#[derive(Debug)]
pub struct Manifest {
    /// The meta section's, which picks the chunk folder and columns (spec, 6.3 and 6.4).
    feature_level: u32,
    app_name: String,
    build_version: String,
    launch_exe: String,
    launch_command: String,
    chunks: Vec<Chunk>,
    files: Vec<ManifestFile>,
    total_size: u64,
}

impl Manifest {
    #[inline]
    #[must_use]
    pub fn files(&self) -> &[ManifestFile] {
        &self.files
    }

    pub(crate) fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    pub(crate) const fn feature_level(&self) -> u32 {
        self.feature_level
    }

    #[inline]
    #[must_use]
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// The build's version label, the one the asset list's `buildVersion` names (spec, 4.1).
    #[inline]
    #[must_use]
    pub fn build_version(&self) -> &str {
        &self.build_version
    }

    /// The program to start, as the manifest spells it (backslashes, maybe a leading slash).
    #[inline]
    #[must_use]
    pub fn launch_exe(&self) -> &str {
        &self.launch_exe
    }

    #[inline]
    #[must_use]
    pub fn launch_command(&self) -> &str {
        &self.launch_command
    }

    /// The sum of every file's size, checked to fit a `u64` when the manifest was read.
    #[inline]
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }
}

/// Why a manifest was refused. Every case refuses the whole manifest: a partial one could install
/// a game with files missing and no way to tell.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("the Epic manifest's {what} is larger than {limit}")]
    TooLarge { what: &'static str, limit: usize },
    #[error("the Epic manifest lists more than {limit} {what}")]
    TooMany { what: &'static str, limit: usize },
    #[error("the Epic manifest is malformed: {0}")]
    Malformed(&'static str),
    #[error("the Epic manifest is neither the binary format nor JSON")]
    UnknownFormat,
    #[error("the Epic manifest is version {0}; decanter reads versions up to 24")]
    UnsupportedVersion(u32),
    #[error("the Epic manifest is stored in a way decanter does not know (flags {0:#04x})")]
    UnknownStorage(u8),
    /// A preload, or a build whose chunks are encrypted (spec, 5.4).
    #[error("the Epic manifest or its chunks are encrypted, which decanter does not install yet")]
    Encrypted,
    /// Spec 6.3 names the flag and nothing of how such a build is laid out.
    #[error("the Epic manifest is a legacy file-data manifest, which decanter does not install")]
    FileData,
    #[error("the Epic manifest's body does not inflate")]
    Inflate(#[source] std::io::Error),
    #[error("the Epic manifest's body does not match the SHA-1 in its header")]
    BodyHash,
    #[error("path {path:?} of the Epic manifest is refused: {fault}")]
    InvalidPath { path: String, fault: PathFault },
    #[error("path {path:?} appears twice in the Epic manifest, ignoring case")]
    DuplicatePath { path: String },
    #[error("path {path:?} of the Epic manifest is both a file and the folder of another")]
    PathConflict { path: String },
    #[error("file {path:?} of the Epic manifest is a symbolic link, which decanter does not install")]
    Symlink { path: String },
    #[error("file {path:?} of the Epic manifest names a chunk the manifest does not list")]
    UnknownChunk { path: String },
    #[error("file {path:?} of the Epic manifest reads past the end of one of its chunks")]
    PartOutsideChunk { path: String },
    #[error("a chunk of the Epic manifest is larger than the 1 MiB the format allows")]
    ChunkTooLarge,
    #[error("the sizes of the Epic manifest's files add up past 2^64 bytes")]
    TotalSizeOverflow,
}

/// What a parser hands over before the checks every manifest kind shares.
struct Parsed {
    feature_level: u32,
    app_name: String,
    build_version: String,
    launch_exe: String,
    launch_command: String,
    chunks: Vec<Chunk>,
    files: Vec<RawFile>,
}

struct RawFile {
    name: String,
    symlink_target: String,
    sha1: Sha1Digest,
    parts: Vec<RawPart>,
    install_tags: usize,
}

struct RawPart {
    guid: Guid,
    offset: u32,
    size: u32,
}

/// Reads `raw`, a manifest as downloaded (whose SHA-1 the caller checked against the manifest
/// answer, spec 5.3), and returns it once every check holds.
///
/// # Errors
///
/// Any [`ManifestError`]: nothing of a manifest that fails a check is returned.
#[inline]
pub fn parse_manifest(raw: &[u8]) -> Result<Manifest, ManifestError> {
    if raw.len() > MAX_RAW_LEN {
        return Err(ManifestError::TooLarge {
            what: "download",
            limit: MAX_RAW_LEN,
        });
    }
    let parsed = match raw.first() {
        Some(b'{') => json::parse(raw)?,
        Some(_) if raw.starts_with(&binary::MAGIC.to_le_bytes()) => binary::parse(raw)?,
        _ => return Err(ManifestError::UnknownFormat),
    };
    let manifest = checked(parsed)?;
    tracing::debug!(
        feature_level = manifest.feature_level,
        files = manifest.files.len(),
        chunks = manifest.chunks.len(),
        total_size = manifest.total_size,
        "read an epic manifest"
    );
    Ok(manifest)
}

/// The checks of both kinds: chunks unique and within the window, encrypted chunks refused, every
/// path safe and unique ignoring case, every part inside a listed chunk.
fn checked(parsed: Parsed) -> Result<Manifest, ManifestError> {
    let Parsed {
        feature_level,
        app_name,
        build_version,
        launch_exe,
        launch_command,
        chunks,
        files: raw_files,
    } = parsed;
    let mut by_guid = HashMap::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        if chunk.window_size > MAX_WINDOW {
            return Err(ManifestError::ChunkTooLarge);
        }
        if by_guid.insert(chunk.guid, index).is_some() {
            return Err(ManifestError::Malformed("a chunk is listed twice"));
        }
    }
    let mut files = Vec::with_capacity(raw_files.len());
    let mut folded = HashSet::with_capacity(raw_files.len());
    let mut total_size = 0_u64;
    let mut tagged = 0_usize;
    for raw in raw_files {
        let shown = shown(&raw.name);
        let path = ManifestPath::parse(&raw.name).map_err(|fault| ManifestError::InvalidPath {
            path: shown.clone(),
            fault,
        })?;
        if !raw.symlink_target.is_empty() {
            return Err(ManifestError::Symlink { path: shown });
        }
        if !folded.insert(path.folded()) {
            return Err(ManifestError::DuplicatePath { path: shown });
        }
        let mut size = 0_u64;
        let mut parts = Vec::with_capacity(raw.parts.len());
        for part in raw.parts {
            let chunk = *by_guid
                .get(&part.guid)
                .ok_or_else(|| ManifestError::UnknownChunk { path: shown.clone() })?;
            let window = chunks.get(chunk).map_or(0, |chunk| chunk.window_size);
            if part.offset.checked_add(part.size).is_none_or(|end| end > window) {
                return Err(ManifestError::PartOutsideChunk { path: shown });
            }
            size = size.saturating_add(u64::from(part.size));
            parts.push(ChunkPart {
                chunk,
                offset: part.offset,
                size: part.size,
            });
        }
        total_size = total_size.checked_add(size).ok_or(ManifestError::TotalSizeOverflow)?;
        if raw.install_tags > 0 {
            tagged = tagged.saturating_add(1);
        }
        files.push(ManifestFile {
            path,
            sha1: raw.sha1,
            size,
            parts,
        });
    }
    for file in &files {
        if path::ancestors(&file.path.folded()).any(|dir| folded.contains(dir)) {
            return Err(ManifestError::PathConflict {
                path: shown(file.path.as_str()),
            });
        }
    }
    let encrypted = chunks.iter().filter(|chunk| chunk.is_encrypted()).count();
    if encrypted > 0 {
        tracing::warn!(
            encrypted,
            chunks = chunks.len(),
            "epic manifest lists encrypted chunks, refused"
        );
        return Err(ManifestError::Encrypted);
    }
    if tagged > 0 {
        // Spec 6.3 lists the tags but no rule for which a default install takes, so every file is
        // installed and the count logged for the owner's first real manifests.
        tracing::info!(
            tagged,
            files = files.len(),
            "epic manifest has files with install tags, installing all of them"
        );
    }
    Ok(Manifest {
        feature_level,
        app_name,
        build_version,
        launch_exe,
        launch_command,
        chunks,
        files,
        total_size,
    })
}

impl Chunk {
    /// A chunk list of feature level 22 and later carries a secret id per chunk; a non-zero one
    /// names the key that decrypts it (spec, 6.4 and 7.2).
    const fn is_encrypted(&self) -> bool {
        !self.secret.is_zero()
    }
}

/// A raw path as an error may quote it: at most 120 characters, which `{:?}` then escapes.
fn shown(raw: &str) -> String {
    raw.chars().take(120).collect()
}

/// Paths of `new` whose file is new or changed against `old`, the manifest of the install being
/// updated.
pub(crate) fn changed<'manifest>(new: &'manifest Manifest, old: &Manifest) -> HashSet<&'manifest str> {
    let previous: HashMap<&str, (&Sha1Digest, u64)> = old
        .files
        .iter()
        .map(|file| (file.path.as_str(), (&file.sha1, file.size)))
        .collect();
    new.files
        .iter()
        .filter(|file| {
            previous
                .get(file.path.as_str())
                .is_none_or(|&(sha1, size)| *sha1 != file.sha1 || size != file.size)
        })
        .map(|file| file.path.as_str())
        .collect()
}

/// Files of `old` that `new` no longer lists, which an update deletes.
pub(crate) fn dropped<'manifest>(new: &Manifest, old: &'manifest Manifest) -> Vec<&'manifest ManifestPath> {
    let current: HashSet<&str> = new.files.iter().map(|file| file.path.as_str()).collect();
    old.files
        .iter()
        .map(|file| &file.path)
        .filter(|path| !current.contains(path.as_str()))
        .collect()
}
