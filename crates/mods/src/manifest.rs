//! `mod.toml`: what one mod is and the files it carries, written by decanter at import and checked
//! again on every read, since anything may have edited the library folder since.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::path::{ModId, ModPath};
use crate::{Error, MAX_MOD_BYTES, MAX_MOD_FILES, Result, files};

pub(crate) const MANIFEST: &str = "mod.toml";
pub(crate) const FILES: &str = "files";
const SCHEMA: u32 = 1;
const MAX_NAME_CHARS: usize = 200;
/// Both names of the Galaxy hook's pair: the proxy must stay what the game loads and the real DLL
/// what the proxy forwards to (design, section 3).
const RESERVED: [&str; 2] = ["galaxy64.dll", "galaxy64.orig.dll"];

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModManifest {
    pub id: ModId,
    pub name: String,
    pub source_file_name: String,
    /// Of the archive the user gave; a folder of loose files has none.
    pub source_sha256: Option<String>,
    pub source_size: u64,
    pub added_at: i64,
    /// The game's subfolder the files go to; `None` for its root.
    pub target: Option<ModPath>,
    pub files: Vec<ModFile>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModFile {
    /// Relative to `target`.
    pub path: ModPath,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Serialize, Deserialize)]
struct RawManifest {
    schema: u32,
    id: String,
    name: String,
    source_file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_sha256: Option<String>,
    source_size: u64,
    added_at: i64,
    #[serde(default)]
    target: String,
    #[serde(default)]
    files: Vec<RawFile>,
}

#[derive(Serialize, Deserialize)]
struct RawFile {
    path: String,
    size: u64,
    sha256: String,
    #[serde(default)]
    executable: bool,
}

impl ModManifest {
    #[must_use]
    #[inline]
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// Where `file` lands, relative to the game's root.
    ///
    /// # Errors
    ///
    /// When `target` and the file's path together are longer than a path may be.
    #[inline]
    pub fn game_path(&self, file: &ModFile) -> Result<ModPath> {
        file.path.inside(self.target.as_ref())
    }

    /// The mod in `dir`, whose name must be its id.
    pub(crate) fn read(dir: &Path, id: &ModId) -> Result<Option<Self>> {
        let path = dir.join(MANIFEST);
        let Some(raw) = files::read::<RawManifest>(&path)? else {
            return Ok(None);
        };
        Self::validate(raw, id, &path).map(Some)
    }

    pub(crate) fn write(&self, dir: &Path) -> Result<()> {
        let raw = RawManifest {
            schema: SCHEMA,
            id: self.id.as_str().to_owned(),
            name: self.name.clone(),
            source_file_name: self.source_file_name.clone(),
            source_sha256: self.source_sha256.clone(),
            source_size: self.source_size,
            added_at: self.added_at,
            target: self.target.as_ref().map(ModPath::as_str).unwrap_or_default().to_owned(),
            files: self
                .files
                .iter()
                .map(|file| RawFile {
                    path: file.path.as_str().to_owned(),
                    size: file.size,
                    sha256: file.sha256.clone(),
                    executable: file.executable,
                })
                .collect(),
        };
        files::write(&dir.join(MANIFEST), &raw)
    }

    fn validate(raw: RawManifest, id: &ModId, path: &Path) -> Result<Self> {
        let bad = |reason: &str| Error::malformed(path, &reason);
        if raw.schema != SCHEMA {
            return Err(bad("unknown schema"));
        }
        if raw.id != id.as_str() {
            return Err(bad("id is not its folder's name"));
        }
        let name = display_name(&raw.name).ok_or_else(|| bad("name is empty, too long or has control characters"))?;
        let source_file_name = display_name(&raw.source_file_name)
            .ok_or_else(|| bad("source_file_name is empty, too long or has control characters"))?;
        if raw.source_sha256.as_deref().is_some_and(|hash| !is_sha256(hash)) {
            return Err(bad("source_sha256 is not a SHA-256"));
        }
        let target = match raw.target.as_str() {
            "" => None,
            target => Some(ModPath::parse(target)?),
        };
        if raw.files.is_empty() {
            return Err(Error::Empty);
        }
        if raw.files.len() > MAX_MOD_FILES {
            return Err(Error::TooManyFiles);
        }
        let mut files = Vec::with_capacity(raw.files.len());
        let mut total = 0_u64;
        for file in raw.files {
            if !is_sha256(&file.sha256) {
                return Err(bad("a file's sha256 is not a SHA-256"));
            }
            total = total.saturating_add(file.size);
            files.push(ModFile {
                path: ModPath::parse(&file.path)?,
                size: file.size,
                sha256: file.sha256,
                executable: file.executable,
            });
        }
        if total > MAX_MOD_BYTES {
            return Err(Error::TooLarge);
        }
        check_paths(files.iter().map(|file| &file.path), target.as_ref())?;
        Ok(Self {
            id: id.clone(),
            name,
            source_file_name,
            source_sha256: raw.source_sha256,
            source_size: raw.source_size,
            added_at: raw.added_at,
            target,
            files,
        })
    }
}

/// The rules every mod's file list keeps, at import and on every read: no reserved name and no two
/// paths that are one file for a Windows game.
pub(crate) fn check_paths<'path>(paths: impl Iterator<Item = &'path ModPath>, target: Option<&ModPath>) -> Result<()> {
    let mut seen = HashMap::new();
    for path in paths {
        let in_game = path.inside(target)?;
        let name = in_game.file_name().to_lowercase();
        if RESERVED.contains(&name.as_str()) {
            return Err(Error::Reserved(in_game.as_str().to_owned()));
        }
        if let Some(first) = seen.insert(in_game.folded(), in_game.as_str().to_owned()) {
            return Err(Error::Collision {
                first,
                second: in_game.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

/// A name shown in the Mods tab: trimmed, printable and short.
pub(crate) fn display_name(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= MAX_NAME_CHARS && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
