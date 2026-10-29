//! Fetching a planned install's files from GOG's CDN into its staging folder.
//!
//! A GOG file is a list of chunks, each its own signed CDN URL and each
//! zlib-compressed; the file is those chunks decompressed and concatenated
//! in order. Every chunk is checked against the manifest's compressed MD5,
//! inflated size and MD5 before it is written, so corruption on the way in
//! is a failed try rather than a broken game.
//!
//! Each file is written to `<path>.part` and renamed once complete. Every
//! [`COMMIT_BYTES`] or [`COMMIT_INTERVAL`] its bytes are synced and only
//! then recorded in the [`Journal`], so a record never names bytes a crash
//! could lose, and a stopped download carries on from the last record.
//!
//! Concurrency is per install rather than process-wide: two installs
//! running at once each get their own budget, which is worse for the disk
//! and much better for the user watching one of them finish.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use decanter_core::platform;
use futures_util::StreamExt;
use reqwest::StatusCode;
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;
use url::Url;

use super::journal::{COMMIT_BYTES, Journal, Start, part_path};
use super::tracking::Stop;

/// How many files transfer at once. GOG's CDN serves a single connection
/// well below a link's capacity, so this is about saturating the link, not
/// about the disk — past a handful it stops helping and starts making the
/// per-file progress jump around.
pub(super) const CONCURRENT_FILES: usize = 4;

/// How long a chunk keeps failing before the download pauses for the
/// network (owner's decision, #1281): a dropped connection or a CDN refusing
/// for a while is normal on a long transfer, losing the download to it is
/// not.
const RETRY_WINDOW: Duration = Duration::from_secs(60);

/// Doubles with each retry up to [`MAX_BACKOFF`], so a CDN that is briefly
/// refusing isn't hammered.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// A slow link records progress at least this often, not only every
/// [`COMMIT_BYTES`].
const COMMIT_INTERVAL: Duration = Duration::from_secs(5);

/// One chunk of a file: where to get it, and what it must hash to. GOG's
/// depot chunks are always zlib.
#[derive(Debug, Clone)]
pub(super) struct ChunkSource {
    pub(super) url: Url,
    /// Of the bytes as served, checked before spending work on inflating.
    pub(super) compressed_md5: [u8; 16],
    pub(super) md5: [u8; 16],
    /// Inflated, at most [`super::plan::MAX_CHUNK_SIZE`].
    pub(super) size: usize,
}

/// Which check a chunk failed: a failed try, retried like a dropped
/// connection.
#[derive(Debug, Clone, Copy, thiserror::Error)]
enum Mismatch {
    #[error("more bytes than the chunk can compress to")]
    Oversized,
    #[error("compressed MD5 mismatch")]
    CompressedMd5,
    #[error("not valid zlib")]
    Inflate,
    #[error("inflated size mismatch")]
    Size,
    #[error("MD5 mismatch")]
    Md5,
}

/// A chunk that failed its integrity check on every try. Names the file and
/// never the URL, which is signed.
#[derive(Debug, thiserror::Error)]
#[error("{file} arrived corrupted: chunk {chunk} failed its integrity check ({check}) on every try")]
pub(super) struct CorruptChunk {
    file: String,
    chunk: usize,
    check: Mismatch,
}

/// The context a chunk's last error carries when the network, not the
/// chunk, is what failed: nothing for the whole retry window, or a refused
/// signed URL, which only a new plan signs again. The download pauses
/// rather than fails.
#[derive(Debug, thiserror::Error)]
#[error("GOG's CDN could not be reached")]
pub(super) struct Unreachable;

/// What a download stopped by [`Cancel`] fails with, so a caller can tell
/// the user's own cancel from a failure worth reporting.
#[derive(Debug, thiserror::Error)]
#[error("download cancelled")]
#[non_exhaustive]
pub struct Cancelled;

const RUNNING: u8 = 0;
const CANCEL: u8 = 1;
const PAUSE: u8 = 2;
const ABORT: u8 = 3;
const QUIT: u8 = 4;

