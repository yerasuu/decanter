// SPDX-License-Identifier: GPL-3.0-or-later
//! A mod's files: `/v1/games/{game}/mods/{mod}/files.json` (secondary, analysis section 1), read as
//! `files` (`file_id`, `name`, `version`, `category_name` or `category_id`, `size_in_bytes` or
//! `size_kb`, `md5`, `uploaded_time`, `file_name`) and `file_updates` (`old_file_id` →
//! `new_file_id`). None of those names are verified, so each is optional: a file without a number
//! is skipped and counted, one without an exact size or an MD5 says so instead of being checked.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::http::Client;
use crate::id::{FileId, GameDomain, NexusModId, api_url, date, display_text};
use crate::{Error, Result};

const CALL: &str = "the mod's files";
const MAX_BYTES: u64 = 8 << 20;
const MAX_FILES: usize = 2_000;
const MAX_UPDATES: usize = 10_000;
const MAX_NAME_CHARS: usize = 200;
const MAX_FIELD_CHARS: usize = 100;
const MAX_DESCRIPTION_CHARS: usize = 400;

/// The file's section on the mod's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Category {
    Main,
    Update,
    Optional,
    Miscellaneous,
    OldVersion,
    Archived,
    Deleted,
    Other,
}

impl Category {
    fn read(name: Option<&str>, id: Option<u64>) -> Self {
        match name.map(str::to_ascii_uppercase).as_deref() {
            Some("MAIN") => Self::Main,
            Some("UPDATE") => Self::Update,
            Some("OPTIONAL") => Self::Optional,
            Some("MISCELLANEOUS") => Self::Miscellaneous,
            Some("OLD_VERSION") => Self::OldVersion,
            Some("ARCHIVED") => Self::Archived,
            Some("DELETED") => Self::Deleted,
            _ => match id {
                Some(1) => Self::Main,
                Some(2) => Self::Update,
                Some(3) => Self::Optional,
                Some(4) => Self::OldVersion,
                Some(5) => Self::Miscellaneous,
                Some(6) => Self::Deleted,
                Some(7) => Self::Archived,
                _ => Self::Other,
            },
        }
    }

