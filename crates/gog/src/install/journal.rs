//! The journal of one download, kept beside its bytes so a pause, a lost network or a crash
//! carries on from what is on disk instead of starting over:
//!
//! ```text
//! <root>/downloading/<account>/<product>/
//!   download.json        the plan (Header), replaced whole, once per plan
//!   progress.log         Records, each appended and synced after the bytes it names
//!   lock                 held while a process owns the download
//!   pending.json         the request, while it waits for a plan or stopped before one
//!   files/<path>.part    a file in flight
//!   files/<path>         a file complete, synced and renamed
//! ```
//!
//! Chunk URLs are not kept: they are signed and expire, so every resume plans again. A plan identical
//! to the journal's (same build, files and chunks) carries on as it was; one for another build keeps,
//! per file, only the leading chunks both builds share.

use std::collections::HashMap;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use decanter_runtime::download::hex_digest;
use decanter_runtime::journal::{LineLog, LockError, StagingLock};
use decanter_runtime::sanitize_relative_path;
use serde::{Deserialize, Serialize};

use super::download::PlannedDownload;
use super::plan::parse_md5;

const HEADER: &str = "download.json";
const LOG: &str = "progress.log";
/// A new build's log, written whole before its header replaces the old one and renamed into place
/// after: a crash between the two leaves it here for the next open to finish the swap.
const LOG_ASIDE: &str = "progress.log.new";
const FILES: &str = "files";
const FORMAT: u32 = 1;

/// Bytes a file in flight writes between two records, and so the most a resume has to check chunk
/// by chunk after a crash before it keeps them. It is also what is hashed again after a crash: past
/// it, the bytes were synced before a record named them.
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

/// The free space a download needed on its library's disk, against what that disk had, when it
/// stopped for it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Room {
    /// What was left to download, plus a margin so the disk is never filled to the last byte.
    pub needed: u64,
    pub free: u64,
}

impl Room {
    pub(super) const fn new(needed: u64, free: u64) -> Self {
        Self { needed, free }
    }

    /// How much more has to be freed.
    #[must_use]
    #[inline]
    pub const fn short(&self) -> u64 {
        self.needed.saturating_sub(self.free)
    }
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
    /// For a full disk, how much it needed.
    pub room: Option<Room>,
}

/// How to plan this download again: the caller's own request, not what it resolved to.
#[derive(Clone, Serialize, Deserialize)]
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
    /// Whose depot a repaired file comes from, so the repair can be planned again from its journal.
    /// None for an install, whose plan hash it then leaves as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) owner: Option<String>,
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
                owner: download.owner.clone(),
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room: Option<Room>,
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
    /// What a pause for a full disk needed.
    pub(super) room: Option<Room>,
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
        room: None,
    };
    for record in records {
        // The CRC held, so this is a record of a newer format; what follows it can't be read
        // either.
        let Ok(record) = record else {
            tracing::warn!("unreadable record in a download log, ignoring the rest");
            break;
        };
        folded.paused = None;
        folded.room = None;
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
            Record::Paused { reason, room } => {
                folded.paused = Some(reason);
                folded.room = room;
            }
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

/// What a build switch kept of the download: GOG published another build while it was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildSwitch {
    /// Bytes the new build shares with the earlier one, and so still has.
    pub kept_bytes: u64,
    /// Bytes the earlier build had downloaded.
    pub had_bytes: u64,
}