/// Set to stop an install, and why. Checked between chunks and between
/// reads, so a stop lands within one chunk's transfer rather than at the end
/// of the file.
#[derive(Clone, Default)]
pub(super) struct Cancel(Arc<AtomicU8>);

impl Cancel {
    /// The confirmed cancel wins over whatever else stopped the download,
    /// since it is the one that deletes it.
    pub(super) fn cancel(&self) {
        self.0.store(CANCEL, Ordering::Relaxed);
    }

    pub(super) fn pause(&self) {
        let _ = self
            .0
            .compare_exchange(RUNNING, PAUSE, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// decanter is quitting: kept like a pause, but not one the user asked for.
    pub(super) fn quit(&self) {
        let _ = self
            .0
            .compare_exchange(RUNNING, QUIT, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Another file failed: what the download ends with is that failure.
    fn abort(&self) {
        let _ = self
            .0
            .compare_exchange(RUNNING, ABORT, Ordering::Relaxed, Ordering::Relaxed);
    }

    pub(super) fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed) != RUNNING
    }

    /// What the user asked for, if they stopped it.
    pub(super) fn requested(&self) -> Option<Stop> {
        match self.0.load(Ordering::Relaxed) {
            CANCEL => Some(Stop::Cancel),
            PAUSE => Some(Stop::Pause),
            QUIT => Some(Stop::Quit),
            _ => None,
        }
    }

    fn check(&self) -> Result<(), Cancelled> {
        if self.is_stopped() {
            return Err(Cancelled);
        }
        Ok(())
    }
}

/// What [`download_all`] reports as bytes land: which file (by index into
/// the planned list), its name, and how much of it is on disk so far.
///
/// Per file, not per install: several transfer at once, so only the caller
/// can meaningfully sum them.
pub(super) type ProgressUpdate = (usize, String, u64);

/// The HTTP client every chunk request goes through.
///
/// Timeouts are the point. reqwest's defaults have none, so a chunk that
/// gets a connection and then no further bytes — a stale CDN URL, a
/// silently dropped response, the exact failure mode these hit in
/// practice — hangs forever instead of erroring. `read_timeout` bounds
/// idle time between reads, so a large chunk still actively transferring
/// isn't penalized.
fn client() -> anyhow::Result<&'static reqwest::Client> {
    static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .build()
                .ok()
        })
        .as_ref()
        .context("could not build the HTTP client")
}

/// A file to fetch and where to put it.
pub(super) struct PlannedDownload {
    pub(super) relative_path: String,
    pub(super) chunks: Vec<ChunkSource>,
    /// Given the mode a program needs to run once written, where this
    /// system has one; the move into the library keeps it.
    pub(super) is_executable: bool,
}

impl PlannedDownload {
    pub(super) fn size(&self) -> u64 {
        self.chunks
            .iter()
            .map(|chunk| u64::try_from(chunk.size).unwrap_or(u64::MAX))
            .sum()
    }
}

/// What every file of one run shares.
struct Run<'run> {
    client: &'run reqwest::Client,
    journal: &'run Journal,
    cancel: &'run Cancel,
    progress: &'run mpsc::Sender<ProgressUpdate>,
}