    /// Whether the author still offers it as current, rather than kept for history.
    #[must_use]
    #[inline]
    pub const fn is_current(self) -> bool {
        matches!(self, Self::Main | Self::Update | Self::Optional | Self::Miscellaneous)
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NexusFile {
    pub id: FileId,
    pub name: String,
    /// The file's version, free text the author types.
    pub version: Option<String>,
    pub category: Category,
    /// Exact, when the answer gives bytes; the download must then match it.
    pub size: Option<u64>,
    /// Rounded to kilobytes: shown, never checked.
    pub size_kb: Option<u64>,
    /// 32 lowercase hex digits.
    pub md5: Option<String>,
    /// Seconds since the Unix epoch, for ordering.
    pub uploaded: Option<i64>,
    /// `YYYY-MM-DD`.
    pub uploaded_date: Option<String>,
    /// The archive's own name, shown and used for the staged file once cleaned.
    pub file_name: Option<String>,
    pub description: String,
}

impl NexusFile {
    /// The size to show: exact bytes, else the kilobytes Nexus rounds to.
    #[must_use]
    #[inline]
    pub fn shown_size(&self) -> Option<u64> {
        self.size.or_else(|| self.size_kb.map(|kb| kb.saturating_mul(1024)))
    }
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FileList {
    /// Current categories first, then by upload, newest first.
    pub files: Vec<NexusFile>,
    /// Which file replaced which, as the author linked them.
    pub updates: HashMap<FileId, FileId>,
}

impl FileList {
    #[must_use]
    #[inline]
    pub fn get(&self, id: FileId) -> Option<&NexusFile> {
        self.files.iter().find(|file| file.id == id)
    }

    fn newest_main(&self) -> Option<&NexusFile> {
        self.files
            .iter()
            .filter(|file| file.category == Category::Main)
            .max_by_key(|file| file.uploaded.unwrap_or_default())
    }
}

#[derive(Deserialize)]
struct RawList {
    #[serde(default)]
    files: Vec<serde_json::Value>,
    #[serde(default)]
    file_updates: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawFile {
    file_id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    category_name: Option<String>,
    #[serde(default)]
    category_id: Option<u64>,
    #[serde(default)]
    size_in_bytes: Option<u64>,
    #[serde(default)]
    size_kb: Option<u64>,
    #[serde(default)]
    md5: Option<String>,
    #[serde(default)]
    uploaded_timestamp: Option<i64>,
    #[serde(default)]
    uploaded_time: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawUpdate {
    old_file_id: u64,
    new_file_id: u64,
}

/// The files of mod `id` of `domain`.
///
/// # Errors
///
/// As a request, [`Error::Malformed`] when the answer is not a file list or no file in it holds up.
#[inline]
pub async fn files(client: &Client, domain: &GameDomain, id: NexusModId) -> Result<FileList> {
    let url = api_url(&format!("/v1/games/{domain}/mods/{id}/files.json"))
        .ok_or_else(|| Error::InvalidDomain(domain.to_string()))?;
    let body = client.get_api(url, CALL, MAX_BYTES).await?;
    let raw: RawList = serde_json::from_slice(&body)
        .map_err(|err| Error::malformed(CALL, format!("line {}, column {}", err.line(), err.column())))?;
    let mut skipped = 0_usize;
    let mut without_md5 = 0_usize;
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(raw.files.len().min(MAX_FILES));
    for value in raw.files.into_iter().take(MAX_FILES) {
        match serde_json::from_value::<RawFile>(value).ok().and_then(validate) {
            Some(file) if seen.insert(file.id) => {
                if file.md5.is_none() {
                    without_md5 = without_md5.saturating_add(1);
                }
                files.push(file);
            }
            Some(_) => {}
            None => skipped = skipped.saturating_add(1),
        }
    }
    let mut updates = HashMap::new();
    let mut skipped_updates = 0_usize;
    for value in raw.file_updates.into_iter().take(MAX_UPDATES) {
        let link = serde_json::from_value::<RawUpdate>(value)
            .ok()
            .and_then(|link| Some((FileId::new(link.old_file_id)?, FileId::new(link.new_file_id)?)));
        match link {
            Some((old, new)) if old != new => {
                updates.insert(old, new);
            }
            _ => skipped_updates = skipped_updates.saturating_add(1),
        }
    }
    tracing::info!(
        %domain,
        mod_id = %id,
        files = files.len(),
        skipped,
        without_md5,
        updates = updates.len(),
        skipped_updates,
        "nexus file list read"
    );
    if files.is_empty() && skipped > 0 {
        return Err(Error::malformed(
            CALL,
            format!("none of its {skipped} files could be read"),
        ));
    }
    files.sort_by_key(|file| {
        (
            !file.category.is_current(),
            file.category,
            core::cmp::Reverse(file.uploaded.unwrap_or_default()),
        )
    });
    Ok(FileList { files, updates })
}

fn validate(raw: RawFile) -> Option<NexusFile> {
    let id = FileId::new(raw.file_id)?;
    let field = |given: Option<&str>, max: usize| {
        given
            .map(|text| display_text(text, max))
            .filter(|shown| !shown.is_empty())
    };
    let md5 = raw
        .md5
        .map(|text| text.trim().to_ascii_lowercase())
        .filter(|text| text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit()));
    Some(NexusFile {
        id,
        name: field(raw.name.as_deref(), MAX_NAME_CHARS).unwrap_or_else(|| id.to_string()),
        version: field(raw.version.as_deref(), MAX_FIELD_CHARS),
        category: Category::read(raw.category_name.as_deref(), raw.category_id),
        size: raw.size_in_bytes,
        size_kb: raw.size_kb,
        md5,
        uploaded: raw.uploaded_timestamp,
        uploaded_date: raw.uploaded_time.as_deref().and_then(date),
        file_name: field(raw.file_name.as_deref(), MAX_NAME_CHARS),
        description: field(raw.description.as_deref(), MAX_DESCRIPTION_CHARS).unwrap_or_default(),
    })
}

/// What a mod installed from file `installed` has newer.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Update<'list> {
    UpToDate,
    Newer(&'list NexusFile),
    /// Neither the file nor any main file is listed any more.
    Gone,
}

/// The file that replaces `installed`.
///
/// The end of the author's `file_updates` links from it when they lead to a listed file, else the
/// newest main file when `installed` is no longer current or no longer listed.
#[must_use]
#[inline]
pub fn update(list: &FileList, installed: FileId) -> Update<'_> {
    let mut current = installed;
    let mut visited = HashSet::from([installed]);
    while let Some(&next) = list.updates.get(&current) {
        if !visited.insert(next) {
            break;
        }
        current = next;
    }
    if current != installed
        && let Some(file) = list.get(current).filter(|file| file.category != Category::Deleted)
    {
        return Update::Newer(file);
    }
    match list.get(installed) {
        Some(file) if file.category.is_current() => Update::UpToDate,
        _ => match list.newest_main() {
            Some(main) if main.id != installed => Update::Newer(main),
            Some(_) => Update::UpToDate,
            None => Update::Gone,
        },
    }
}
