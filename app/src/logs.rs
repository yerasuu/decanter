//! decanter's own log: stderr as before, and a file per day in the active
//! library's `decanter/logs/`, so what happened is still there for a
//! developer after the terminal is gone. Plain text in English, the same
//! lines stderr gets, plus decanter's own debug events.
//!
//! The file opens once the library is known, after the migration that may
//! put it there; lines logged before that are kept and written first.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, fmt};

/// Days of logs kept; older files go when a new day's file opens.
const KEEP: usize = 14;

/// `None` once it's settled there is no file this session: no library, or it couldn't be opened.
static FILE: OnceLock<Option<File>> = OnceLock::new();
/// Lines from before [`FILE`] is settled. A mutex because any thread can log then.
static EARLY: Mutex<Vec<u8>> = Mutex::new(Vec::new());

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
        FILE.get()
            .and_then(Option::as_ref)
            .map_or(Ok(buf.len()), |mut file| file.write(buf))
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
            settle(None);
            return;
        }
    };
    let path = dir.join(format!("decanter-{}.log", chrono::Utc::now().format("%Y-%m-%d")));
    let opened = fs::create_dir_all(&dir).and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
    match opened {
        Ok(file) => {
            settle(Some(file));
            tracing::info!(path = %path.display(), "logging to file");
            prune(&dir);
        }
        Err(err) => {
            settle(None);
            tracing::warn!(path = %path.display(), %err, "could not open the log file");
        }
    }
}

fn settle(file: Option<File>) {
    let written = {
        let mut early = EARLY.lock().unwrap_or_else(PoisonError::into_inner);
        let written = file.as_ref().map_or(Ok(()), |mut file| file.write_all(&early));
        FILE.get_or_init(|| file);
        *early = Vec::new();
        written
    };
    // Only once the lock is released: this line goes through the writer, which takes it.
    if let Err(err) = written {
        tracing::warn!(%err, "could not write the early lines to the log file");
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
