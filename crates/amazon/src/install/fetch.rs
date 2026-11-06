// SPDX-License-Identifier: GPL-3.0-or-later
//! Fetching a game's files into staging. Each file is one object, streamed to `<path>.part`,
//! hashed as it arrives and renamed once its SHA-256 matches the manifest; a stopped one asks the
//! CDN for the rest with `Range`. nile neither resumes nor fails on a bad hash (it only prints
//! "Checksum error"); here a mismatch deletes the part and fetches it again, and fails the download
//! once that keeps happening.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use decanter_runtime::journal::LineLog;
use futures_util::StreamExt as _;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex as AsyncMutex;

use super::disk;
use super::distribution::GameDownload;
use super::stop::StopFlag;
use crate::manifest::ManifestFile;

/// How many files transfer at once.
const CONCURRENT_FILES: usize = 4;

/// How long a file keeps failing on the network before the download pauses for it, as GOG's do
/// (#1281): a dropped connection is normal on a long transfer, losing the download to it is not.
const RETRY_WINDOW: Duration = Duration::from_secs(60);
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Fetches of one file whose hash came out wrong before the download fails for it.
const MAX_CORRUPT_TRIES: u32 = 3;

/// Files the CDN sent whole when asked for their rest, across the process, so the log says how
/// often `Range` is ignored (U2 of the analysis).
static RANGE_IGNORED: AtomicUsize = AtomicUsize::new(0);

/// One file to fetch: its index in the manifest, and where its part and its finished copy go.
pub(super) struct Job<'manifest> {
    pub(super) index: usize,
    pub(super) file: &'manifest ManifestFile,
    pub(super) done: PathBuf,
}

impl Job<'_> {
    pub(super) fn part(&self) -> PathBuf {
        part_path(&self.done)
    }
}

pub(super) fn part_path(done: &Path) -> PathBuf {
    let mut part = done.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// Why fetching the files stopped.
#[derive(Debug)]
pub(super) enum Failure {
    /// The stop flag was set; the caller reads why.
    Stopped,
    /// The CDN did not answer for the whole retry window, or refused the URL.
    Unreachable,
    DiskFull,
    Corrupt {
        path: String,
    },
    Io {
        path: String,
        source: io::Error,
    },
}

/// What a single try of one file ended in.
enum Try {
    Stopped,
    Mismatch,
    Refused(u16),
    Network(String),
    Io(io::Error),
}

/// What the files of one run share: bytes on disk per job, to report the download's total.
pub(super) struct Run<'run> {
    pub(super) download: &'run GameDownload,
    pub(super) flag: &'run StopFlag,
    pub(super) log: &'run AsyncMutex<LineLog>,
    pub(super) on_disk: &'run [AtomicU64],
    pub(super) current: &'run AtomicUsize,
}

/// Fetches every job, at most [`CONCURRENT_FILES`] at a time. The first failure stops the others:
/// a partial game is not a smaller game.
pub(super) async fn fetch_all(run: &Run<'_>, jobs: &[Job<'_>], mut tick: impl FnMut()) -> Result<(), Failure> {
    let client = super::distribution::client().map_err(|err| {
        tracing::error!(error = %err, "no amazon download client");
        Failure::Unreachable
    })?;
    let transfers = futures_util::stream::iter(jobs.iter().enumerate())
        .map(|(slot, job)| fetch_file(client, run, slot, job))
        .buffer_unordered(CONCURRENT_FILES);
    tokio::pin!(transfers);
    let mut ticker = tokio::time::interval(Duration::from_millis(150));
    let mut failure = None;
    loop {
        tokio::select! {
            next = transfers.next() => match next {
                None => break,
                Some(Ok(())) => {}
                Some(Err(err)) => {
                    run.flag.abort();
                    // A file that stopped because another failed says nothing of its own.
                    if failure.as_ref().is_none_or(|kept| matches!(kept, Failure::Stopped)) {
                        failure = Some(err);
                    }
                }
            },
            _ = ticker.tick() => tick(),
        }
    }
    tick();
    failure.map_or(Ok(()), Err)
}

