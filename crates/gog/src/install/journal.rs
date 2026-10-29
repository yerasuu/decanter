//! The journal of one download, kept beside its bytes so a pause, a lost network or a crash
//! carries on from what is on disk instead of starting over:
//!
//! ```text
//! <root>/downloading/<account>/<product>/
//!   download.json        the plan (Header), replaced whole, once per plan
//!   progress.log         Records, each appended and synced after the bytes it names
//!   lock                 held while a process owns the download
//!   files/<path>.part    a file in flight
//!   files/<path>         a file complete, synced and renamed
//! ```
//!
//! Chunk URLs are not kept: they are signed and expire, so every resume plans again, and only a
//! plan identical to the journal's (same build, files and chunks) carries on.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use decanter_runtime::download::hex_digest;
use decanter_runtime::journal::{LineLog, LockError, StagingLock};
use decanter_runtime::sanitize_relative_path;
use serde::{Deserialize, Serialize};

use super::download::PlannedDownload;

const HEADER: &str = "download.json";
const LOG: &str = "progress.log";
const FILES: &str = "files";
const FORMAT: u32 = 1;

/// Bytes a file in flight writes between two records, and so the most a crash can cost it. It is
/// also what is hashed again after a crash: past it, the bytes were synced before a record named
/// them.
pub(super) const COMMIT_BYTES: u64 = 64 * 1024 * 1024;

/// A run this one takes over from lets go within one chunk; past this, another process owns it.
const LOCK_WAIT: Duration = Duration::from_secs(15);

/// Why a download stopped before finishing. Its data stays in staging until the user cancels it.
///
/// Exhaustive on purpose: the app words each reason, so a new one has to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    User,
    /// No answer from GOG for the whole retry window, or a refused chunk URL, which the next
    /// plan signs again.
    Network,
    /// decanter quit cleanly while it ran.
    AppClosed,
    /// decanter stopped without pausing it: a crash, a kill, a power cut.
    ClosedUnexpectedly,
    DiskFull,
    /// The library folder it downloads into went away (an unplugged disk).
    LibraryUnavailable,
}

/// What a download fails with when it stopped and kept what it had: resuming it carries on from
/// `bytes_done`.
#[derive(Debug, thiserror::Error)]
#[error("download paused ({reason:?}) with {bytes_done} bytes kept")]
#[non_exhaustive]
pub struct Paused {
    pub reason: PauseReason,
    pub bytes_done: u64,
    /// The library folder it downloads into, so the app can tell when an unplugged one is back.
    pub root: Option<PathBuf>,
}

