// SPDX-License-Identifier: GPL-3.0-or-later
//! Downloading a version's zip into a staging folder inside the game's mods folder, so that the
//! data stays in the library and on the filesystem the import renames into (analysis, section 4).
//!
//! The listing gives a size per version and, as far as the analysis could find, no hash: a size it
//! gives must be matched exactly, the stream is cut at decanter-mods' own size cap otherwise, and
//! the SHA-256 is computed on the way for the log and the import's duplicate check. The archive's
//! own CRCs are checked by the import, which alone opens it.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::http::Client;
use crate::id::PackageId;
use crate::listing::PackageVersion;
use crate::{Error, Result};

const CALL: &str = "a package download";
const INCOMING: &str = "incoming";

static STAGING_COUNT: AtomicU32 = AtomicU32::new(0);
const MAX_STAGING_ATTEMPTS: u32 = 1000;

/// How far the downloads have got, written by the download task and read by the Mods tab on its
/// own timer, which also asks it to stop.
#[derive(Debug, Default)]
pub struct Transfer {
    done: AtomicU64,
    total: AtomicU64,
    current: AtomicUsize,
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

    /// Which of the plan's items is downloading, from 0.
    #[must_use]
    #[inline]
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
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

    /// Counts `bytes` more expected, for the downloads whose size is known before they start.
    #[inline]
    pub fn expect(&self, bytes: u64) {
        self.total.fetch_add(bytes, Ordering::Relaxed);
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
/// when dropped: the archives are only needed until the import has extracted them.
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
            let dir = incoming.join(format!("thunderstore-{}-{count}", std::process::id()));
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

    /// Refuses downloads of `bytes` in all that would leave the library's disk with less than
    /// decanter-mods' margin; the import checks again for what it extracts.
    ///
    /// # Errors
    ///
    /// [`Error::NoSpace`].
    #[inline]
    pub fn check_space(&self, bytes: u64) -> Result<()> {
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
            Ok(()) => tracing::debug!(path = %self.dir.display(), "thunderstore staging removed"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(path = %self.dir.display(), %err, "could not remove thunderstore staging"),
        }
    }
}

/// A version's zip, complete in the staging folder.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Downloaded {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// Downloads every item of `plan` into `staging`, dependencies first, after checking the disk
/// has room for the sizes the listing gives.
///
/// # Errors
///
/// As [`download`] and [`Staging::check_space`]; the downloads already complete stay in `staging`
/// until it is dropped.
#[inline]
pub async fn download_plan(
    client: &Client,
    staging: &Staging,
    plan: &crate::Plan,
    transfer: &Transfer,
) -> Result<Vec<Downloaded>> {
    let listed: u64 = plan
        .items
        .iter()
        .filter_map(|item| item.version.file_size)
        .fold(0, u64::saturating_add);
    staging.check_space(listed)?;
    transfer.expect(listed);
    let mut downloaded = Vec::with_capacity(plan.items.len());
    for (index, item) in plan.items.iter().enumerate() {
        transfer.current.store(index, Ordering::Relaxed);
        downloaded.push(download(client, staging, &item.package, &item.version, transfer).await?);
    }
    Ok(downloaded)
}

/// Downloads `version` of `package` into `staging`, counting into `transfer`.
///
/// # Errors
///
/// As a request, [`Error::SizeMismatch`] when the listing's size is not what came,
/// [`Error::TooLarge`] past decanter-mods' size cap, [`Error::Cancelled`], or [`Error::Io`]; the
/// partial file is removed then.
#[inline]
pub async fn download(
    client: &Client,
    staging: &Staging,
    package: &PackageId,
    version: &PackageVersion,
    transfer: &Transfer,
) -> Result<Downloaded> {
    let path = staging.dir.join(format!("{package}-{}.zip", version.number));
    let fetch = Fetch {
        call: CALL,
        limit: decanter_mods::MAX_MOD_BYTES,
        expected: version.file_size,
        transfer: Some(transfer),
    };
    let (size, sha256) = stream_to_file(client, version.download_url.clone(), &path, &fetch).await?;
    tracing::info!(
        package = %package,
        version = %version.number,
        bytes = size,
        listed_size = ?version.file_size,
        sha256,
        "thunderstore package downloaded"
    );
    Ok(Downloaded { path, size, sha256 })
}

/// How one GET is streamed to a file: its name in errors, at most `limit` bytes, exactly
/// `expected` when given, counted into `transfer` when given.
pub(crate) struct Fetch<'transfer> {
    pub call: &'static str,
    pub limit: u64,
    pub expected: Option<u64>,
    pub transfer: Option<&'transfer Transfer>,
}

/// Streams a GET of `url` into a new file at `path` and returns its size and SHA-256; a failure
/// removes the file.
pub(crate) async fn stream_to_file(
    client: &Client,
    url: url::Url,
    path: &Path,
    fetch: &Fetch<'_>,
) -> Result<(u64, String)> {
    let written = write(client, url, path, fetch).await;
    if written.is_err()
        && let Err(err) = tokio::fs::remove_file(path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %err, "could not remove a partial download");
    }
    written
}

async fn write(client: &Client, url: url::Url, path: &Path, fetch: &Fetch<'_>) -> Result<(u64, String)> {
    let &Fetch {
        call,
        limit,
        expected,
        transfer,
    } = fetch;
    if expected.is_some_and(|size| size > limit) {
        return Err(Error::TooLarge { call, limit });
    }
    let mut response = client.get(url, call).await?;
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
        return Err(Error::TooLarge { call, limit });
    }
    if expected.is_none()
        && let (Some(counter), Some(length)) = (transfer, declared)
    {
        counter.expect(length);
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(Error::io(path))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(|err| Error::network(call, err))? {
        let counted = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        size = size.saturating_add(counted);
        if size > limit {
            return Err(Error::TooLarge { call, limit });
        }
        if let Some(listed) = expected
            && size > listed
        {
            return Err(Error::SizeMismatch {
                expected: listed,
                got: size,
            });
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(Error::io(path))?;
        if let Some(counter) = transfer {
            counter.advance(counted)?;
        }
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
    Ok((size, hex(&hasher.finalize())))
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
