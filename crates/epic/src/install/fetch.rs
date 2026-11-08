// SPDX-License-Identifier: GPL-3.0-or-later
//! Fetching a game's chunks and assembling its files in staging (spec, 7.4).
//!
//! Each file to fetch gets a `<path>.part` of its full size; each chunk is downloaded once, checked
//! (see `chunk`), and its parts written at their place in every file that uses them; the log
//! records the chunk then. A file whose last part landed is hashed whole and, once its SHA-1 is the
//! manifest's, renamed into place and recorded. A resumed run skips the chunks the log records for
//! the parts it kept; a part file a crash left short of a write it had logged fails its SHA-1 and is
//! fetched again, a bounded number of times.
//!
//! legendary's retry loop never advances its counter, so a failing chunk is retried forever (spec,
//! 7.4). Here each chunk gets [`ATTEMPTS`] tries, the first retry at once and then an exponential
//! back-off with jitter, across the CDNs in turn; then, once, a pause and as many again; then the
//! download pauses for the network, to be carried on later with a fresh manifest answer.

use core::time::Duration;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::{self, Seek as _, Write as _};
use std::path::PathBuf;

use decanter_runtime::install::stop::StopFlag;
use decanter_runtime::journal::LineLog;
use futures_util::StreamExt as _;
use url::Url;

use super::chunk::{self, Fault, Verified, Want};
use super::disk;
use super::distribution::{self, Fetched};
use crate::manifest::{Guid, MAX_CHUNK_FILE, Manifest, ManifestFile};

/// Spec 13.11 starts at eight parallel chunk downloads, against legendary's twice the CPU count.
const CONCURRENT_CHUNKS: usize = 8;
/// Tries of one chunk in one round, as many as legendary declares (spec, 7.4).
const ATTEMPTS: u32 = 7;
/// Rounds of [`ATTEMPTS`]: the second is the one requeue spec 7.4 recommends.
const ROUNDS: u32 = 2;
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// The longest `Retry-After` a 429 is waited for (spec, 13.11).
const MAX_RETRY_AFTER: Duration = Duration::from_mins(1);
const CHUNK_TIMEOUT: Duration = Duration::from_mins(2);
/// Copies of one chunk that failed their checks before the download fails for it.
const MAX_CORRUPT_TRIES: u32 = 3;
/// Assemblies of one file that failed its SHA-1 before the download fails for it.
const MAX_FILE_TRIES: u32 = 3;

/// One file to fetch, and where its part and its finished copy go.
pub(super) struct Job<'manifest> {
    pub(super) file: &'manifest ManifestFile,
    pub(super) done: PathBuf,
    /// Its part file is new, so every chunk it uses is written to it whatever the log says.
    pub(super) is_fresh: bool,
}

impl Job<'_> {
    pub(super) fn part(&self) -> PathBuf {
        decanter_runtime::install::files::part_path(&self.done)
    }
}

/// Why fetching the files stopped.
#[derive(Debug)]
pub(super) enum Failure {
    /// The stop flag was set; the caller reads why.
    Stopped,
    /// No CDN gave a chunk through every try.
    Unreachable,
    DiskFull,
    /// A chunk kept failing its checks, or a file its SHA-1; names which.
    Corrupt(String),
    Encrypted,
    Io {
        path: String,
        source: io::Error,
    },
}

/// What the files of one run share.
pub(super) struct Run<'run> {
    pub(super) manifest: &'run Manifest,
    pub(super) bases: &'run [String],
    pub(super) flag: &'run StopFlag,
    pub(super) log: LineLog,
    /// Chunks the log records as written into the parts it kept.
    pub(super) applied: HashSet<Guid>,
}

/// Where one part of a chunk goes.
struct Target {
    job: usize,
    file_offset: u64,
    chunk_offset: u32,
    size: u32,
}

