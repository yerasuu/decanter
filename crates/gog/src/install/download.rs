//! Fetching a planned install's files from GOG's CDN.
//!
//! A GOG file is a list of chunks, each its own signed CDN URL and each
//! zlib-compressed; the file is those chunks decompressed and concatenated
//! in order. Nothing here is resumable, and deliberately so — an install
//! that fails or is cancelled throws its staging directory away
//! ([`super::staging`]), so a retry always starts from nothing and there
//! is no partial state to reconcile.
//!
//! Concurrency is per install rather than process-wide: two installs
//! running at once each get their own budget, which is worse for the disk
//! and much better for the user watching one of them finish.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
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

/// One chunk of a file: where to get it, and whether it needs inflating.
#[derive(Debug, Clone)]
pub(super) struct ChunkSource {
    pub(super) url: Url,
    pub(super) compressed: bool,
}

/// Set to stop an install. Checked between chunks and between reads, so a
/// cancel lands within one chunk's transfer rather than at the end of the
/// file.
#[derive(Clone, Default)]
pub(super) struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub(super) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn check(&self) -> anyhow::Result<()> {
        if self.is_cancelled() {
            bail!("install cancelled");
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
            let bytes = download_file(client, &file, &destination, &cancel, index, &progress)
                .await
                .with_context(|| format!("{} failed to download", file.relative_path));
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
    for chunk in &file.chunks {
        crate::achv_try!(cancel.check());
        let bytes = crate::achv_try!(fetch_chunk(client, chunk, cancel).await);
        crate::achv_try!(
            tokio::io::AsyncWriteExt::write_all(&mut sink, &bytes)
                .await
                .with_context(|| format!("writing {}", destination.display()))
        );
        written += bytes.len() as u64;
        // A closed receiver means the caller stopped listening, which is
        // not a reason to stop downloading.
        let _ = progress.send((index, file.relative_path.clone(), written)).await;
    }

    crate::achv_try!(
        tokio::io::AsyncWriteExt::flush(&mut sink)
            .await
            .with_context(|| format!("flushing {}", destination.display()))
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
                tracing::debug!("chunk attempt {} failed: {err}", attempt + 1);
                last_error = Some(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("chunk download failed")))
}

async fn fetch_chunk_once(client: &reqwest::Client, chunk: &ChunkSource, cancel: &Cancel) -> anyhow::Result<Vec<u8>> {
    let response = crate::achv_try!(client.get(chunk.url.clone()).send().await);
    let response = crate::achv_try!(response.error_for_status());

    // Streamed rather than `bytes()`, so a cancel lands mid-chunk instead
    // of waiting for the whole thing.
    let mut body = response.bytes_stream();
    let mut raw = Vec::new();
    while let Some(part) = body.next().await {
        crate::achv_try!(cancel.check());
        raw.extend_from_slice(&crate::achv_try!(part));
    }

    if !chunk.compressed {
        return Ok(raw);
    }
    inflate(&raw).context("chunk did not decompress")
}

/// GOG's depot chunks are raw zlib. Synchronous: a chunk is a few
/// megabytes at most, already fully in memory, and inflating it is far
/// cheaper than the transfer that produced it.
fn inflate(raw: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoder = flate2::read::ZlibDecoder::new(raw);
    let mut decompressed = Vec::new();
    crate::achv_try!(decoder.read_to_end(&mut decompressed));
    Ok(decompressed)
}

#[cfg(test)]
mod tests;