async fn fetch_file(client: &reqwest::Client, run: &Run<'_>, slot: usize, job: &Job<'_>) -> Result<(), Failure> {
    let path = job.file.path().as_str();
    let mut corrupt_tries = 0_u32;
    let mut failing_since: Option<Instant> = None;
    let mut backoff = FIRST_BACKOFF;
    loop {
        if run.flag.requested().is_some() {
            return Err(Failure::Stopped);
        }
        let on_disk = || run.on_disk.get(slot).map_or(0, |bytes| bytes.load(Ordering::Relaxed));
        let before = on_disk();
        let outcome = try_file(client, run, slot, job).await;
        // A try that moved bytes before it failed is a connection that dropped, not a CDN that is
        // down, so the retry window starts again.
        if on_disk() > before {
            failing_since = None;
            backoff = FIRST_BACKOFF;
        }
        match outcome {
            Ok(()) => return finish(run, job).await,
            Err(Try::Stopped) => return Err(Failure::Stopped),
            Err(Try::Mismatch) => {
                corrupt_tries = corrupt_tries.saturating_add(1);
                discard_part(job).await;
                if let Some(on_disk) = run.on_disk.get(slot) {
                    on_disk.store(0, Ordering::Relaxed);
                }
                tracing::warn!(
                    path,
                    tries = corrupt_tries,
                    "an amazon file arrived with the wrong hash, fetching it again"
                );
                if corrupt_tries >= MAX_CORRUPT_TRIES {
                    return Err(Failure::Corrupt { path: path.to_owned() });
                }
            }
            Err(Try::Refused(status)) => {
                tracing::warn!(
                    path,
                    status,
                    host = run.download.host(),
                    "the amazon CDN refused a file"
                );
                return Err(Failure::Unreachable);
            }
            Err(Try::Network(reason)) => {
                let since = *failing_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= RETRY_WINDOW {
                    tracing::warn!(path, reason, "the amazon CDN kept failing for the whole retry window");
                    return Err(Failure::Unreachable);
                }
                tracing::debug!(
                    path,
                    reason,
                    backoff_ms = backoff.as_millis(),
                    "amazon file transfer failed, retrying"
                );
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    () = run.flag.stopped() => return Err(Failure::Stopped),
                }
                backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
            }
            Err(Try::Io(source)) if source.kind() == io::ErrorKind::StorageFull => return Err(Failure::DiskFull),
            Err(Try::Io(source)) => {
                return Err(Failure::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
}

/// One try: carries on from the part on disk when the CDN honours `Range`, else starts over.
async fn try_file(client: &reqwest::Client, run: &Run<'_>, slot: usize, job: &Job<'_>) -> Result<(), Try> {
    let part = job.part();
    let size = job.file.size();
    let mut have = match tokio::fs::symlink_metadata(&part).await {
        Ok(meta) if meta.file_type().is_file() && meta.len() <= size => meta.len(),
        Ok(_) => {
            discard_part(job).await;
            0
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
        Err(err) => return Err(Try::Io(err)),
    };
    let report = |bytes: u64| {
        if let Some(on_disk) = run.on_disk.get(slot) {
            on_disk.store(bytes, Ordering::Relaxed);
        }
        run.current.store(job.index, Ordering::Relaxed);
    };
    report(have);
    let mut hasher = if have > 0 {
        prefix_hash(&part).await?
    } else {
        Sha256::new()
    };
    if have < size {
        let mut request = client.get(run.download.file_url(job.file.sha256()));
        if have > 0 {
            request = request.header(RANGE, format!("bytes={have}-"));
        }
        let mut response = tokio::select! {
            sent = request.send() => sent.map_err(|err| Try::Network(err.without_url().to_string()))?,
            () = run.flag.stopped() => return Err(Try::Stopped),
        };
        let status = response.status();
        let resumes = match status {
            StatusCode::PARTIAL_CONTENT if have > 0 && range_start(&response) == Some(have) => true,
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => false,
            StatusCode::RANGE_NOT_SATISFIABLE => return Err(Try::Mismatch),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::GONE => {
                return Err(Try::Refused(status.as_u16()));
            }
            other => return Err(Try::Network(format!("HTTP {other}"))),
        };
        if have > 0 && !resumes {
            let ignored = RANGE_IGNORED.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            tracing::info!(
                path = job.file.path().as_str(),
                kept_bytes = have,
                status = status.as_u16(),
                ignored_total = ignored,
                "the amazon CDN ignored a Range request, fetching the file from its start"
            );
            have = 0;
            hasher = Sha256::new();
            report(0);
        }
        if let Some(parent) = part.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Try::Io)?;
        }
        let mut writer = if have > 0 {
            tokio::fs::OpenOptions::new().append(true).open(&part).await
        } else {
            tokio::fs::File::create(&part).await
        }
        .map_err(Try::Io)?;
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|err| Try::Network(err.without_url().to_string()))?,
                () = run.flag.stopped() => {
                    writer.flush().await.map_err(Try::Io)?;
                    return Err(Try::Stopped);
                }
            };
            let Some(chunk) = chunk else {
                break;
            };
            let length = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            if have.saturating_add(length) > size {
                return Err(Try::Mismatch);
            }
            writer.write_all(&chunk).await.map_err(Try::Io)?;
            hasher.update(&chunk);
            have = have.saturating_add(length);
            report(have);
        }
        writer.flush().await.map_err(Try::Io)?;
        writer.sync_all().await.map_err(Try::Io)?;
        if have < size {
            // The body ended early; the next try asks for the rest.
            return Err(Try::Network(format!("the body ended at {have} of {size} bytes")));
        }
    }
    if hasher.finalize().as_slice() == job.file.sha256().as_bytes() {
        Ok(())
    } else {
        Err(Try::Mismatch)
    }
}