/// The chunks one pass fetches, in the order their files first need them.
struct Pass {
    order: Vec<usize>,
    targets: HashMap<usize, Vec<Target>>,
    /// Parts each job still waits for.
    remaining: HashMap<usize, usize>,
    /// Bytes of the pass' jobs that the log says are already written.
    already: u64,
}

impl Pass {
    fn plan(run: &Run<'_>, jobs: &[Job<'_>], pending: &[usize]) -> Self {
        let mut pass = Self {
            order: Vec::new(),
            targets: HashMap::new(),
            remaining: HashMap::with_capacity(pending.len()),
            already: 0,
        };
        for &index in pending {
            let Some(job) = jobs.get(index) else {
                continue;
            };
            let mut file_offset = 0_u64;
            let mut left = 0_usize;
            for part in job.file.parts() {
                let is_written = run
                    .manifest
                    .chunks()
                    .get(part.chunk)
                    .is_some_and(|chunk| run.applied.contains(&chunk.guid));
                if job.is_fresh || !is_written {
                    let order = &mut pass.order;
                    pass.targets
                        .entry(part.chunk)
                        .or_insert_with(|| {
                            order.push(part.chunk);
                            Vec::new()
                        })
                        .push(Target {
                            job: index,
                            file_offset,
                            chunk_offset: part.offset,
                            size: part.size,
                        });
                    left = left.saturating_add(1);
                } else {
                    pass.already = pass.already.saturating_add(u64::from(part.size));
                }
                file_offset = file_offset.saturating_add(u64::from(part.size));
            }
            pass.remaining.insert(index, left);
        }
        pass
    }
}

