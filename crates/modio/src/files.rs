// SPDX-License-Identifier: GPL-3.0-or-later
//! A mod's files (modfiles) as the reference documents them: `filesize`, `filehash.md5`,
//! `virus_status`, `virus_positive` and `download.binary_url` with `download.date_expires`.
//!
//! A listing's download address is not kept: when a game requires API-initiated downloads it
//! carries a verification hash that expires, so "saving and reusing the `binary_url` won't work"
//! and [`crate::download`] asks for the file again right before it downloads.

use serde::Deserialize;

use crate::id::{FileId, checked_url, date, display_text, md5_hex};

const MAX_NAME_CHARS: usize = 200;
const MAX_VERSION_CHARS: usize = 64;
const MAX_CHANGELOG_CHARS: usize = 2000;

/// What mod.io's virus scan says of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Virus {
    /// Scanned, nothing found.
    Clean,
    /// Not scanned yet, or the scan is not done.
    Unscanned,
    /// `virus_positive` 2: potentially harmful. Shown as a warning; the user decides.
    Harmful,
    /// `virus_positive` 1: malicious. Never downloaded.
    Malicious,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModFile {
    pub id: FileId,
    pub version: Option<String>,
    pub filename: String,
    /// The size the download must match, when given.
    pub size: Option<u64>,
    /// Lowercase hex, when given.
    pub md5: Option<String>,
    pub virus: Virus,
    /// `YYYY-MM-DD`.
    pub added: Option<String>,
    pub date_added: i64,
    pub changelog: String,
}

#[derive(Deserialize)]
pub(crate) struct RawFile {
    id: u64,
    #[serde(default)]
    date_added: i64,
    #[serde(default)]
    virus_status: u32,
    #[serde(default)]
    virus_positive: u32,
    #[serde(default)]
    filesize: Option<u64>,
    #[serde(default)]
    filehash: Option<RawHash>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    download: Option<RawDownload>,
}

#[derive(Deserialize)]
struct RawHash {
    #[serde(default)]
    md5: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RawDownload {
    #[serde(default)]
    binary_url: Option<String>,
    #[serde(default)]
    date_expires: i64,
}

impl RawDownload {
    /// The address when it is HTTPS on a mod.io host, and when it stops working (Unix, 0 for not said).
    pub(crate) fn link(&self) -> Option<(url::Url, i64)> {
        Some((checked_url(self.binary_url.as_deref()?)?, self.date_expires))
    }
}

impl RawFile {
    pub(crate) const fn download(&self) -> Option<&RawDownload> {
        self.download.as_ref()
    }

    pub(crate) fn validate(&self) -> Option<ModFile> {
        let virus = match (self.virus_status, self.virus_positive) {
            (_, 1) => Virus::Malicious,
            (_, 2) => Virus::Harmful,
            (1, _) => Virus::Clean,
            _ => Virus::Unscanned,
        };
        let id = FileId::new(self.id)?;
        Some(ModFile {
            id,
            version: self
                .version
                .as_deref()
                .map(|text| display_text(text, MAX_VERSION_CHARS))
                .filter(|text| !text.is_empty()),
            filename: self
                .filename
                .as_deref()
                .map(|text| display_text(text, MAX_NAME_CHARS))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| id.to_string()),
            size: self.filesize.filter(|&size| size > 0),
            md5: self
                .filehash
                .as_ref()
                .and_then(|hash| hash.md5.as_deref())
                .and_then(md5_hex),
            virus,
            added: date(self.date_added),
            date_added: self.date_added,
            changelog: self
                .changelog
                .as_deref()
                .map(|text| display_text(text, MAX_CHANGELOG_CHARS))
                .unwrap_or_default(),
        })
    }
}
