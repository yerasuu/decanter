//! decanter's own log: stderr as before, and a file per UTC day in the active
//! library's `decanter/logs/`, so what happened is still there for a
//! developer after the terminal is gone. Plain text in English, the same
//! lines stderr gets, plus decanter's own debug events.
//!
//! The file opens once the library is known, after the migration that may
//! put it there; lines logged before that are kept and written first. With
//! debug mode on at that point, the same lines also go to the session's log in
//! the debug log folder (see [`crate::debug_logs`]).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError, RwLock};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use decanter_core::private;

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, fmt};

/// Days of logs kept; older files go when a new day's file opens.
const KEEP: usize = 14;

/// How often the day is checked; a sleep until midnight would not count time spent suspended.
const DAY_CHECK: Duration = Duration::from_secs(60);

/// `None` once it's settled there is no file this session: no library, or it couldn't be opened.
/// Swapped for the next day's file by [`rotate`].
static FILE: OnceLock<Option<RwLock<File>>> = OnceLock::new();
/// Lines from before [`FILE`] is settled. A mutex because any thread can log then.
static EARLY: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Settled with [`FILE`], before it: `None` with debug mode off when decanter started.
static DEBUG: OnceLock<Option<DebugLog>> = OnceLock::new();

struct DebugLog {
    file: File,
    partial: PathBuf,
}

struct LibraryLog;

impl Write for LibraryLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if FILE.get().is_none() {
            let mut early = EARLY.lock().unwrap_or_else(PoisonError::into_inner);
            // Settled while this thread waited for the lock, after the early lines went out.
            if FILE.get().is_none() {
                early.extend_from_slice(buf);
                return Ok(buf.len());
            }
            drop(early);
        }
        let written = FILE.get().and_then(Option::as_ref).map_or(Ok(buf.len()), |file| {
            let file = file.read().unwrap_or_else(PoisonError::into_inner);
            (&*file).write(buf)
        });
        // Only what the library's file took, since the writer sends the rest again.
        if let (Ok(count), Some(Some(debug))) = (&written, DEBUG.get())
            && let Some(taken) = buf.get(..*count)
        {
            // A line the debug copy misses is no reason to fail the library's log.
            let _: io::Result<()> = (&debug.file).write_all(taken);
        }
        written
    }

    /// Nothing to flush: writes go straight to the file.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn init() {
    let file_filter = Targets::new().with_default(LevelFilter::INFO).with_targets(
        [
            "decanter",
            "decanter_core",
            "decanter_gog",
            "decanter_ps",
            "decanter_runtime",
        ]
        .map(|target| (target, LevelFilter::DEBUG)),
    );
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(io::stderr).with_filter(LevelFilter::INFO))
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(|| LibraryLog)
                .with_filter(file_filter),
        )
        .init();
    tracing::info!(
        build = decanter_core::BUILD,
        pid = std::process::id(),
        "decanter started"
    );
}

/// Settles where the file goes, once the library is known; before a library exists
/// (or before the migration ran) there is nowhere inside one to put it.
pub fn open() {
    let dir = match decanter_core::bootstrap::location() {
        decanter_core::bootstrap::Location::Library(root) if root.is_dir() => {
            decanter_core::bootstrap::library_data_dir(&root).join("logs")
        }
        decanter_core::bootstrap::Location::Library(_)
        | decanter_core::bootstrap::Location::Legacy(_)
        | decanter_core::bootstrap::Location::Unconfigured => {
            settle(None, None);
            return;
        }
    };
    let debug = open_debug();
    let day = Utc::now().date_naive();
    let path = path_for(&dir, day);
    match open_file(&path) {
        Ok(file) => {
            settle(Some(file), debug);
            tracing::info!(path = %path.display(), "logging to file");
            prune(&dir);
            watch_day(dir, day);
        }
        Err(err) => {
            settle(None, debug);
            tracing::warn!(path = %path.display(), %err, "could not open the log file");
        }
    }
    if let Some(Some(session)) = DEBUG.get() {
        tracing::info!(path = %session.partial.display(), "debug mode: logging this session to the debug log folder");
    }
}