/// Downloads every file into the journal's staging, from where `starts`
/// says each one is, at most [`CONCURRENT_FILES`] at a time, reporting
/// progress as it goes.
///
/// Returns the bytes on disk per file, in the order given. The first
/// failure stops the whole call: a partial install is not a smaller
/// install, it's a broken one.
pub(super) async fn download_all(
    files: Vec<PlannedDownload>,
    starts: Vec<Start>,
    journal: &Journal,
    cancel: Cancel,
    progress: mpsc::Sender<ProgressUpdate>,
) -> anyhow::Result<Vec<u64>> {
    let client = crate::achv_try!(client());
    let mut written = vec![0u64; files.len()];
    let run = Run {
        client,
        journal,
        cancel: &cancel,
        progress: &progress,
    };

    let mut results =
        futures_util::stream::iter(files.into_iter().zip(starts).enumerate().map(|(index, (file, start))| {
            let run = &run;
            async move {
                let (chunk, bytes) = match start {
                    Start::Done => {
                        let size = file.size();
                        let _ = run.progress.send((index, file.relative_path.clone(), size)).await;
                        return (index, Ok(size));
                    }
                    Start::At { chunk, bytes } => (chunk, bytes),
                };
                // A corrupt chunk already names its file, and is the message the
                // user should see rather than a generic one wrapped around it.
                let bytes = download_file(run, &file, index, chunk, bytes).await.map_err(|err| {
                    if err.is::<CorruptChunk>() {
                        err
                    } else {
                        err.context(format!("{} failed to download", file.relative_path))
                    }
                });
                (index, bytes)
            }
        }))
        .buffer_unordered(CONCURRENT_FILES);

    // Collected rather than short-circuited: returning on the first error
    // would drop the other in-flight futures mid-write, before each has
    // recorded what it had synced.
    let mut failure = None;
    while let Some((index, bytes)) = results.next().await {
        match bytes {
            Ok(bytes) => {
                if let Some(slot) = written.get_mut(index) {
                    *slot = bytes;
                }
            }
            Err(err) => {
                if !err.is::<Cancelled>() {
                    tracing::error!(
                        file_index = index,
                        error = %err,
                        "file download failed, stopping the rest of the install"
                    );
                }
                cancel.abort();
                failure.get_or_insert(err);
            }
        }
    }

    match failure {
        Some(err) => Err(err),
        None => Ok(written),
    }
}

/// Where one file stands while it downloads.
struct Cursor {
    chunks: usize,
    written: u64,
    committed: u64,
    committed_at: Instant,
}

/// Fetches one file's chunks in order from chunk `chunk` (`bytes` into the
/// file), decompressing and appending each, then renames it complete.
async fn download_file(
    run: &Run<'_>,
    file: &PlannedDownload,
    index: usize,
    chunk: usize,
    bytes: u64,
) -> anyhow::Result<u64> {
    let destination = run.journal.files().join(&file.relative_path);
    let part = part_path(&destination);
    if let Some(parent) = destination.parent() {
        crate::achv_try!(
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))
        );
    }

    // Cut to the recorded length rather than appended to: whatever is past
    // it was never vouched for.
    let mut sink = crate::achv_try!(
        tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .await
            .with_context(|| format!("opening {}", part.display()))
    );
    crate::achv_try!(
        sink.set_len(bytes)
            .await
            .with_context(|| format!("cutting {} back to {bytes} bytes", part.display()))
    );
    crate::achv_try!(sink.seek(std::io::SeekFrom::Start(bytes)).await);

    let mut cursor = Cursor {
        chunks: chunk,
        written: bytes,
        committed: bytes,
        committed_at: Instant::now(),
    };
    if let Err(err) = fill(run, file, index, &mut sink, &mut cursor).await {
        // What landed since the last record is worth one more sync, unless
        // the user cancelled: then all of it goes.
        if run.cancel.requested() != Some(Stop::Cancel)
            && cursor.written > cursor.committed
            && let Err(commit_err) = commit(run.journal, index, &mut sink, &mut cursor).await
        {
            tracing::debug!(file_index = index, error = %commit_err, "couldn't record the last bytes of a stopped file");
        }
        return Err(err);
    }

    crate::achv_try!(commit(run.journal, index, &mut sink, &mut cursor).await);
    drop(sink);
    if file.is_executable
        && let Some(permissions) = platform::executable_permissions()
    {
        crate::achv_try!(
            tokio::fs::set_permissions(&part, permissions)
                .await
                .with_context(|| format!("making {} executable", part.display()))
        );
    }
    crate::achv_try!(
        tokio::fs::rename(&part, &destination)
            .await
            .with_context(|| format!("renaming {} into place", part.display()))
    );
    if let Some(parent) = destination.parent() {
        crate::achv_try!(
            decanter_runtime::journal::sync_dir(parent)
                .await
                .with_context(|| format!("syncing {}", parent.display()))
        );
    }
    crate::achv_try!(run.journal.done(index).await);
    tracing::debug!(
        relative_path = file.relative_path,
        bytes = cursor.written,
        chunk_count = file.chunks.len(),
        "file downloaded"
    );
    Ok(cursor.written)
}

