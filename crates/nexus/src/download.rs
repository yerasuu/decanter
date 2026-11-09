// SPDX-License-Identifier: GPL-3.0-or-later
//! Downloading one file into a staging folder inside the game's mods folder, so that the data
//! stays in the library and on the filesystem the import renames into (analysis, section 4).
//!
//! The link comes from `/v1/games/{game}/mods/{mod}/files/{file}/download_link.json`, which per
//! the analysis' secondary sources answers Premium accounts with a list of `{name, short_name,
//! URI}` mirrors and free ones with 403; the `key`/`expires` pair of the site's `nxm://` links is
//! not used, since decanter registers no handler for them. The stream is cut at the listed size
//! when the file list gives one in bytes, at decanter-mods' size cap otherwise, and its MD5 is
//! compared with the listed one when there is one; the SHA-256 is computed on the way for the log
//! and the import's duplicate check. The archive's own CRCs are checked by the import, which alone
//! opens it.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::files::NexusFile;
use crate::http::Client;
use crate::id::{FileId, GameDomain, NexusModId, api_url, checked_download_url};
use crate::{Error, Result};

const LINK_CALL: &str = "a download link";
const CALL: &str = "a mod download";
const MAX_LINK_BYTES: u64 = 64 << 10;
const INCOMING: &str = "incoming";
const MAX_FILE_NAME: usize = 120;

static STAGING_COUNT: AtomicU32 = AtomicU32::new(0);
const MAX_STAGING_ATTEMPTS: u32 = 1000;

/// How far the download has got, written by the download task and read by the Mods tab on its
/// own timer, which also asks it to stop.
#[derive(Debug, Default)]
pub struct Transfer {
    done: AtomicU64,
    total: AtomicU64,
    stop: AtomicBool,
}

impl Transfer {
    /// `(done, total)` bytes, or `None` before any size is known.
    #[must_use]
    #[inline]
    pub fn snapshot(&self) -> Option<(u64, u64)> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.done.load(Ordering::Relaxed).min(total), total))
    }

    /// Asks the download to stop; it then ends with [`Error::Cancelled`] and removes its file.
    #[inline]
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    #[must_use]
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn advance(&self, bytes: u64) -> Result<()> {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        if self.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}

/// A folder of `<mods>/incoming/` that only this download writes, removed with everything in it
/// when dropped: the archive is only needed until the import has extracted it.
#[derive(Debug)]
pub struct Staging {
    dir: PathBuf,
}

impl Staging {
    /// # Errors
    ///
    /// [`Error::Io`] when the folder can't be made.
    #[inline]
    pub fn new(mods: &decanter_mods::GameMods) -> Result<Self> {
        let incoming = mods.dir().join(INCOMING);
        fs::create_dir_all(&incoming).map_err(Error::io(&incoming))?;
        // Never into a folder already there: a leftover of a crashed run is not reused.
        for _ in 0..MAX_STAGING_ATTEMPTS {
            let count = STAGING_COUNT.fetch_add(1, Ordering::Relaxed);
            let dir = incoming.join(format!("nexus-{}-{count}", std::process::id()));
            #[expect(clippy::create_dir, reason = "create_dir_all would reuse a folder already there")]
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Self { dir }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(Error::io(&dir)(err)),
            }
        }
        Err(Error::Io {
            path: incoming,
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        })
    }

    /// Refuses a download of `bytes` that would leave the library's disk with less than
    /// decanter-mods' margin; the import checks again for what it extracts.
    fn check_space(&self, bytes: u64) -> Result<()> {
        match decanter_core::disk_usage(&self.dir) {
            Some((free, _)) if bytes.saturating_add(decanter_mods::FREE_SPACE_MARGIN) > free => {
                Err(Error::NoSpace { needed: bytes, free })
            }
            Some(_) => Ok(()),
            None => {
                tracing::warn!(path = %self.dir.display(), "free space unknown, downloading anyway");
                Ok(())
            }
        }
    }
}

#[expect(
    clippy::missing_trait_methods,
    reason = "Drop's provided methods are the right ones here"
)]
impl Drop for Staging {
    #[inline]
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.dir) {
            Ok(()) => tracing::debug!(path = %self.dir.display(), "nexus staging removed"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(path = %self.dir.display(), %err, "could not remove nexus staging"),
        }
    }
}

/// A file, complete in the staging folder.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Downloaded {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    /// Whether the file list gave an MD5 to compare; without one only the size, when exact, and
    /// the archive's own CRCs vouch for it.
    pub is_md5_checked: bool,
    /// The host the file came from, for its origin.
    pub host: String,
}

#[derive(Deserialize)]
struct RawLink {
    #[serde(rename = "URI")]
    uri: String,
}