/// Fetches every job. `written` is what staging already holds of the download; `tick` is given the
/// download's bytes on disk and the file most recently written.
pub(super) async fn fetch_all(
    run: &mut Run<'_>,
    jobs: &mut [Job<'_>],
    staged: u64,
    mut tick: impl FnMut(u64, &str),
) -> Result<(), Failure> {
    let client = distribution::cdn_client().map_err(|err| {
        tracing::error!(error = %err, "no epic download client");
        Failure::Unreachable
    })?;
    let mut pending: Vec<usize> = (0..jobs.len()).collect();
    let mut tries = vec![0_u32; jobs.len()];
    let mut written = staged;
    let mut rolling_off = 0_usize;
    loop {
        prepare_parts(jobs, &pending).await?;
        let pass = Pass::plan(run, jobs, &pending);
        written = written.saturating_add(pass.already);
        tracing::debug!(
            files = pending.len(),
            chunks = pass.order.len(),
            already = pass.already,
            "epic download pass planned"
        );
        let outcome = run_pass(&client, run, jobs, pass, &mut written, &mut rolling_off, &mut tick).await;
        let failed = match outcome {
            Ok(failed) => failed,
            Err(err) => {
                run.flag.abort();
                return Err(err);
            }
        };
        if rolling_off > 0 {
            tracing::warn!(
                chunks = rolling_off,
                "epic chunks passed their SHA-1 but not the rolling hash as decanter computes it (spec Q15)"
            );
            rolling_off = 0;
        }
        if failed.is_empty() {
            tick(written, "");
            return Ok(());
        }
        for &index in &failed {
            let (Some(job), Some(tried)) = (jobs.get_mut(index), tries.get_mut(index)) else {
                continue;
            };
            *tried = tried.saturating_add(1);
            let path = job.file.path().as_str();
            tracing::warn!(
                path,
                tries = *tried,
                "an epic file came out with the wrong SHA-1, assembling it again"
            );
            if *tried >= MAX_FILE_TRIES {
                return Err(Failure::Corrupt(path.to_owned()));
            }
            job.is_fresh = true;
            written = written.saturating_sub(job.file.size());
        }
        pending = failed;
    }
}

/// Gives every pending job a part file of its size, keeping one that has it unless the job is
/// fresh; one created here makes the job fresh.
async fn prepare_parts(jobs: &mut [Job<'_>], pending: &[usize]) -> Result<(), Failure> {
    let wanted: Vec<(usize, PathBuf, u64, bool)> = pending
        .iter()
        .filter_map(|&index| {
            let job = jobs.get(index)?;
            Some((index, job.part(), job.file.size(), job.is_fresh))
        })
        .collect();
    let created = tokio::task::spawn_blocking(move || {
        let mut created = Vec::new();
        for (index, part, size, is_fresh) in wanted {
            if !is_fresh && disk::has_size(&part, size).map_err(|err| (index, err))? {
                continue;
            }
            if let Some(parent) = part.parent() {
                std::fs::create_dir_all(parent).map_err(|err| (index, err))?;
            }
            let file = std::fs::File::create(&part).map_err(|err| (index, err))?;
            file.set_len(size).map_err(|err| (index, err))?;
            created.push(index);
        }
        Ok::<_, (usize, io::Error)>(created)
    })
    .await
    .map_err(|err| Failure::Io {
        path: String::new(),
        source: io::Error::other(err),
    })?;
    match created {
        Ok(created) => {
            for index in created {
                if let Some(job) = jobs.get_mut(index) {
                    job.is_fresh = true;
                }
            }
            Ok(())
        }
        Err((index, source)) => Err(io_failure(jobs.get(index).map(|job| job.file), source)),
    }
}

async fn run_pass(
    client: &reqwest::Client,
    run: &mut Run<'_>,
    jobs: &[Job<'_>],
    mut pass: Pass,
    written: &mut u64,
    rolling_off: &mut usize,
    tick: &mut impl FnMut(u64, &str),
) -> Result<Vec<usize>, Failure> {
    let mut failed = Vec::new();
    let ready: Vec<usize> = pass
        .remaining
        .iter()
        .filter(|&(_, &left)| left == 0)
        .map(|(&index, _)| index)
        .collect();
    for index in ready {
        if !complete(run, jobs, index).await? {
            failed.push(index);
        }
    }
    let (manifest, bases, flag) = (run.manifest, run.bases, run.flag);
    let feature_level = manifest.feature_level();
    let downloads = futures_util::stream::iter(pass.order.clone())
        .map(|chunk_index| {
            let Some(chunk) = manifest.chunks().get(chunk_index) else {
                return tokio::spawn(async { Err(Failure::Corrupt("a chunk index past the list".to_owned())) });
            };
            let path = chunk::path(feature_level, chunk);
            let urls: Vec<Url> = bases
                .iter()
                .filter_map(|base| Url::parse(&format!("{base}/{path}")).ok())
                .collect();
            tokio::spawn(download(client.clone(), urls, Want::of(chunk), flag.clone()))
        })
        .buffered(CONCURRENT_CHUNKS);
    tokio::pin!(downloads);
    let mut ticker = tokio::time::interval(Duration::from_millis(150));
    let mut current = String::new();
    for chunk_index in pass.order.clone() {
        let verified = loop {
            tokio::select! {
                next = downloads.next() => break next,
                _ = ticker.tick() => tick(*written, &current),
            }
        };
        let verified = match verified {
            Some(Ok(Ok(verified))) => verified,
            Some(Ok(Err(err))) => return Err(err),
            Some(Err(err)) => {
                return Err(Failure::Io {
                    path: String::new(),
                    source: io::Error::other(err),
                });
            }
            None => break,
        };
        if verified.is_rolling_off {
            *rolling_off = rolling_off.saturating_add(1);
        }
        let targets = pass.targets.remove(&chunk_index).unwrap_or_default();
        let bytes = write_chunk(jobs, verified, &targets).await?;
        *written = written.saturating_add(bytes);
        if let Some(guid) = run.manifest.chunks().get(chunk_index).map(|chunk| chunk.guid) {
            run.log
                .append(format!("chunk {guid}").as_bytes())
                .await
                .map_err(|source| io_failure(None, source))?;
            run.applied.insert(guid);
        }
        for target in &targets {
            let Some(left) = pass.remaining.get_mut(&target.job) else {
                continue;
            };
            *left = left.saturating_sub(1);
            if *left > 0 {
                continue;
            }
            if let Some(job) = jobs.get(target.job) {
                job.file.path().as_str().clone_into(&mut current);
            }
            if !complete(run, jobs, target.job).await? {
                failed.push(target.job);
            }
        }
        tick(*written, &current);
    }
    Ok(failed)
}

/// Writes a chunk's parts into their files, returning the bytes written.
async fn write_chunk(jobs: &[Job<'_>], verified: Verified, targets: &[Target]) -> Result<u64, Failure> {
    let writes: Vec<(usize, PathBuf, u64, usize, usize)> = targets
        .iter()
        .filter_map(|target| {
            let job = jobs.get(target.job)?;
            let start = usize::try_from(target.chunk_offset).ok()?;
            let end = start.checked_add(usize::try_from(target.size).ok()?)?;
            Some((target.job, job.part(), target.file_offset, start, end))
        })
        .collect();
    let data = verified.data;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut bytes = 0_u64;
        for (index, part, file_offset, start, end) in writes {
            let write = || -> io::Result<usize> {
                let slice = data
                    .get(start..end)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a part reads past its chunk"))?;
                let mut file = std::fs::OpenOptions::new().write(true).open(&part)?;
                file.seek(io::SeekFrom::Start(file_offset))?;
                file.write_all(slice)?;
                Ok(slice.len())
            };
            let len = write().map_err(|err| (index, err))?;
            bytes = bytes.saturating_add(u64::try_from(len).unwrap_or(u64::MAX));
        }
        Ok::<_, (usize, io::Error)>(bytes)
    })
    .await
    .map_err(|err| Failure::Io {
        path: String::new(),
        source: io::Error::other(err),
    })?;
    outcome.map_err(|(index, source)| io_failure(jobs.get(index).map(|job| job.file), source))
}

/// Hashes a job's assembled part and, when its SHA-1 is right, renames it into place and records
/// it, in that order: a record never names a file a crash could leave as a part. `false` when the
/// SHA-1 is wrong, which leaves the part to be assembled again.
async fn complete(run: &mut Run<'_>, jobs: &[Job<'_>], index: usize) -> Result<bool, Failure> {
    let Some(job) = jobs.get(index) else {
        return Ok(true);
    };
    let part = job.part();
    let hashed = tokio::task::spawn_blocking(move || disk::sha1_of(&part))
        .await
        .map_err(|err| io_failure(Some(job.file), io::Error::other(err)))?
        .map_err(|source| io_failure(Some(job.file), source))?;
    if hashed != *job.file.sha1() {
        return Ok(false);
    }
    tokio::fs::rename(job.part(), &job.done)
        .await
        .map_err(|source| io_failure(Some(job.file), source))?;
    if let Some(parent) = job.done.parent() {
        decanter_runtime::journal::sync_dir(parent)
            .await
            .map_err(|source| io_failure(Some(job.file), source))?;
    }
    run.log
        .append(done_record(job.file).as_bytes())
        .await
        .map_err(|source| io_failure(Some(job.file), source))?;
    Ok(true)
}

fn io_failure(file: Option<&ManifestFile>, source: io::Error) -> Failure {
    if source.kind() == io::ErrorKind::StorageFull {
        return Failure::DiskFull;
    }
    Failure::Io {
        path: file.map(|file| file.path().as_str().to_owned()).unwrap_or_default(),
        source,
    }
}

/// A finished file in the log: its SHA-1 and its path, so a later plan of another build keeps it
/// only when it still lists that file with that content.
pub(super) fn done_record(file: &ManifestFile) -> String {
    format!("done {} {}", file.sha1(), file.path().as_str())
}

/// A chunk the log records as written: its GUID in the 32 hex digits of [`Guid`]'s `Display`.
pub(super) fn applied_chunk(record: &str) -> Option<Guid> {
    let text = record.strip_prefix("chunk ")?;
    if text.len() != 32 {
        return None;
    }
    let mut words = [0_u32; 4];
    for (index, word) in words.iter_mut().enumerate() {
        *word = u32::from_str_radix(text.get(index * 8..index * 8 + 8)?, 16).ok()?;
    }
    Some(Guid(words))
}

/// One chunk, through the bounded retry policy of the module's doc.
async fn download(client: reqwest::Client, urls: Vec<Url>, want: Want, flag: StopFlag) -> Result<Verified, Failure> {
    if urls.is_empty() {
        return Err(Failure::Unreachable);
    }
    let mut corrupt = 0_u32;
    let mut tried = 0_usize;
    let mut last = String::new();
    for round in 0..ROUNDS {
        for attempt in 0..ATTEMPTS {
            if flag.requested().is_some() {
                return Err(Failure::Stopped);
            }
            let Some(url) = urls.get(tried % urls.len()) else {
                return Err(Failure::Unreachable);
            };
            tried = tried.saturating_add(1);
            let host = url.host_str().unwrap_or_default();
            let fetched = tokio::select! {
                fetched = distribution::fetch_capped(&client, url, MAX_CHUNK_FILE, CHUNK_TIMEOUT) => fetched,
                () = flag.stopped() => return Err(Failure::Stopped),
            };
            let mut wait = backoff(attempt);
            match fetched {
                Ok(raw) => match chunk::decode(&raw, &want) {
                    Ok(verified) => return Ok(verified),
                    Err(Fault::Encrypted) => return Err(Failure::Encrypted),
                    Err(fault) => {
                        corrupt = corrupt.saturating_add(1);
                        tracing::warn!(chunk = %want.guid, host, %fault, tries = corrupt, "an epic chunk failed its checks");
                        if corrupt >= MAX_CORRUPT_TRIES {
                            return Err(Failure::Corrupt(format!("chunk {}", want.guid)));
                        }
                    }
                },
                Err(Fetched::RateLimited(asked)) => {
                    wait = asked.unwrap_or(wait).max(wait).min(MAX_RETRY_AFTER);
                    tracing::info!(host, wait_ms = wait.as_millis(), "the epic CDN rate limited a chunk");
                }
                Err(err) => {
                    last = format!("{host}: {err}");
                    tracing::debug!(chunk = %want.guid, host, round, attempt, error = %err, "epic chunk transfer failed");
                }
            }
            let is_last = round + 1 == ROUNDS && attempt + 1 == ATTEMPTS;
            if !is_last {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = flag.stopped() => return Err(Failure::Stopped),
                }
            }
        }
        if round + 1 < ROUNDS {
            tracing::info!(chunk = %want.guid, last, "an epic chunk failed every try of a round, requeued once");
            tokio::select! {
                () = tokio::time::sleep(MAX_BACKOFF) => {}
                () = flag.stopped() => return Err(Failure::Stopped),
            }
        }
    }
    tracing::warn!(chunk = %want.guid, tries = tried, last, "an epic chunk failed every try, the download pauses");
    Err(Failure::Unreachable)
}

/// The wait after try `attempt` failed: none after the first, so the first retry is immediate, then
/// [`FIRST_BACKOFF`] doubling up to [`MAX_BACKOFF`], each with up to half again of jitter so that
/// parallel chunks do not retry in step.
fn backoff(attempt: u32) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let base = FIRST_BACKOFF
        .saturating_mul(1_u32.checked_shl(attempt.saturating_sub(1)).unwrap_or(u32::MAX))
        .min(MAX_BACKOFF);
    let millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    let jitter = RandomState::new().build_hasher().finish() % millis.div_ceil(2).max(1);
    Duration::from_millis(millis.saturating_add(jitter))
}