async fn fill(
    run: &Run<'_>,
    file: &PlannedDownload,
    index: usize,
    sink: &mut tokio::fs::File,
    cursor: &mut Cursor,
) -> anyhow::Result<()> {
    for (chunk_index, chunk) in file.chunks.iter().enumerate().skip(cursor.chunks) {
        crate::achv_try!(run.cancel.check());
        let bytes = crate::achv_try!(fetch_chunk(run.client, chunk, run.cancel).await.map_err(|err| {
            match err.downcast::<Mismatch>() {
                Ok(check) => anyhow::Error::new(CorruptChunk {
                    file: file.relative_path.clone(),
                    chunk: chunk_index,
                    check,
                }),
                Err(err) => err,
            }
        }));
        crate::achv_try!(
            sink.write_all(&bytes)
                .await
                .with_context(|| format!("writing {}", file.relative_path))
        );
        cursor.chunks = chunk_index + 1;
        cursor.written += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if cursor.written - cursor.committed >= COMMIT_BYTES || cursor.committed_at.elapsed() >= COMMIT_INTERVAL {
            crate::achv_try!(commit(run.journal, index, sink, cursor).await);
        }
        // `trace!`, not `debug!`: same reasoning as `fetch_chunk_once` — a
        // file can be thousands of chunks, one line per chunk written.
        tracing::trace!(
            file_index = index,
            chunk_bytes = bytes.len(),
            file_bytes_written = cursor.written,
            "wrote chunk"
        );
        // A closed receiver means the caller stopped listening, which is
        // not a reason to stop downloading.
        let _ = run
            .progress
            .send((index, file.relative_path.clone(), cursor.written))
            .await;
    }
    Ok(())
}

/// Syncs the file's bytes, then records them: in that order, so no record
/// ever names bytes a crash could still lose.
async fn commit(
    journal: &Journal,
    index: usize,
    sink: &mut tokio::fs::File,
    cursor: &mut Cursor,
) -> anyhow::Result<()> {
    crate::achv_try!(sink.flush().await.context("flushing a downloaded file"));
    crate::achv_try!(sink.sync_data().await.context("syncing a downloaded file"));
    crate::achv_try!(journal.commit(index, cursor.chunks, cursor.written).await);
    cursor.committed = cursor.written;
    cursor.committed_at = Instant::now();
    Ok(())
}