/// A signed link to download `file` of mod `id`; Premium accounts only.
///
/// # Errors
///
/// [`Error::PremiumOnly`] on a 403, as a request otherwise, [`Error::Malformed`] when no mirror in
/// the answer is an HTTPS link on Nexus' hosts.
#[inline]
pub async fn download_link(client: &Client, domain: &GameDomain, id: NexusModId, file: FileId) -> Result<url::Url> {
    let url = api_url(&format!("/v1/games/{domain}/mods/{id}/files/{file}/download_link.json"))
        .ok_or_else(|| Error::InvalidDomain(domain.to_string()))?;
    let body = match client.get_api(url, LINK_CALL, MAX_LINK_BYTES).await {
        Ok(body) => body,
        Err(Error::Unavailable { status: 403, .. }) => {
            tracing::info!(%domain, mod_id = %id, file_id = %file, "nexus download link refused, premium only");
            return Err(Error::PremiumOnly);
        }
        Err(err) => return Err(err),
    };
    let mirrors: Vec<serde_json::Value> = serde_json::from_slice(&body)
        .map_err(|err| Error::malformed(LINK_CALL, format!("line {}, column {}", err.line(), err.column())))?;
    let count = mirrors.len();
    let Some(link) = mirrors
        .into_iter()
        .filter_map(|value| serde_json::from_value::<RawLink>(value).ok())
        .find_map(|raw| checked_download_url(&raw.uri))
    else {
        return Err(Error::malformed(
            LINK_CALL,
            format!("none of its {count} mirrors is an HTTPS link on Nexus' hosts"),
        ));
    };
    tracing::info!(
        %domain,
        mod_id = %id,
        file_id = %file,
        mirrors = count,
        host = link.host_str().unwrap_or_default(),
        "nexus download link received"
    );
    Ok(link)
}

/// Downloads `file` from `link` into `staging`, counting into `transfer`.
///
/// # Errors
///
/// [`Error::NoSpace`], [`Error::SizeMismatch`] when the listed size is not what came,
/// [`Error::Md5Mismatch`], [`Error::TooLarge`] past decanter-mods' size cap,
/// [`Error::Cancelled`], as a request, or [`Error::Io`]; the partial file is removed then.
#[inline]
pub async fn download(
    client: &Client,
    staging: &Staging,
    file: &NexusFile,
    link: url::Url,
    transfer: &Transfer,
) -> Result<Downloaded> {
    let limit = decanter_mods::MAX_MOD_BYTES;
    if file.size.is_some_and(|size| size > limit) {
        return Err(Error::TooLarge { call: CALL, limit });
    }
    staging.check_space(file.shown_size().unwrap_or_default())?;
    let host = link.host_str().unwrap_or_default().to_owned();
    let path = staging.dir.join(staged_name(file));
    let written = write(client, link, &path, file, transfer).await;
    if written.is_err()
        && let Err(err) = tokio::fs::remove_file(&path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %err, "could not remove a partial download");
    }
    let (size, sha256) = written?;
    tracing::info!(
        file_id = %file.id,
        bytes = size,
        listed_size = ?file.size,
        is_md5_checked = file.md5.is_some(),
        sha256,
        host,
        "nexus file downloaded"
    );
    if file.md5.is_none() {
        tracing::warn!(file_id = %file.id, "nexus lists no md5 for the file, only its size was checked");
    }
    Ok(Downloaded {
        path,
        size,
        sha256,
        is_md5_checked: file.md5.is_some(),
        host,
    })
}

async fn write(
    client: &Client,
    link: url::Url,
    path: &Path,
    file: &NexusFile,
    transfer: &Transfer,
) -> Result<(u64, String)> {
    let limit = decanter_mods::MAX_MOD_BYTES;
    let expected = file.size;
    let mut response = client.get_download(link, CALL).await?;
    let declared = response.content_length();
    if let (Some(listed), Some(length)) = (expected, declared)
        && listed != length
    {
        return Err(Error::SizeMismatch {
            expected: listed,
            got: length,
        });
    }
    if declared.is_some_and(|length| length > limit) {
        return Err(Error::TooLarge { call: CALL, limit });
    }
    if let Some(total) = expected.or(declared) {
        transfer.total.store(total, Ordering::Relaxed);
    }
    let mut out = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(Error::io(path))?;
    let mut sha256 = Sha256::new();
    let mut md5 = md5::Context::new();
    let mut size = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(|err| Error::network(CALL, err))? {
        let counted = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        size = size.saturating_add(counted);
        if size > limit {
            return Err(Error::TooLarge { call: CALL, limit });
        }
        if let Some(listed) = expected
            && size > listed
        {
            return Err(Error::SizeMismatch {
                expected: listed,
                got: size,
            });
        }
        sha256.update(&chunk);
        md5.consume(&chunk);
        out.write_all(&chunk).await.map_err(Error::io(path))?;
        transfer.advance(counted)?;
    }
    out.flush().await.map_err(Error::io(path))?;
    if let Some(listed) = expected
        && size != listed
    {
        return Err(Error::SizeMismatch {
            expected: listed,
            got: size,
        });
    }
    if let Some(listed) = file.md5.as_deref()
        && hex(&md5.finalize().0) != listed
    {
        tracing::warn!(file_id = %file.id, bytes = size, "nexus download md5 does not match its listing");
        return Err(Error::Md5Mismatch);
    }
    Ok((size, hex(&sha256.finalize())))
}

/// The archive's own name with only safe characters left, so that the import's preview shows
/// something the user recognises; the file's number when nothing is left.
fn staged_name(file: &NexusFile) -> String {
    let cleaned: String = file
        .file_name
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter_map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => Some(character),
            ' ' => Some('_'),
            _ => None,
        })
        .take(MAX_FILE_NAME)
        .collect();
    match cleaned.trim_start_matches('.') {
        "" => format!("nexus-{}", file.id),
        kept => kept.to_owned(),
    }
}

fn hex(digest: &[u8]) -> String {
    let mut text = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        for nibble in [byte >> 4_u8, byte & 0x0f] {
            text.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    text
}