/// What older versions made with the default modes, made owner-only once per start on a thread of
/// its own, since a library on a slow or network disk may hold hundreds of logs.
pub fn protect() {
    let decanter_core::bootstrap::Location::Library(root) = decanter_core::bootstrap::location() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("private-files".to_owned())
        .spawn(move || {
            let data = decanter_core::bootstrap::library_data_dir(&root);
            let db = decanter_core::bootstrap::db_path(&root);
            let journals = ["-wal", "-shm", "-journal"].map(|suffix| {
                let mut name = db.clone().into_os_string();
                name.push(suffix);
                PathBuf::from(name)
            });
            for path in [db, data.join("logs"), data.join("reports")]
                .into_iter()
                .chain(journals)
            {
                private::narrow(&path);
            }
            private::narrow(&decanter_ps::run_log::dir(&decanter_ps::paths::active()));
            // A folder the user picked may hold more than decanter's; only this host's part is its.
            if let Some((folder, true)) = crate::debug_logs::folder() {
                private::narrow(&crate::debug_logs::host_dir(&folder));
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "could not start making the library's logs owner-only");
    }
}

/// Read once, here: the subscriber is already running, so turning the mode on later only
/// reaches the next session.
fn open_debug() -> Option<DebugLog> {
    let folder = crate::debug_logs::active_folder()?;
    crate::debug_logs::open_session(&folder)
        .map(|(file, partial)| DebugLog { file, partial })
        .inspect_err(|err| tracing::warn!(folder = %folder.display(), %err, "could not open the session's debug log"))
        .ok()
}

/// Whether this session writes its log to the debug log folder.
pub fn debug_session() -> bool {
    matches!(DEBUG.get(), Some(Some(_)))
}

/// At a clean exit the session's debug log is whole: it loses its `.partial`, so a sync tool
/// takes it. Lines logged after this still reach the renamed file.
pub fn close() {
    let Some(Some(session)) = DEBUG.get() else {
        return;
    };
    tracing::info!("decanter exiting, finishing the session's debug log");
    match crate::debug_logs::complete(&session.partial) {
        Ok(done) => tracing::debug!(path = %done.display(), "session debug log finished"),
        Err(err) => tracing::warn!(path = %session.partial.display(), %err, "could not finish the session's debug log"),
    }
}

fn settle(file: Option<File>, debug: Option<DebugLog>) {
    let (written, debug_written) = {
        let mut early = EARLY.lock().unwrap_or_else(PoisonError::into_inner);
        let written = file.as_ref().map_or(Ok(()), |mut file| file.write_all(&early));
        let debug_written = debug.as_ref().map_or(Ok(()), |debug| (&debug.file).write_all(&early));
        DEBUG.get_or_init(|| debug);
        FILE.get_or_init(|| file.map(RwLock::new));
        *early = Vec::new();
        drop(early);
        (written, debug_written)
    };
    // Only once the lock is released: these lines go through the writer, which takes it.
    if let Err(err) = written {
        tracing::warn!(%err, "could not write the early lines to the log file");
    }
    if let Err(err) = debug_written {
        tracing::warn!(%err, "could not write the early lines to the session's debug log");
    }
}

fn path_for(dir: &Path, day: NaiveDate) -> PathBuf {
    dir.join(format!("decanter-{}.log", day.format("%Y-%m-%d")))
}

fn open_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        private::create_dir(dir)?;
    }
    private::file_options().create(true).append(true).open(path)
}

/// decanter can stay open for days in the tray; each day still gets its own file, and the
/// oldest go as new ones open.
#[expect(clippy::infinite_loop, reason = "the watcher lives as long as the process")]
fn watch_day(dir: PathBuf, mut day: NaiveDate) {
    let watcher = std::thread::Builder::new().name("log-day".to_owned()).spawn(move || {
        loop {
            std::thread::sleep(DAY_CHECK);
            let today = Utc::now().date_naive();
            if today != day {
                day = today;
                rotate(&dir, today);
            }
        }
    });
    if let Err(err) = watcher {
        tracing::warn!(%err, "could not start the log's day watcher, staying on one file");
    }
}

fn rotate(dir: &Path, day: NaiveDate) {
    let Some(Some(current)) = FILE.get() else {
        return;
    };
    let path = path_for(dir, day);
    match open_file(&path) {
        Ok(file) => {
            *current.write().unwrap_or_else(PoisonError::into_inner) = file;
            tracing::info!(build = decanter_core::BUILD, path = %path.display(), "logging to file");
            prune(dir);
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "could not open the new day's log file, staying on the old one");
        }
    }
}

fn prune(dir: &Path) {
    let mut logs: Vec<_> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|extension| extension == "log")
                    && path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| stem.starts_with("decanter-"))
            })
            .collect(),
        Err(err) => {
            tracing::warn!(dir = %dir.display(), %err, "could not list old log files");
            return;
        }
    };
    // The date in the name sorts like the day it records.
    logs.sort();
    for old in logs.iter().take(logs.len().saturating_sub(KEEP)) {
        if let Err(err) = fs::remove_file(old) {
            tracing::warn!(path = %old.display(), %err, "could not remove an old log file");
        }
    }
}