/// One chunk, retried for up to [`RETRY_WINDOW`], returned decompressed.
async fn fetch_chunk(client: &reqwest::Client, chunk: &ChunkSource, cancel: &Cancel) -> anyhow::Result<Vec<u8>> {
    let mut failing_since = None;
    let mut backoff = FIRST_BACKOFF;
    loop {
        crate::achv_try!(cancel.check());
        let err = match fetch_chunk_once(client, chunk, cancel).await {
            Ok(bytes) => return Ok(bytes),
            // A stop isn't a transport failure and must not be retried.
            Err(err) if cancel.is_stopped() => return Err(err),
            Err(err) => err,
        };
        let status = err.downcast_ref::<reqwest::Error>().and_then(reqwest::Error::status);
        if status.is_some_and(|status| status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN) {
            tracing::warn!(error = %err, "chunk URL refused, pausing until it is signed again");
            return Err(err.context(Unreachable));
        }
        // Gone or never there: waiting won't change it.
        if status.is_some_and(|status| status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS) {
            tracing::error!(error = %err, "chunk refused by the CDN");
            return Err(err);
        }
        let failing_for = failing_since.get_or_insert_with(Instant::now).elapsed();
        if failing_for >= RETRY_WINDOW {
            tracing::error!(window_secs = RETRY_WINDOW.as_secs(), error = %err, "chunk still failing after the retry window");
            // Corrupt bytes for a whole minute are not the network.
            return Err(if err.is::<Mismatch>() {
                err
            } else {
                err.context(Unreachable)
            });
        }
        // A retry that goes on to succeed is not a failure of the install,
        // so this is `warn!`, not `error!`.
        tracing::warn!(failing_secs = failing_for.as_secs(), error = %err, "chunk attempt failed, retrying");
        sleep_unless_stopped(backoff.min(RETRY_WINDOW.saturating_sub(failing_for)), cancel).await;
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

/// So a pause or cancel during a backoff lands at once, not after it.
async fn sleep_unless_stopped(duration: Duration, cancel: &Cancel) {
    let until = Instant::now() + duration;
    while !cancel.is_stopped() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        tokio::time::sleep(left.min(Duration::from_millis(250))).await;
    }
}

async fn fetch_chunk_once(client: &reqwest::Client, chunk: &ChunkSource, cancel: &Cancel) -> anyhow::Result<Vec<u8>> {
    // `without_url`: the URL is signed, and these errors reach logs and the
    // UI.
    let response = crate::achv_try!(
        client
            .get(chunk.url.clone())
            .send()
            .await
            .map_err(reqwest::Error::without_url)
    );
    let response = crate::achv_try!(response.error_for_status().map_err(reqwest::Error::without_url));

    // Deflate grows incompressible data by a few bytes per 64 KiB block, so
    // a body past this is not the chunk; stopping here also keeps a server
    // that never stops sending from filling memory.
    let max_compressed = chunk.size + chunk.size / 64 + 1024;
    let expected = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0);
    let mut raw = Vec::with_capacity(expected.min(max_compressed));
    let mut compressed_md5 = md5::Context::new();
    // Streamed rather than `bytes()`, so a cancel lands mid-chunk instead
    // of waiting for the whole thing, and hashed as it arrives so the check
    // costs no second pass over the buffer.
    let mut body = response.bytes_stream();
    while let Some(part) = body.next().await {
        crate::achv_try!(cancel.check());
        let part = crate::achv_try!(part.map_err(reqwest::Error::without_url));
        if raw.len() + part.len() > max_compressed {
            return Err(Mismatch::Oversized.into());
        }
        compressed_md5.consume(&part);
        raw.extend_from_slice(&part);
    }
    if compressed_md5.finalize().0 != chunk.compressed_md5 {
        return Err(Mismatch::CompressedMd5.into());
    }

    let compressed_bytes = raw.len();
    let (size, md5) = (chunk.size, chunk.md5);
    // Inflating and hashing a chunk is CPU work of a few milliseconds, off
    // the async workers so four files in flight don't stall the runtime.
    let inflated = crate::achv_try!(
        tokio::task::spawn_blocking(move || inflate_checked(&raw, size, md5))
            .await
            .context("chunk check worker panicked")
    );
    let inflated = crate::achv_try!(inflated);
    // `trace!`, not `debug!`: a file transfers hundreds to thousands of
    // chunks, and logging each at `debug!` would flood the log for what a
    // caller sees as one file finishing.
    tracing::trace!(compressed_bytes, decompressed_bytes = inflated.len(), "fetched chunk");
    Ok(inflated)
}

/// Inflates a GOG depot chunk (raw zlib) and checks it against the
/// manifest's size and MD5.
fn inflate_checked(raw: &[u8], size: usize, md5: [u8; 16]) -> Result<Vec<u8>, Mismatch> {
    let mut inflated = Vec::with_capacity(size);
    // One byte past `size` tells a long chunk from a right one without
    // inflating all of whatever it really is.
    let limit = u64::try_from(size).map_or(u64::MAX, |size| size.saturating_add(1));
    crate::achv_try!(
        flate2::read::ZlibDecoder::new(raw)
            .take(limit)
            .read_to_end(&mut inflated)
            .map_err(|err| {
                tracing::debug!(error = %err, "chunk did not inflate");
                Mismatch::Inflate
            })
    );
    if inflated.len() != size {
        return Err(Mismatch::Size);
    }
    if md5::compute(&inflated).0 != md5 {
        return Err(Mismatch::Md5);
    }
    Ok(inflated)
}
