//! `deployed/ledger.toml`: every file decanter copied into a Steam game's install, with the hash of
//! what it put there and of the install's own file it saved aside. Written before a pass touches
//! the install and again after, so that the next pass, also one after a crash, tells its own files
//! from what Steam or the game put there by their content.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::manifest::is_sha256;
use crate::path::{ModId, ModPath, id_component};
use crate::{Error, MAX_MOD_FILES, Result, files};

const SCHEMA: u32 = 1;

#[derive(Debug, Default)]
pub(crate) struct Ledger {
    /// The account whose set the install holds; `None` while it holds nothing of decanter's.
    pub(crate) account: Option<String>,
    pub(crate) files: BTreeMap<ModPath, Placed>,
    /// Folders decanter made in the install for files a mod adds, removed again once empty.
    pub(crate) dirs: BTreeSet<ModPath>,
}

#[derive(Debug, Clone)]
pub(crate) struct Placed {
    pub(crate) owner: ModId,
    /// SHA-256 of what decanter put, or is about to put, at the path.
    pub(crate) hash: String,
    /// SHA-256 of the install's own file, saved aside as `backups/<hash>`; `None` for a path only
    /// a mod has.
    pub(crate) base: Option<String>,
    pub(crate) base_executable: bool,
    /// The file as it was once its copy was done; `None` while a pass is placing it.
    pub(crate) stamp: Option<Stamp>,
    /// While a pass replaces one of decanter's files with another mod's: the hash it replaces.
    pub(crate) previous: Option<String>,
    /// While a pass puts the install's own file back.
    pub(crate) is_leaving: bool,
}

/// Size and modification time, which spare hashing a file decanter placed while both are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    len: u64,
    modified: u64,
}

impl Stamp {
    pub(crate) fn of(metadata: &fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX));
        Self {
            len: metadata.len(),
            modified,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RawLedger {
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    files: BTreeMap<String, RawPlaced>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dirs: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RawPlaced {
    owner: String,
    hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    base_executable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stamp: Option<Stamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<String>,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    leaving: bool,
}

impl Ledger {
    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty() && self.dirs.is_empty()
    }

    /// A game with no file yet has nothing of decanter's in its install.
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let Some(raw) = files::read::<RawLedger>(path)? else {
            return Ok(Self::default());
        };
        let bad = |reason: &dyn core::fmt::Display| Error::malformed(path, reason);
        if raw.schema != SCHEMA {
            return Err(bad(&"unknown schema"));
        }
        if raw.files.len() > MAX_MOD_FILES || raw.dirs.len() > MAX_MOD_FILES {
            return Err(bad(&"lists more files than decanter writes"));
        }
        let account = raw
            .account
            .map(|account| id_component(&account).map(str::to_owned).map_err(|err| bad(&err)))
            .transpose()?;
        let is_hash = |hash: Option<&String>| hash.is_none_or(|hash| is_sha256(hash));
        let mut files = BTreeMap::new();
        for (text, placed) in raw.files {
            if !is_hash(Some(&placed.hash)) || !is_hash(placed.base.as_ref()) || !is_hash(placed.previous.as_ref()) {
                return Err(bad(&"a hash is not a SHA-256"));
            }
            files.insert(
                ModPath::parse(&text).map_err(|err| bad(&err))?,
                Placed {
                    owner: ModId::parse(&placed.owner).map_err(|err| bad(&err))?,
                    hash: placed.hash,
                    base: placed.base,
                    base_executable: placed.base_executable,
                    stamp: placed.stamp,
                    previous: placed.previous,
                    is_leaving: placed.leaving,
                },
            );
        }
        let dirs = raw
            .dirs
            .iter()
            .map(|text| ModPath::parse(text).map_err(|err| bad(&err)))
            .collect::<Result<_>>()?;
        Ok(Self { account, files, dirs })
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let raw = RawLedger {
            schema: SCHEMA,
            account: self.account.clone(),
            files: self
                .files
                .iter()
                .map(|(file, placed)| {
                    (
                        file.as_str().to_owned(),
                        RawPlaced {
                            owner: placed.owner.as_str().to_owned(),
                            hash: placed.hash.clone(),
                            base: placed.base.clone(),
                            base_executable: placed.base_executable,
                            stamp: placed.stamp,
                            previous: placed.previous.clone(),
                            leaving: placed.is_leaving,
                        },
                    )
                })
                .collect(),
            dirs: self.dirs.iter().map(|dir| dir.as_str().to_owned()).collect(),
        };
        files::write(path, &raw)
    }
}