/// How to plan this download again: the caller's own request, not what it resolved to.
#[derive(Serialize, Deserialize)]
pub(super) struct Request {
    pub(super) kind: Kind,
    pub(super) account_id: String,
    pub(super) product_id: String,
    pub(super) parent_id: Option<String>,
    pub(super) title: String,
    pub(super) branch: Option<String>,
    /// A pinned build (a rollback) stays pinned on resume.
    pub(super) build_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Install,
    Repair,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Header {
    format: u32,
    /// MD5 of `build_id` and `files`. The log's first record repeats it, so a log left by an
    /// earlier plan is never read against this one.
    plan: String,
    pub(super) request: Request,
    pub(super) build_id: String,
    /// GOG's own folder name for the build, which a DLC shares with its game.
    pub(super) install_directory: String,
    pub(super) total_bytes: u64,
    pub(super) files: Vec<HeaderFile>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct HeaderFile {
    pub(super) path: String,
    executable: bool,
    size: u64,
    chunks: Vec<HeaderChunk>,
}

#[derive(Serialize, Deserialize)]
struct HeaderChunk {
    md5: String,
    compressed_md5: String,
    size: usize,
}

impl Header {
    pub(super) fn new(
        request: Request,
        build_id: String,
        install_directory: String,
        downloads: &[PlannedDownload],
    ) -> anyhow::Result<Self> {
        let files: Vec<HeaderFile> = downloads
            .iter()
            .map(|download| HeaderFile {
                path: download.relative_path.clone(),
                executable: download.is_executable,
                size: download.size(),
                chunks: download
                    .chunks
                    .iter()
                    .map(|chunk| HeaderChunk {
                        md5: hex_digest(&chunk.md5),
                        compressed_md5: hex_digest(&chunk.compressed_md5),
                        size: chunk.size,
                    })
                    .collect(),
            })
            .collect();
        let plan = crate::achv_try!(serde_json::to_vec(&(&build_id, &files)).context("encoding the download plan"));
        Ok(Self {
            format: FORMAT,
            plan: hex_digest(&md5::compute(plan).0),
            request,
            build_id,
            install_directory,
            total_bytes: files.iter().map(|file| file.size).sum(),
            files,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Record {
    Begin {
        plan: String,
    },
    /// The first `chunks` chunks of file `file`, `bytes` long, are on disk and synced.
    Commit {
        file: usize,
        chunks: usize,
        bytes: u64,
    },
    Done {
        file: usize,
    },
    Paused {
        reason: PauseReason,
    },
    Resumed,
    /// Files are being moved into the library: a resume finishes the move, never downloads again.
    Moving,
    /// The library records the install; only removing staging was left.
    Recorded,
}

#[derive(Clone, Copy)]
enum FileProgress {
    Committed { chunks: usize, bytes: u64 },
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Downloading,
    Moving,
    Recorded,
}

pub(super) struct Folded {
    files: Vec<FileProgress>,
    pub(super) phase: Phase,
    /// Set when the last record is a pause: a clean stop, whose log is trusted as it is.
    pub(super) paused: Option<PauseReason>,
}

impl Folded {
    pub(super) fn bytes_done(&self, header: &Header) -> u64 {
        self.files
            .iter()
            .zip(&header.files)
            .map(|(progress, file)| match progress {
                FileProgress::Committed { bytes, .. } => *bytes,
                FileProgress::Done => file.size,
            })
            .sum()
    }
}

/// `None` when the log doesn't begin with this header's plan: then it describes no file here.
fn fold(header: &Header, lines: &[Vec<u8>]) -> Option<Folded> {
    let mut records = lines.iter().map(|line| serde_json::from_slice::<Record>(line));
    match records.next() {
        Some(Ok(Record::Begin { plan })) if plan == header.plan => {}
        _ => return None,
    }
    let mut folded = Folded {
        files: vec![FileProgress::Committed { chunks: 0, bytes: 0 }; header.files.len()],
        phase: Phase::Downloading,
        paused: None,
    };
    for record in records {
        // The CRC held, so this is a record of a newer format; what follows it can't be read
        // either.
        let Ok(record) = record else {
            tracing::warn!("unreadable record in a download log, ignoring the rest");
            break;
        };
        folded.paused = None;
        match record {
            Record::Commit { file, chunks, bytes } => {
                if let Some(slot) = folded.files.get_mut(file) {
                    *slot = FileProgress::Committed { chunks, bytes };
                }
            }
            Record::Done { file } => {
                if let Some(slot) = folded.files.get_mut(file) {
                    *slot = FileProgress::Done;
                }
            }
            Record::Paused { reason } => folded.paused = Some(reason),
            Record::Moving => folded.phase = Phase::Moving,
            Record::Recorded => folded.phase = Phase::Recorded,
            Record::Begin { .. } | Record::Resumed => {}
        }
    }
    Some(folded)
}

/// Where file `index` of a download starts.
#[derive(Clone, Copy)]
pub(super) enum Start {
    Done,
    At { chunk: usize, bytes: u64 },
}

pub(super) enum Opened {
    /// Fetch what `starts` says is missing, then move.
    Download {
        journal: Journal,
        header: Header,
        starts: Vec<Start>,
    },
    /// A crash hit the move into the library: it is finished with the plan it was moving, whatever
    /// GOG offers now, so an update is never left half applied.
    Move { journal: Journal, header: Header },
}

/// Why an existing journal was not carried on.
#[derive(Debug, thiserror::Error)]
enum Discarded {
    #[error("its plan header can't be read: {0}")]
    Unreadable(String),
    #[error("its progress log doesn't belong to its plan")]
    LogMismatch,
    #[error("GOG now plans build {to} instead of build {from}")]
    PlanChanged { from: String, to: String },
    #[error("it had already finished")]
    Finished,
}

/// An open journal: this process owns the download until it is dropped.
pub(super) struct Journal {
    dir: PathBuf,
    /// Up to four files append at once; each waits for its own record to be synced, and their
    /// lines must not interleave.
    log: tokio::sync::Mutex<LineLog>,
    lock: StagingLock,
}

impl Journal {
    pub(super) fn files(&self) -> PathBuf {
        self.dir.join(FILES)
    }

    /// Only once the file's first `chunks` chunks (`bytes`) are synced.
    pub(super) async fn commit(&self, file: usize, chunks: usize, bytes: u64) -> anyhow::Result<()> {
        self.append(&Record::Commit { file, chunks, bytes }).await
    }

    /// Only once the file is renamed to its final name and its folder synced.
    pub(super) async fn done(&self, file: usize) -> anyhow::Result<()> {
        self.append(&Record::Done { file }).await
    }

    pub(super) async fn paused(&self, reason: PauseReason) -> anyhow::Result<()> {
        self.append(&Record::Paused { reason }).await
    }

    pub(super) async fn moving(&self) -> anyhow::Result<()> {
        self.append(&Record::Moving).await
    }

    pub(super) async fn recorded(&self) -> anyhow::Result<()> {
        self.append(&Record::Recorded).await
    }

    async fn append(&self, record: &Record) -> anyhow::Result<()> {
        let line = crate::achv_try!(serde_json::to_vec(record).context("encoding a download log record"));
        self.log
            .lock()
            .await
            .append(&line)
            .await
            .with_context(|| format!("writing {}", self.dir.join(LOG).display()))
    }

    /// Deletes the whole download: the end of a confirmed cancel, or of a finished install.
    pub(super) async fn remove(self) -> io::Result<()> {
        let Self { dir, log, lock } = self;
        drop(log);
        // Unlocked first, since Windows can't delete a file that is still open.
        drop(lock);
        remove_dir(&dir).await
    }
}

async fn remove_dir(dir: &Path) -> io::Result<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        Ok(()) | Err(_) => Ok(()),
    }
}

async fn remove_file(path: &Path) -> io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        Ok(()) | Err(_) => Ok(()),
    }
}

pub(super) fn part_path(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// Takes `staging` for this download: carries on the journal there when it belongs to the same
/// plan, finishes its move when a crash hit that, and otherwise starts the download over.
///
/// # Errors
/// [`LockError::Busy`] when another process owns the download, or the I/O error that stopped it.
pub(super) async fn open(staging: &Path, header: Header, downloads: &[PlannedDownload]) -> anyhow::Result<Opened> {
    crate::achv_try!(
        tokio::fs::create_dir_all(staging)
            .await
            .with_context(|| format!("creating {}", staging.display()))
    );
    let lock = crate::achv_try!(lock_waiting(staging).await);
    let discarded = match read_header(staging).await {
        Ok(None) => None,
        Err(discarded) => Some(discarded),
        Ok(Some(previous)) => match crate::achv_try!(carry_on(staging, previous, &header, downloads).await) {
            CarryOn::Download { log, starts } => {
                return Ok(Opened::Download {
                    journal: Journal::new(staging, log, lock),
                    header,
                    starts,
                });
            }
            CarryOn::Move { log, previous } => {
                return Ok(Opened::Move {
                    journal: Journal::new(staging, log, lock),
                    header: *previous,
                });
            }
            CarryOn::Over(discarded) => Some(discarded),
        },
    };
    start_over(staging, header, lock, discarded).await
}

impl Journal {
    fn new(staging: &Path, log: LineLog, lock: StagingLock) -> Self {
        Self {
            dir: staging.to_path_buf(),
            log: tokio::sync::Mutex::new(log),
            lock,
        }
    }
}

enum CarryOn {
    Download { log: LineLog, starts: Vec<Start> },
    Move { log: LineLog, previous: Box<Header> },
    Over(Discarded),
}

async fn carry_on(
    staging: &Path,
    previous: Header,
    header: &Header,
    downloads: &[PlannedDownload],
) -> anyhow::Result<CarryOn> {
    let (mut log, lines) = match LineLog::open(&staging.join(LOG)).await {
        Ok(opened) => opened,
        // The header is written before its log: a crash between the two leaves no log.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(CarryOn::Over(Discarded::LogMismatch)),
        Err(err) => return Err(anyhow::Error::new(err).context("reading the download log")),
    };
    let Some(folded) = fold(&previous, &lines) else {
        return Ok(CarryOn::Over(Discarded::LogMismatch));
    };
    match folded.phase {
        Phase::Moving => {
            tracing::warn!(
                build_id = previous.build_id,
                "finishing a move into the library a crash interrupted"
            );
            Ok(CarryOn::Move {
                log,
                previous: Box::new(previous),
            })
        }
        Phase::Recorded => Ok(CarryOn::Over(Discarded::Finished)),
        Phase::Downloading if previous.plan != header.plan => Ok(CarryOn::Over(Discarded::PlanChanged {
            from: previous.build_id,
            to: header.build_id.clone(),
        })),
        Phase::Downloading => {
            let starts = crate::achv_try!(reconcile(staging.join(FILES), downloads, &folded).await);
            let resumed =
                crate::achv_try!(serde_json::to_vec(&Record::Resumed).context("encoding a download log record"));
            crate::achv_try!(log.append(&resumed).await.context("writing the download log"));
            tracing::info!(
                bytes_done = folded.bytes_done(&previous),
                clean = folded.paused.is_some(),
                "resuming a download from its journal"
            );
            Ok(CarryOn::Download { log, starts })
        }
    }
}

/// Clears `staging` but for its lock and writes a fresh header and log.
async fn start_over(
    staging: &Path,
    header: Header,
    lock: StagingLock,
    discarded: Option<Discarded>,
) -> anyhow::Result<Opened> {
    if let Some(reason) = discarded {
        tracing::warn!(staging = %staging.display(), %reason, "starting the download over");
    }
    crate::achv_try!(clear(staging).await);

    let encoded = crate::achv_try!(serde_json::to_vec(&header).context("encoding the download plan"));
    crate::achv_try!(
        decanter_runtime::journal::write_atomic(&staging.join(HEADER), &encoded)
            .await
            .context("writing the download plan")
    );
    crate::achv_try!(
        tokio::fs::create_dir_all(staging.join(FILES))
            .await
            .context("creating the download folder")
    );
    let begin = crate::achv_try!(
        serde_json::to_vec(&Record::Begin {
            plan: header.plan.clone()
        })
        .context("encoding a download log record")
    );
    let log = crate::achv_try!(
        LineLog::create(&staging.join(LOG), &begin)
            .await
            .context("creating the download log")
    );
    let starts = vec![Start::At { chunk: 0, bytes: 0 }; header.files.len()];
    Ok(Opened::Download {
        journal: Journal::new(staging, log, lock),
        header,
        starts,
    })
}

/// Empties `staging` but for its lock, which the caller holds. The log goes
/// first, so no header is ever read against a log that outlived its plan.
async fn clear(staging: &Path) -> anyhow::Result<()> {
    crate::achv_try!(
        remove_file(&staging.join(LOG))
            .await
            .context("clearing the old download log")
    );
    let mut entries = crate::achv_try!(
        tokio::fs::read_dir(staging)
            .await
            .with_context(|| format!("reading {}", staging.display()))
    );
    while let Some(entry) = crate::achv_try!(entries.next_entry().await) {
        if entry.file_name() == "lock" {
            continue;
        }
        let path = entry.path();
        let removed = if crate::achv_try!(entry.file_type().await).is_dir() {
            remove_dir(&path).await
        } else {
            remove_file(&path).await
        };
        crate::achv_try!(removed.with_context(|| format!("clearing {}", path.display())));
    }
    Ok(())
}

/// Deletes the download in `staging`, which nothing in this process runs:
/// the end of a confirmed cancel of a paused one.
///
/// # Errors
/// [`LockError::Busy`] when another process runs it, or what stopped the removal.
pub(super) async fn discard(staging: &Path) -> anyhow::Result<()> {
    let lock = crate::achv_try!(decanter_runtime::journal::lock(staging));
    crate::achv_try!(clear(staging).await);
    drop(lock);
    crate::achv_try!(
        remove_dir(staging)
            .await
            .with_context(|| format!("removing {}", staging.display()))
    );
    Ok(())
}

async fn lock_waiting(staging: &Path) -> Result<StagingLock, LockError> {
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match decanter_runtime::journal::lock(staging) {
            Err(LockError::Busy(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            locked => return locked,
        }
    }
}

/// `Ok(None)` when there is no journal (a staging folder from before journals, or none at all).
async fn read_header(staging: &Path) -> Result<Option<Header>, Discarded> {
    let bytes = match tokio::fs::read(staging.join(HEADER)).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Discarded::Unreadable(err.to_string())),
    };
    let header: Header = match serde_json::from_slice(&bytes) {
        Ok(header) => header,
        Err(err) => return Err(Discarded::Unreadable(err.to_string())),
    };
    if header.format != FORMAT {
        return Err(Discarded::Unreadable(format!("format {}", header.format)));
    }
    // Checked where they come back from disk: finishing a move joins them onto the library.
    let is_safe = |path: &str| sanitize_relative_path(path).is_some_and(|safe| safe.as_os_str() == path);
    if let Some(file) = header.files.iter().find(|file| !is_safe(&file.path)) {
        return Err(Discarded::Unreadable(format!("unsafe path {:?}", file.path)));
    }
    Ok(Some(header))
}

/// What a journal says without taking it, for listing downloads left unfinished.
pub(super) struct Found {
    pub(super) header: Header,
    /// `None` when the log can't be read against the header: nothing of it would be kept.
    pub(super) folded: Option<Folded>,
    /// Another process owns it right now.
    pub(super) in_use: bool,
}

pub(super) async fn inspect(staging: &Path) -> Option<Found> {
    let header = match read_header(staging).await {
        Ok(Some(header)) => header,
        Ok(None) => return None,
        Err(reason) => {
            tracing::warn!(staging = %staging.display(), %reason, "skipping an unreadable download journal");
            return None;
        }
    };
    let in_use = matches!(decanter_runtime::journal::lock(staging), Err(LockError::Busy(_)));
    let folded = decanter_runtime::journal::read_records(&staging.join(LOG))
        .await
        .ok()
        .and_then(|lines| fold(&header, &lines));
    Some(Found { header, folded, in_use })
}

/// When the journal last recorded anything: for a paused download, when it paused.
pub(super) async fn last_written(staging: &Path) -> Option<std::time::SystemTime> {
    let written = match tokio::fs::metadata(staging.join(LOG)).await {
        Ok(meta) => Ok(meta),
        // The header is written before its log: a crash between the two leaves no log.
        Err(_) => tokio::fs::metadata(staging.join(HEADER)).await,
    };
    written.and_then(|meta| meta.modified()).ok()
}

/// What a resume needs to know of each file, owned so the check runs off the async workers.
struct Shape {
    path: PathBuf,
    /// Each chunk's inflated size and MD5.
    chunks: Vec<(u64, [u8; 16])>,
}

fn len64(size: usize) -> u64 {
    u64::try_from(size).unwrap_or(u64::MAX)
}

async fn reconcile(files: PathBuf, downloads: &[PlannedDownload], folded: &Folded) -> anyhow::Result<Vec<Start>> {
    let shapes: Vec<Shape> = downloads
        .iter()
        .map(|download| Shape {
            path: PathBuf::from(&download.relative_path),
            chunks: download
                .chunks
                .iter()
                .map(|chunk| (len64(chunk.size), chunk.md5))
                .collect(),
        })
        .collect();
    let progress = folded.files.clone();
    let clean = folded.paused.is_some();
    let checked = tokio::task::spawn_blocking(move || {
        shapes
            .iter()
            .zip(progress)
            .map(|(shape, progress)| reconcile_file(&files, shape, progress, clean))
            .collect::<io::Result<Vec<Start>>>()
    })
    .await;
    match checked {
        Ok(starts) => starts.context("checking the files already downloaded"),
        Err(err) => Err(anyhow::Error::new(err).context("download check worker panicked")),
    }
}

/// Brings one file back to what its log vouches for. Bytes are only ever cut, never assumed: a
/// file shorter than its record lost data a sync promised (a disk that lies, an unsafe unplug) and
/// starts over, and after an unclean exit the last window is hashed again.
fn reconcile_file(files: &Path, shape: &Shape, progress: FileProgress, clean: bool) -> io::Result<Start> {
    let complete = files.join(&shape.path);
    let part = part_path(&complete);
    let (chunks, bytes) = match progress {
        FileProgress::Done => {
            let size: u64 = shape.chunks.iter().map(|&(size, _)| size).sum();
            if std::fs::metadata(&complete).is_ok_and(|meta| meta.len() == size) {
                return Ok(Start::Done);
            }
            tracing::warn!(path = %shape.path.display(), "a finished file is missing or the wrong size, fetching it again");
            match std::fs::remove_file(&complete) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                Ok(()) | Err(_) => (0, 0),
            }
        }
        FileProgress::Committed { chunks, bytes } => (chunks, bytes),
    };
    // A crash between a file's rename and its record leaves it complete with no `Done`.
    if complete.is_file() {
        crate::achv_try!(std::fs::rename(&complete, &part));
    }
    let on_disk = match std::fs::metadata(&part) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
        Err(err) => return Err(err),
    };
    let named = shape
        .chunks
        .get(..chunks)
        .map(|named| named.iter().map(|&(size, _)| size).sum::<u64>());
    let (chunks, bytes) = if named == Some(bytes) && on_disk >= bytes {
        (chunks, bytes)
    } else {
        tracing::warn!(
            path = %shape.path.display(),
            on_disk,
            committed = bytes,
            "a file in flight doesn't hold what its log says, fetching it again"
        );
        (0, 0)
    };
    let (chunks, bytes) = if clean || chunks == 0 {
        (chunks, bytes)
    } else {
        crate::achv_try!(rehash_window(&part, &shape.chunks, chunks, bytes))
    };
    if on_disk > 0 {
        let file = crate::achv_try!(std::fs::OpenOptions::new().write(true).open(&part));
        crate::achv_try!(file.set_len(bytes));
    }
    Ok(Start::At { chunk: chunks, bytes })
}

