// SPDX-License-Identifier: GPL-3.0-or-later
//! Downloading one modfile into a staging folder inside the game's mods folder, so that the data
//! stays in the library and on the filesystem the import renames into (analysis, sections 3 and 4).
//!
//! The file's record is asked for right before, since its `binary_url` may carry a hash that
//! expires. A file mod.io's scan calls malicious is refused; one whose game needs a logged-in user
//! to download is refused before asking. The stream is cut at the size mod.io lists and at
//! decanter-mods' own cap, and checked at the end against the listed size and MD5; the SHA-256 is
//! computed on the way for the log and the import's duplicate check. The download address carries
//! no key, so nothing of the user's goes to the file's host.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::files::{ModFile, RawDownload, RawFile, Virus};
use crate::games::{Game, check_access};
use crate::http::Client;
use crate::id::{FileId, ModioModId, now_unix};
use crate::terms::hex;
use crate::{Error, Result};

const RECORD_CALL: &str = "the file's record";
const CALL: &str = "a mod download";
const INCOMING: &str = "incoming";
const MAX_RECORD_BYTES: u64 = 1 << 20;

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

/// A folder of `<mods>/incoming/` that only this install writes, removed with everything in it
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
            let dir = incoming.join(format!("modio-{}-{count}", std::process::id()));
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
            Ok(()) => tracing::debug!(path = %self.dir.display(), "modio staging removed"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(path = %self.dir.display(), %err, "could not remove modio staging"),
        }
    }
}

/// A modfile, complete in the staging folder.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Downloaded {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    /// The record as read right before the download.
    pub file: ModFile,
    /// The host the download was asked from, for the mod's origin.
    pub host: String,
}

/// Downloads `file` of `mod_id` of `game` into `staging`, counting into `transfer`.
///
/// # Errors
///
/// [`Error::NoThirdPartyAccess`] and [`Error::NeedsLogin`] from the game's record,
/// [`Error::Malicious`], [`Error::LinkExpired`], as a request, [`Error::SizeMismatch`] and
/// [`Error::HashMismatch`] when what came is not what mod.io lists, [`Error::TooLarge`] past
/// decanter-mods' size cap, [`Error::NoSpace`], [`Error::Cancelled`] or [`Error::Io`]; the partial
/// file is removed then.
#[inline]
pub async fn download(
    client: &Client,
    staging: &Staging,
    game: &Game,
    mod_id: ModioModId,
    file: FileId,
    transfer: &Transfer,
) -> Result<Downloaded> {
    let (record, url) = fresh_record(client, game, mod_id, file).await?;
    let limit = decanter_mods::MAX_MOD_BYTES;
    if record.size.is_some_and(|size| size > limit) {
        return Err(Error::TooLarge { call: CALL, limit });
    }
    staging.check_space(record.size.unwrap_or_default())?;
    if let Some(size) = record.size {
        transfer.total.store(size, Ordering::Relaxed);
    }
    let host = url.host_str().unwrap_or_default().to_owned();
    let path = staging.dir.join(format!("{mod_id}-{file}.archive"));
    let written = write(client, url, &path, &record, transfer).await;
    if written.is_err()
        && let Err(err) = tokio::fs::remove_file(&path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %err, "could not remove a partial download");
    }
    let (size, sha256) = written?;
    tracing::info!(
        game_id = %game.id,
        mod_id = %mod_id,
        file_id = %file,
        bytes = size,
        md5_checked = record.md5.is_some(),
        virus = ?record.virus,
        sha256,
        "modio file downloaded"
    );
    Ok(Downloaded {
        path,
        size,
        sha256,
        file: record,
        host,
    })
}

/// The file's record, asked for right before its download, and its download address once the
/// game, the virus scan and the address's expiry allow it.
async fn fresh_record(client: &Client, game: &Game, mod_id: ModioModId, file: FileId) -> Result<(ModFile, url::Url)> {
    check_access(game)?;
    if game.access.needs_login() {
        return Err(Error::NeedsLogin);
    }
    let raw: RawFile = client
        .api(
            &format!("/games/{}/mods/{mod_id}/files/{file}", game.id),
            &[],
            RECORD_CALL,
            MAX_RECORD_BYTES,
        )
        .await?;
    let record = raw
        .validate()
        .filter(|record| record.id == file)
        .ok_or_else(|| Error::malformed(RECORD_CALL, "not this file's record"))?;
    if record.virus == Virus::Malicious {
        tracing::warn!(game_id = %game.id, mod_id = %mod_id, file_id = %file, "modio file flagged malicious, refused");
        return Err(Error::Malicious);
    }
    let (url, expires) = raw
        .download()
        .and_then(RawDownload::link)
        .ok_or_else(|| Error::malformed(RECORD_CALL, "no download address on mod.io's hosts"))?;
    if expires != 0 && expires <= now_unix() {
        return Err(Error::LinkExpired);
    }
    Ok((record, url))
}

async fn write(
    client: &Client,
    url: url::Url,
    path: &Path,
    record: &ModFile,
    transfer: &Transfer,
) -> Result<(u64, String)> {
    let limit = decanter_mods::MAX_MOD_BYTES;
    let expected = record.size;
    let mut response = client.file(url, CALL).await?;
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
    if expected.is_none()
        && let Some(length) = declared
    {
        transfer.total.store(length, Ordering::Relaxed);
    }
    let mut file = tokio::fs::OpenOptions::new()
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
        file.write_all(&chunk).await.map_err(Error::io(path))?;
        transfer.advance(counted)?;
    }
    file.flush().await.map_err(Error::io(path))?;
    if let Some(listed) = expected
        && size != listed
    {
        return Err(Error::SizeMismatch {
            expected: listed,
            got: size,
        });
    }
    if let Some(listed) = record.md5.as_deref()
        && hex(&md5.finalize().0) != listed
    {
        tracing::warn!(file_id = %record.id, "modio download's md5 differs from the listed one");
        return Err(Error::HashMismatch);
    }
    Ok((size, hex(&sha256.finalize())))
}