pub(super) enum Opened {
    /// Fetch what `starts` says is missing, then move.
    Download {
        journal: Journal,
        header: Header,
        starts: Vec<Start>,
        switch: Option<BuildSwitch>,
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

    pub(super) async fn paused(&self, reason: PauseReason, room: Option<Room>) -> anyhow::Result<()> {
        self.append(&Record::Paused { reason, room }).await
    }

    pub(super) async fn moving(&self) -> anyhow::Result<()> {
        self.append(&Record::Moving).await
    }

    pub(super) async fn recorded(&self) -> anyhow::Result<()> {
        self.append(&Record::Recorded).await
    }

    async fn append(&self, record: &Record) -> anyhow::Result<()> {
        let line = crate::achv_try!(encode(record));
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

fn encode(record: &Record) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(record).context("encoding a download log record")
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

/// Takes `staging` for this download: carries on the journal there, keeping what still matches when
/// GOG now plans another build, finishes its move when a crash hit that, and otherwise starts the
/// download over. `held` is the folder's lock when the caller already has it.
///
/// # Errors
/// [`LockError::Busy`] when another process owns the download, or the I/O error that stopped it.
pub(super) async fn open(staging: &Path, header: Header, held: Option<StagingLock>) -> anyhow::Result<Opened> {
    crate::achv_try!(
        tokio::fs::create_dir_all(staging)
            .await
            .with_context(|| format!("creating {}", staging.display()))
    );
    let lock = match held {
        Some(lock) => lock,
        None => crate::achv_try!(lock_waiting(staging).await),
    };
    let discarded = match read_header(staging).await {
        Ok(None) => None,
        Err(discarded) => Some(discarded),
        Ok(Some(previous)) => match crate::achv_try!(carry_on(staging, previous, &header).await) {
            CarryOn::Download { log, starts, switch } => {
                crate::achv_try!(super::pending::clear(staging).await);
                return Ok(Opened::Download {
                    journal: Journal::new(staging, log, lock),
                    header,
                    starts,
                    switch,
                });
            }
            CarryOn::Move { log, previous } => {
                crate::achv_try!(super::pending::clear(staging).await);
                return Ok(Opened::Move {
                    journal: Journal::new(staging, log, lock),
                    header: *previous,
                });
            }
            CarryOn::Over(discarded) => Some(discarded),
        },
    };
    let opened = crate::achv_try!(start_over(staging, header, lock, discarded).await);
    crate::achv_try!(super::pending::clear(staging).await);
    Ok(opened)
}

/// Whether `staging` holds a plan, as opposed to a download that is only queued.
pub(super) async fn has_plan(staging: &Path) -> bool {
    tokio::fs::try_exists(staging.join(HEADER)).await.unwrap_or(true)
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
    Download {
        log: LineLog,
        starts: Vec<Start>,
        switch: Option<BuildSwitch>,
    },
    Move {
        log: LineLog,
        previous: Box<Header>,
    },
    Over(Discarded),
}

async fn carry_on(staging: &Path, previous: Header, header: &Header) -> anyhow::Result<CarryOn> {
    crate::achv_try!(finish_swap(staging, &previous).await);
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
        Phase::Downloading if previous.plan != header.plan => {
            drop(log);
            let (swapped, starts, switch) = crate::achv_try!(carry_over(staging, &previous, &folded, header).await);
            Ok(CarryOn::Download {
                log: swapped,
                starts,
                switch: Some(switch),
            })
        }
        Phase::Downloading => {
            let reconciled = crate::achv_try!(reconcile(staging.join(FILES), &header.files, &folded).await);
            // Each recovered tail is synced by `reconcile` before its record names it.
            for &file in &reconciled.recovered {
                if let Some(&Start::At { chunk, bytes }) = reconciled.starts.get(file) {
                    let commit = crate::achv_try!(encode(&Record::Commit {
                        file,
                        chunks: chunk,
                        bytes
                    }));
                    crate::achv_try!(log.append(&commit).await.context("writing the download log"));
                }
            }
            let resumed = crate::achv_try!(encode(&Record::Resumed));
            crate::achv_try!(log.append(&resumed).await.context("writing the download log"));
            tracing::info!(
                bytes_done = folded.bytes_done(&previous),
                recovered_files = reconciled.recovered.len(),
                clean = folded.paused.is_some(),
                "resuming a download from its journal"
            );
            Ok(CarryOn::Download {
                log,
                starts: reconciled.starts,
                switch: None,
            })
        }
    }
}

/// A crash between writing a new build's header and putting its log in place left that log aside:
/// it belongs to the header on disk, so the swap is finished, and otherwise it is dropped.
async fn finish_swap(staging: &Path, header: &Header) -> anyhow::Result<()> {
    let aside = staging.join(LOG_ASIDE);
    let lines = match decanter_runtime::journal::read_records(&aside).await {
        Ok(lines) => lines,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(anyhow::Error::new(err).context("reading the new build's download log")),
    };
    if fold(header, &lines).is_none() {
        return remove_file(&aside)
            .await
            .context("removing the log of a build switch a crash cut short");
    }
    tracing::warn!(staging = %staging.display(), "finishing a build switch a crash interrupted");
    swap_log(staging).await
}

async fn swap_log(staging: &Path) -> anyhow::Result<()> {
    crate::achv_try!(
        tokio::fs::rename(staging.join(LOG_ASIDE), staging.join(LOG))
            .await
            .context("putting the new build's download log in place")
    );
    decanter_runtime::journal::sync_dir(staging)
        .await
        .context("syncing the download folder")
}

/// What a build switch does to one file the earlier build had started or finished.
#[derive(Clone, Copy)]
enum Cut {
    Keep,
    /// A finished file sharing only its first chunks with the new build, or changing its mode, kept
    /// as a file in flight.
    Reopen {
        bytes: u64,
    },
    Trim {
        bytes: u64,
    },
    Drop,
}

/// GOG planned another build since the journal was written (#1281: carry on with the new build):
/// each file keeps the leading chunks both builds share, by MD5 and size in order, and loses the
/// rest, so no file ever mixes the two. The new plan's header and log replace the old ones only
/// after every file is cut back, and every cut is one the old log already copes with, so a crash at
/// any point resumes either build safely.
async fn carry_over(
    staging: &Path,
    previous: &Header,
    folded: &Folded,
    header: &Header,
) -> anyhow::Result<(LineLog, Vec<Start>, BuildSwitch)> {
    let files = staging.join(FILES);
    // A recovered tail needs no record in the old log: the new one names every kept chunk.
    let before = crate::achv_try!(reconcile(files.clone(), &previous.files, folded).await);
    let mut earlier: HashMap<&str, (&HeaderFile, Start)> = previous
        .files
        .iter()
        .zip(before.starts)
        .map(|(file, start)| (file.path.as_str(), (file, start)))
        .collect();
    let mut cuts = Vec::with_capacity(previous.files.len());
    let mut starts = Vec::with_capacity(header.files.len());
    for file in &header.files {
        let start = match earlier.remove(file.path.as_str()) {
            Some((was, start)) => {
                let (cut, start) = carried(was, start, file);
                cuts.push((file.path.clone(), cut));
                start
            }
            None => Start::At { chunk: 0, bytes: 0 },
        };
        starts.push(start);
    }
    cuts.extend(earlier.into_keys().map(|path| (path.to_owned(), Cut::Drop)));
    let cut =
        tokio::task::spawn_blocking(move || cuts.iter().try_for_each(|(path, cut)| apply_cut(&files, path, *cut)))
            .await;
    crate::achv_try!(match cut {
        Ok(cut) => cut.context("cutting the files of the earlier build back"),
        Err(err) => Err(anyhow::Error::new(err).context("build switch worker panicked")),
    });

    let mut records = Vec::with_capacity(starts.len() + 2);
    records.push(crate::achv_try!(encode(&Record::Begin {
        plan: header.plan.clone()
    })));
    for (file, start) in starts.iter().enumerate() {
        match *start {
            Start::Done => records.push(crate::achv_try!(encode(&Record::Done { file }))),
            Start::At { chunk, bytes } if chunk > 0 => records.push(crate::achv_try!(encode(&Record::Commit {
                file,
                chunks: chunk,
                bytes
            }))),
            Start::At { .. } => {}
        }
    }
    records.push(crate::achv_try!(encode(&Record::Resumed)));
    let aside = staging.join(LOG_ASIDE);
    drop(crate::achv_try!(
        LineLog::create_all(&aside, &records)
            .await
            .context("writing the new build's download log")
    ));
    let encoded = crate::achv_try!(serde_json::to_vec(header).context("encoding the download plan"));
    crate::achv_try!(
        decanter_runtime::journal::write_atomic(&staging.join(HEADER), &encoded)
            .await
            .context("writing the new build's download plan")
    );
    crate::achv_try!(swap_log(staging).await);
    let (log, _) = crate::achv_try!(
        LineLog::open(&staging.join(LOG))
            .await
            .context("reading the download log")
    );

    let kept: u64 = starts
        .iter()
        .zip(&header.files)
        .map(|(start, file)| match start {
            Start::Done => file.size,
            Start::At { bytes, .. } => *bytes,
        })
        .sum();
    let had = folded.bytes_done(previous);
    tracing::info!(
        from_build = previous.build_id,
        to_build = header.build_id,
        kept_bytes = kept,
        had_bytes = had,
        total_bytes = header.total_bytes,
        "GOG published another build while the download was stopped; keeping what both share"
    );
    Ok((
        log,
        starts,
        BuildSwitch {
            kept_bytes: kept,
            had_bytes: had,
        },
    ))
}

/// How file `was`, where `start` says it stood, carries into `now`, the same path in the new build.
fn carried(was: &HeaderFile, start: Start, now: &HeaderFile) -> (Cut, Start) {
    let shared = was
        .chunks
        .iter()
        .zip(&now.chunks)
        .take_while(|(old, new)| old.md5 == new.md5 && old.size == new.size)
        .count();
    let bytes_of = |count: usize| now.chunks.iter().take(count).map(|chunk| len64(chunk.size)).sum();
    match start {
        Start::Done if shared == now.chunks.len() && shared == was.chunks.len() && was.executable == now.executable => {
            (Cut::Keep, Start::Done)
        }
        // Reopened even when every chunk matches, so a file that changed only its mode gets the new one
        // as it is finished again.
        Start::Done if shared > 0 => {
            let bytes = bytes_of(shared);
            (Cut::Reopen { bytes }, Start::At { chunk: shared, bytes })
        }
        Start::At { chunk, bytes } if chunk > 0 && chunk <= shared => (Cut::Keep, Start::At { chunk, bytes }),
        Start::At { chunk, .. } if chunk > 0 && shared > 0 => {
            let bytes = bytes_of(shared);
            (Cut::Trim { bytes }, Start::At { chunk: shared, bytes })
        }
        Start::Done | Start::At { .. } => (Cut::Drop, Start::At { chunk: 0, bytes: 0 }),
    }
}

fn apply_cut(files: &Path, path: &str, cut: Cut) -> io::Result<()> {
    let complete = files.join(path);
    let part = part_path(&complete);
    match cut {
        Cut::Keep => Ok(()),
        Cut::Reopen { bytes } => {
            crate::achv_try!(std::fs::rename(&complete, &part));
            cut_to(&part, bytes)
        }
        Cut::Trim { bytes } => cut_to(&part, bytes),
        Cut::Drop => {
            crate::achv_try!(remove_file_now(&complete));
            remove_file_now(&part)
        }
    }
}

fn cut_to(path: &Path, bytes: u64) -> io::Result<()> {
    crate::achv_try!(std::fs::OpenOptions::new().write(true).open(path)).set_len(bytes)
}

fn remove_file_now(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        Ok(()) | Err(_) => Ok(()),
    }
}

/// Clears `staging` but for its lock and its queued request, which stays until the fresh header
/// written here holds it, and writes that header and a log.
async fn start_over(
    staging: &Path,
    header: Header,
    lock: StagingLock,
    discarded: Option<Discarded>,
) -> anyhow::Result<Opened> {
    if let Some(reason) = discarded {
        tracing::warn!(staging = %staging.display(), %reason, "starting the download over");
    }
    crate::achv_try!(clear(staging, Keep::Pending).await);

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
        switch: None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    Lock,
    Pending,
}

/// Empties `staging` but for its lock, which the caller holds, and for its queued request when
/// asked. The log goes first, so no header is ever read against a log that outlived its plan.
async fn clear(staging: &Path, keep: Keep) -> anyhow::Result<()> {
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
        let name = entry.file_name();
        if name == "lock" || keep == Keep::Pending && name == super::pending::PENDING {
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
    discard_with(staging, lock).await
}

/// [`discard`], for a caller already holding `staging`'s lock.
pub(super) async fn discard_with(staging: &Path, lock: StagingLock) -> anyhow::Result<()> {
    crate::achv_try!(clear(staging, Keep::Lock).await);
    drop(lock);
    crate::achv_try!(
        remove_dir(staging)
            .await
            .with_context(|| format!("removing {}", staging.display()))
    );
    Ok(())
}

/// Deletes a staging folder that has no journal, for the user to free what an older version or an
/// early cut left. The lock is taken first and the journal looked for under it, so a download that
/// started meanwhile is never touched.
///
/// # Errors
/// [`LockError::Busy`] when a process runs it, an error when it has a journal, or what stopped the removal.
pub(super) async fn discard_stray(staging: &Path) -> anyhow::Result<()> {
    let lock = crate::achv_try!(decanter_runtime::journal::lock(staging));
    if has_journal(staging) {
        anyhow::bail!("{} has a download journal", staging.display());
    }
    crate::achv_try!(clear(staging, Keep::Lock).await);
    drop(lock);
    crate::achv_try!(
        remove_dir(staging)
            .await
            .with_context(|| format!("removing {}", staging.display()))
    );
    Ok(())
}

/// A queued download's request counts: it is listed, and resumed, from that alone.
pub(super) fn has_journal(staging: &Path) -> bool {
    staging.join(HEADER).exists() || staging.join(super::pending::PENDING).exists()
}

pub(super) async fn lock_waiting(staging: &Path) -> Result<StagingLock, LockError> {
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
    pub(super) request: Request,
    /// `None` for a download queued and never planned.
    pub(super) header: Option<Header>,
    /// `None` when the log can't be read against the header: nothing of it would be kept.
    pub(super) folded: Option<Folded>,
    /// Why it stopped while it waited for a plan, which is newer than anything the log says.
    pub(super) stop: Option<PauseReason>,
    /// What a repair never planned repairs.
    pub(super) files: Vec<super::pending::PendingFile>,
    /// Another process owns it right now.
    pub(super) in_use: bool,
}

impl Found {
    pub(super) fn bytes_done(&self) -> u64 {
        self.header
            .as_ref()
            .zip(self.folded.as_ref())
            .map_or(0, |(header, folded)| folded.bytes_done(header))
    }
}

pub(super) async fn inspect(staging: &Path) -> Option<Found> {
    let header = match read_header(staging).await {
        Ok(header) => header,
        Err(reason) => {
            tracing::warn!(staging = %staging.display(), %reason, "skipping an unreadable download journal");
            return None;
        }
    };
    let (request, stop, files) = match (&header, super::pending::read(staging).await) {
        (Some(planned), queued) => (
            planned.request.clone(),
            queued.and_then(|stopped| stopped.stop),
            Vec::new(),
        ),
        (None, Some(queued)) => (queued.request, queued.stop, queued.files),
        (None, None) => return None,
    };
    let in_use = matches!(decanter_runtime::journal::lock(staging), Err(LockError::Busy(_)));
    let folded = match &header {
        Some(planned) => decanter_runtime::journal::read_records(&staging.join(LOG))
            .await
            .ok()
            .and_then(|lines| fold(planned, &lines)),
        None => None,
    };
    Some(Found {
        request,
        header,
        folded,
        stop,
        files,
        in_use,
    })
}

/// When the journal last recorded anything: for a paused download, when it paused.
pub(super) async fn last_written(staging: &Path) -> Option<std::time::SystemTime> {
    // The header is written before its log, and a queued download has neither.
    for name in [LOG, HEADER, super::pending::PENDING] {
        if let Ok(modified) = tokio::fs::metadata(staging.join(name))
            .await
            .and_then(|meta| meta.modified())
        {
            return Some(modified);
        }
    }
    None
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

struct Reconciled {
    starts: Vec<Start>,
    /// Files whose start went past their last record, with the bytes it now names already synced:
    /// each needs a `Commit` before the download goes on.
    recovered: Vec<usize>,
}

/// Brings every file of `plan` (the plan `folded` was read against) back to what its log vouches for,
/// plus the whole chunks past the last record that still match the plan.
async fn reconcile(files: PathBuf, plan: &[HeaderFile], folded: &Folded) -> anyhow::Result<Reconciled> {
    let shapes: Vec<Shape> = plan
        .iter()
        .map(|file| Shape {
            path: PathBuf::from(&file.path),
            // Written by `hex_digest`, so it parses; one that didn't would match no bytes, and the
            // re-hash would cut the file back there, which is safe.
            chunks: file
                .chunks
                .iter()
                .map(|chunk| (len64(chunk.size), parse_md5(&chunk.md5).unwrap_or_default()))
                .collect(),
        })
        .collect();
    let progress = folded.files.clone();
    let clean = folded.paused.is_some();
    let checked = tokio::task::spawn_blocking(move || {
        let mut reconciled = Reconciled {
            starts: Vec::with_capacity(shapes.len()),
            recovered: Vec::new(),
        };
        for (index, (shape, progress)) in shapes.iter().zip(progress).enumerate() {
            let (start, is_recovered) = crate::achv_try!(reconcile_file(&files, shape, progress, clean));
            if is_recovered {
                reconciled.recovered.push(index);
            }
            reconciled.starts.push(start);
        }
        io::Result::Ok(reconciled)
    })
    .await;
    match checked {
        Ok(starts) => starts.context("checking the files already downloaded"),
        Err(err) => Err(anyhow::Error::new(err).context("download check worker panicked")),
    }
}

/// Brings one file back to what its log vouches for. Bytes are only ever cut, never assumed: a
/// file shorter than its record lost data a sync promised (a disk that lies, an unsafe unplug) and
/// starts over, and after an unclean exit the last window is hashed again. Past the record, the
/// whole chunks that match the plan are kept and synced; `true` when there were any.
fn reconcile_file(files: &Path, shape: &Shape, progress: FileProgress, clean: bool) -> io::Result<(Start, bool)> {
    let complete = files.join(&shape.path);
    let part = part_path(&complete);
    let (chunks, bytes) = match progress {
        FileProgress::Done => {
            let size: u64 = shape.chunks.iter().map(|&(size, _)| size).sum();
            if std::fs::metadata(&complete).is_ok_and(|meta| meta.len() == size) {
                return Ok((Start::Done, false));
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
    let is_consistent = named == Some(bytes) && on_disk >= bytes;
    let (chunks, bytes) = if is_consistent {
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
    let (vouched_chunks, vouched_bytes) = if clean || chunks == 0 {
        (chunks, bytes)
    } else {
        crate::achv_try!(rehash_window(&part, &shape.chunks, chunks, bytes))
    };
    // Only past a record whose own bytes all held: a file that lost what a sync promised, or whose
    // last window failed its re-hash, says nothing good about what follows.
    let (chunks, bytes) = if is_consistent && vouched_chunks == chunks && on_disk > bytes {
        crate::achv_try!(recover_tail(&part, &shape.chunks, chunks, bytes, on_disk))
    } else {
        (vouched_chunks, vouched_bytes)
    };
    let is_recovered = chunks > vouched_chunks;
    if on_disk > 0 {
        let file = crate::achv_try!(std::fs::OpenOptions::new().write(true).open(&part));
        crate::achv_try!(file.set_len(bytes));
        // After a kill the recovered bytes may still be only in the page cache: synced before the
        // caller's record names them, the phase 2 ordering.
        if is_recovered {
            crate::achv_try!(file.sync_data());
        }
    }
    Ok((Start::At { chunk: chunks, bytes }, is_recovered))
}

/// What [`recover_tail`] reads at a time, so a chunk of up to `MAX_CHUNK_SIZE` is hashed without
/// holding it whole.
const SCAN_BUFFER: usize = 1024 * 1024;

/// Chunks written after the last record (up to [`COMMIT_BYTES`] or a commit interval of them per
/// file, more when the commit of a stopped file failed) whose record an unclean exit cut off:
/// the plan's next chunks are read from `part` past `bytes` in order, and each holding its whole
/// inflated size with the right MD5 is kept, so only the chunk in flight is lost. Stops at the first
/// that is short or doesn't match; reads at most `on_disk - bytes` and never past the plan.
fn recover_tail(
    part: &Path,
    chunks: &[(u64, [u8; 16])],
    count: usize,
    bytes: u64,
    on_disk: u64,
) -> io::Result<(usize, u64)> {
    let mut file = crate::achv_try!(std::fs::File::open(part));
    crate::achv_try!(std::io::Seek::seek(&mut file, io::SeekFrom::Start(bytes)));
    let mut buffer = vec![0; SCAN_BUFFER];
    let (mut kept, mut offset) = (count, bytes);
    let mut rejected = None;
    for (index, &(size, md5)) in chunks.iter().enumerate().skip(count) {
        if on_disk.saturating_sub(offset) < size {
            rejected = Some((index, "partial"));
            break;
        }
        let mut hash = md5::Context::new();
        let mut left = size;
        while left > 0 {
            let take = usize::try_from(left).map_or(SCAN_BUFFER, |remaining| remaining.min(SCAN_BUFFER));
            let Some(piece) = buffer.get_mut(..take) else {
                break;
            };
            match file.read_exact(piece) {
                Ok(()) => hash.consume(&*piece),
                // Shorter than its metadata said a moment ago: something else cut it.
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err),
            }
            left -= len64(take);
        }
        if left > 0 {
            rejected = Some((index, "partial"));
            break;
        }
        if hash.finalize().0 != md5 {
            rejected = Some((index, "md5 mismatch"));
            break;
        }
        kept = index + 1;
        offset += size;
    }
    let (rejected_chunk, rejected_why) = rejected.map_or((None, "end of plan"), |(index, why)| (Some(index), why));
    tracing::info!(
        part = %part.display(),
        recovered_chunks = kept - count,
        recovered_bytes = offset - bytes,
        discarded_bytes = on_disk.saturating_sub(offset),
        rejected_chunk,
        rejected_why,
        "checked the chunks past the last record"
    );
    Ok((kept, offset))
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