/// The start of a `Content-Range: bytes <start>-<end>/<total>` answer.
fn range_start(response: &reqwest::Response) -> Option<u64> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let (start, _) = value.strip_prefix("bytes ")?.split_once('-')?;
    start.parse().ok()
}

async fn prefix_hash(part: &Path) -> Result<Sha256, Try> {
    let part = part.to_path_buf();
    tokio::task::spawn_blocking(move || disk::hasher_over(&part))
        .await
        .map_err(|err| Try::Io(io::Error::other(err)))?
        .map_err(Try::Io)
}

async fn discard_part(job: &Job<'_>) {
    if let Err(err) = tokio::fs::remove_file(job.part()).await
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = job.file.path().as_str(), error = %err, "couldn't delete a part file");
    }
}

/// Renames the verified part into place and records it, in that order: a record never names a file
/// a crash could leave as a part, and a file renamed but not recorded is hashed again on resume.
async fn finish(run: &Run<'_>, job: &Job<'_>) -> Result<(), Failure> {
    let io_failure = |source| Failure::Io {
        path: job.file.path().as_str().to_owned(),
        source,
    };
    tokio::fs::rename(job.part(), &job.done).await.map_err(io_failure)?;
    if let Some(parent) = job.done.parent() {
        decanter_runtime::journal::sync_dir(parent).await.map_err(io_failure)?;
    }
    let record = done_record(job.file);
    run.log.lock().await.append(record.as_bytes()).await.map_err(io_failure)
}

/// A finished file in the log: its hash and its path, so a later plan of another version keeps it
/// only when it still lists that file with that content.
pub(super) fn done_record(file: &ManifestFile) -> String {
    format!("done {} {}", file.sha256(), file.path().as_str())
}
