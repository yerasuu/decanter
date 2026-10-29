//! Fetching a planned install's files from GOG's CDN.
//!
//! A GOG file is a list of chunks, each its own signed CDN URL and each
//! zlib-compressed; the file is those chunks decompressed and concatenated
//! in order. Every chunk is checked against the manifest's compressed MD5,
//! inflated size and MD5 before it is written, so corruption on the way in
//! is a failed try rather than a broken game.
//!
//! Nothing here is resumable, and deliberately so — an install that fails
//! or is cancelled throws its staging directory away ([`super::staging`]),
//! so a retry always starts from nothing and there is no partial state to
//! reconcile.
//!
//! Concurrency is per install rather than process-wide: two installs
//! running at once each get their own budget, which is worse for the disk
//! and much better for the user watching one of them finish.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;
use decanter_core::platform;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use url::Url;

/// How many files transfer at once. GOG's CDN serves a single connection
/// well below a link's capacity, so this is about saturating the link, not
/// about the disk — past a handful it stops helping and starts making the
/// per-file progress jump around.
pub(super) const CONCURRENT_FILES: usize = 4;

/// Attempts per chunk before the file (and the install) fails. Signed CDN
/// URLs and long transfers make the occasional dropped connection normal;
/// losing a whole install to one is not.
const CHUNK_ATTEMPTS: usize = 3;

/// Grows with each retry, so a CDN that is briefly refusing isn't hammered.
const RETRY_BACKOFF: Duration = Duration::from_millis(500);

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

/// What a download stopped by [`Cancel`] fails with, so a caller can tell
/// the user's own cancel from a failure worth reporting.
#[derive(Debug, thiserror::Error)]
#[error("download cancelled")]
#[non_exhaustive]
pub struct Cancelled;

/// Set to stop an install. Checked between chunks and between reads, so a
/// cancel lands within one chunk's transfer rather than at the end of the
/// file.
#[derive(Clone, Default)]
pub(super) struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub(super) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(super) fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
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

/// Downloads every file into `staging`, at most [`CONCURRENT_FILES`] at a
/// time, reporting progress as it goes.
///
/// Returns the bytes written per file, in the order given. The first
/// failure fails the whole call: a partial install is not a smaller
/// install, it's a broken one.
pub(super) async fn download_all(
    files: Vec<PlannedDownload>,
    staging: &Path,
    cancel: Cancel,
    progress: mpsc::Sender<ProgressUpdate>,
) -> anyhow::Result<Vec<u64>> {
    let client = crate::achv_try!(client());
    let mut written = vec![0u64; files.len()];

    let mut results = futures_util::stream::iter(files.into_iter().enumerate().map(|(index, file)| {
        let destination = staging.join(&file.relative_path);
        let cancel = cancel.clone();
        let progress = progress.clone();
        async move {
            // A corrupt chunk already names its file, and is the message the
            // user should see rather than a generic one wrapped around it.
            let bytes = download_file(client, &file, &destination, &cancel, index, &progress)
                .await
                .map_err(|err| {
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
    // would drop the other in-flight futures mid-write, leaving half-
    // written files in staging that the caller is about to delete anyway
    // — but also cancelling them properly is what `cancel` is for.
    let mut failure = None;
    while let Some((index, bytes)) = results.next().await {
        match bytes {
            Ok(bytes) => written[index] = bytes,
            Err(err) => {
                tracing::error!(
                    file_index = index,
                    error = %err,
                    "file download failed, cancelling the rest of the install"
                );
                cancel.cancel();
                failure.get_or_insert(err);
            }
        }
    }

    match failure {
        Some(err) => Err(err),
        None => Ok(written),
    }
}

/// Fetches one file's chunks in order, decompressing and appending each.
async fn download_file(
    client: &reqwest::Client,
    file: &PlannedDownload,
    destination: &Path,
    cancel: &Cancel,
    index: usize,
    progress: &mpsc::Sender<ProgressUpdate>,
) -> anyhow::Result<u64> {
    if let Some(parent) = destination.parent() {
        crate::achv_try!(
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))
        );
    }

    // Truncating, not appending: a leftover file from an attempt that died
    // before staging could be cleared would otherwise be prefixed onto
    // this one.
    let mut sink = crate::achv_try!(
        tokio::fs::File::create(destination)
            .await
            .with_context(|| format!("creating {}", destination.display()))
    );

    let mut written = 0u64;
    for (chunk_index, chunk) in file.chunks.iter().enumerate() {
        crate::achv_try!(cancel.check());
        let bytes = crate::achv_try!(fetch_chunk(client, chunk, cancel).await.map_err(|err| {
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
            tokio::io::AsyncWriteExt::write_all(&mut sink, &bytes)
                .await
                .with_context(|| format!("writing {}", destination.display()))
        );
        written += bytes.len() as u64;
        // `trace!`, not `debug!`: same reasoning as `fetch_chunk_once` — a
        // file can be thousands of chunks, one line per chunk written.
        tracing::trace!(
            file_index = index,
            chunk_bytes = bytes.len(),
            file_bytes_written = written,
            "wrote chunk"
        );
        // A closed receiver means the caller stopped listening, which is
        // not a reason to stop downloading.
        let _ = progress.send((index, file.relative_path.clone(), written)).await;
    }

    crate::achv_try!(
        tokio::io::AsyncWriteExt::flush(&mut sink)
            .await
            .with_context(|| format!("flushing {}", destination.display()))
    );
    if file.is_executable
        && let Some(permissions) = platform::executable_permissions()
    {
        crate::achv_try!(
            tokio::fs::set_permissions(destination, permissions)
                .await
                .with_context(|| format!("making {} executable", destination.display()))
        );
    }
    tracing::debug!(
        relative_path = file.relative_path,
        bytes = written,
        chunk_count = file.chunks.len(),
        "file downloaded"
    );
    Ok(written)
}

/// One chunk, retried a few times, returned decompressed.
async fn fetch_chunk(client: &reqwest::Client, chunk: &ChunkSource, cancel: &Cancel) -> anyhow::Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 0..CHUNK_ATTEMPTS {
        crate::achv_try!(cancel.check());
        if attempt > 0 {
            tokio::time::sleep(RETRY_BACKOFF * attempt as u32).await;
        }
        match fetch_chunk_once(client, chunk, cancel).await {
            Ok(bytes) => return Ok(bytes),
            // A cancel isn't a transport failure and must not be retried.
            Err(err) if cancel.is_cancelled() => return Err(err),
            Err(err) => {
                // A retry that goes on to succeed is not a failure of the
                // install, so this is `warn!`, not `error!` — see the CDN
                // refusal test this path exists for.
                tracing::warn!(attempt = attempt + 1, max_attempts = CHUNK_ATTEMPTS, error = %err, "chunk attempt failed, retrying");
                last_error = Some(err);
            }
        }
    }
    let err = last_error.unwrap_or_else(|| anyhow::anyhow!("chunk download failed"));
    tracing::error!(attempts = CHUNK_ATTEMPTS, error = %err, "chunk download failed after exhausting retries");
    Err(err)
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