/// Hashes the last committed chunks of `part` (at most [`COMMIT_BYTES`], at least one) and cuts
/// back to the first that doesn't match.
fn rehash_window(part: &Path, chunks: &[(u64, [u8; 16])], count: usize, bytes: u64) -> io::Result<(usize, u64)> {
    let mut first = count;
    let mut start = bytes;
    while let Some(&(size, _)) = first.checked_sub(1).and_then(|index| chunks.get(index)) {
        if first < count && bytes.saturating_sub(start).saturating_add(size) > COMMIT_BYTES {
            break;
        }
        first -= 1;
        start = start.saturating_sub(size);
    }
    let mut file = crate::achv_try!(std::fs::File::open(part));
    crate::achv_try!(std::io::Seek::seek(&mut file, io::SeekFrom::Start(start)));
    let mut offset = start;
    for (index, &(size, md5)) in chunks.iter().enumerate().take(count).skip(first) {
        let mut buffer = vec![0; usize::try_from(size).unwrap_or(usize::MAX)];
        let matches = match file.read_exact(&mut buffer) {
            Ok(()) => md5::compute(&buffer).0 == md5,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => false,
            Err(err) => return Err(err),
        };
        if !matches {
            tracing::warn!(part = %part.display(), chunk = index, "a committed chunk didn't survive the crash, fetching from it again");
            return Ok((index, offset));
        }
        offset += size;
    }
    tracing::debug!(part = %part.display(), checked_bytes = bytes - start, "re-hashed the last window after an unclean exit");
    Ok((count, bytes))
}
