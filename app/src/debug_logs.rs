//! Debug mode (decision #620): decanter's and the PS port's logs gathered in one folder,
//! which the user may sync between machines, with a subfolder per host so two machines never
//! write the same file. Every file is written as `.partial` and renamed once whole, so a sync
//! tool never copies half a log.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead as _, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::Utc;
use decanter_core::bootstrap::{self, Location};
use decanter_core::{private, redact};

use crate::i18n::Text;

/// The PS launch settings' key of a game's own debug switch (`titles.<TITLE_ID>.debug`).
pub const TITLE_KEY: &str = "debug";
const SESSIONS_KEPT: usize = 20;
const JOBS_KEPT: usize = 50;
const SESSION_PREFIX: &str = "decanter-";
/// `YYYYmmdd-HHMMSS`, the stamp every kept log's name carries.
const STAMP_LEN: usize = 15;

pub fn enabled() -> bool {
    crate::core::db()
        .and_then(|db| db.debug_mode().map_err(crate::core::StoreError::Query))
        .inspect_err(|err| tracing::warn!(%err, "could not read the debug mode, leaving it off"))
        .unwrap_or(false)
}

pub fn set_enabled(enabled: bool) -> Result<(), Text> {
    let db = crate::achv_try!(crate::core::db());
    db.set_debug_mode(enabled)
        .map_err(|err| Text::new("settings.general.debug.errors.save").arg("error", err))
}

/// The folder whether the mode is on or not, and whether the user picked it; `None` before
/// there is a library to hold the default.
pub fn folder() -> Option<(PathBuf, bool)> {
    let chosen = crate::core::db().ok().and_then(|db| {
        db.debug_log_dir()
            .inspect_err(|err| tracing::warn!(%err, "could not read the debug log folder, using the default"))
            .ok()
            .flatten()
    });
    chosen.map_or_else(
        || default_folder().map(|dir| (dir, false)),
        |dir| Some((PathBuf::from(dir), true)),
    )
}

fn default_folder() -> Option<PathBuf> {
    match bootstrap::location() {
        Location::Library(root) => Some(bootstrap::library_data_dir(&root).join("logs").join("debug")),
        Location::Legacy(_) | Location::Unconfigured => None,
    }
}

/// `None` with the mode off.
pub fn active_folder() -> Option<PathBuf> {
    enabled().then(folder).flatten().map(|(dir, _)| dir)
}

/// The port's `PS_DEBUG_DIR`: the folder itself, since the port adds the host on its own.
/// `title_on` is the game's own switch, which turns the mode on for that game alone.
pub fn ps_folder(title_on: bool) -> Option<PathBuf> {
    (title_on || enabled()).then(folder).flatten().map(|(dir, _)| dir)
}

pub fn host_dir(folder: &Path) -> PathBuf {
    folder.join(host())
}

fn host() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| sanitise_host(crate::platform::hostname().as_deref().unwrap_or_default()))
}

/// A single path component on every OS: anything but ASCII letters, digits, `-` and `_` becomes
/// `_`, and dots only inside the name so it can never read as `.` or `..`.
fn sanitise_host(name: &str) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_matches('.');
    if clean.is_empty() {
        "unknown-host".to_owned()
    } else {
        clean.to_owned()
    }
}

/// Points the debug logs at `picked`, or back at the default with `None`. `picked` comes from a
/// folder dialog, so it is checked here, once: an absolute, writable directory.
pub fn set_folder(picked: Option<&Path>) -> Result<(), Text> {
    let db = crate::achv_try!(crate::core::db());
    let save = |dir: Option<&str>| {
        db.set_debug_log_dir(dir)
            .map_err(|err| Text::new("settings.general.debug.errors.save").arg("error", err))
    };
    let Some(picked) = picked else {
        return save(None);
    };
    if !picked.is_absolute() || !picked.is_dir() {
        return Err(Text::new("settings.general.debug.errors.not_dir").arg("path", picked.display()));
    }
    crate::achv_try!(crate::platform::check_writable(picked).map_err(|err| {
        Text::new("settings.general.debug.errors.not_writable")
            .arg("path", picked.display())
            .arg("error", err)
    }));
    if default_folder().as_deref() == Some(picked) {
        return save(None);
    }
    let Some(text) = picked.to_str() else {
        return Err(Text::new("settings.general.debug.errors.not_utf8").arg("path", picked.display()));
    };
    save(Some(text))
}

/// The folder's host subfolder, made when missing so there is something to open.
pub fn open_folder(folder: &Path) -> io::Result<()> {
    let dir = host_dir(folder);
    private::create_dir(&dir)?;
    crate::desktop::open(&dir)
}

/// This session's log in `folder`'s host subfolder, as an open `.partial` file and its path.
/// What a previous session of this host left as `.partial` ended without a clean exit, and is
/// renamed first so it syncs as it is.
pub fn open_session(folder: &Path) -> io::Result<(fs::File, PathBuf)> {
    let dir = host_dir(folder);
    private::create_dir(&dir)?;
    complete_stale(&dir);
    prune(&dir, SESSIONS_KEPT.saturating_sub(1), session_stamp);
    let stem = format!("{SESSION_PREFIX}{}", Utc::now().format("%Y%m%d-%H%M%S"));
    let mut attempt: u32 = 1;
    loop {
        let name = if attempt == 1 {
            format!("{stem}.log")
        } else {
            format!("{stem}-{attempt}.log")
        };
        let partial = partial_of(&dir.join(&name));
        if !dir.join(&name).exists() {
            match private::file_options().append(true).create_new(true).open(&partial) {
                Ok(file) => return Ok((file, partial)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
        attempt = attempt.saturating_add(1);
    }
}

/// Drops the `.partial` from a finished file's name, which is when a sync tool may take it.
pub fn complete(partial: &Path) -> io::Result<PathBuf> {
    let done = partial.with_extension("");
    fs::rename(partial, &done)?;
    Ok(done)
}

fn complete_stale(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), %err, "could not list the debug log folder");
            return;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_stale = name
            .to_str()
            .and_then(|name| name.strip_suffix(".partial"))
            .and_then(session_stamp)
            .is_some();
        if is_stale {
            match complete(&entry.path()) {
                Ok(done) => tracing::info!(log = %done.display(), "finished a previous session's debug log"),
                Err(err) => {
                    tracing::warn!(log = %entry.path().display(), %err, "could not finish a previous session's debug log");
                }
            }
        }
    }
}

/// Copies a finished job's log, and its call report when there is one, into the host's `jobs/`
/// with the mode on. Copied rather than written there, so the library keeps its own logs, which
/// Registro and the run statistics read, whatever the mode; the copies, meant to be synced, lose
/// what [`redact::text`] finds.
pub async fn keep_job(log: PathBuf, calls: Option<PathBuf>) {
    let Some(folder) = active_folder() else {
        return;
    };
    let jobs = host_dir(&folder).join("jobs");
    let copied = tokio::task::spawn_blocking(move || copy_job(&jobs, &log, calls.as_deref())).await;
    match copied {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(%err, "could not copy the job's log to the debug log folder"),
        Err(err) => tracing::warn!(%err, "the copy of the job's log to the debug log folder did not finish"),
    }
}

fn copy_job(jobs: &Path, log: &Path, calls: Option<&Path>) -> io::Result<()> {
    private::create_dir(jobs)?;
    if let Some(calls) = calls.filter(|calls| calls.is_file()) {
        publish_copy(calls, jobs)?;
    }
    let copied = publish_copy(log, jobs)?;
    tracing::debug!(log = %copied.display(), "job log copied to the debug log folder");
    prune(jobs, JOBS_KEPT, job_stamp);
    Ok(())
}

fn publish_copy(source: &Path, dir: &Path) -> io::Result<PathBuf> {
    let Some(name) = source.file_name() else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    let partial = partial_of(&dir.join(name));
    let lines = BufReader::new(fs::File::open(source)?).split(b'\n');
    let mut copy = BufWriter::new(
        private::file_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial)?,
    );
    let mut redacted = 0_usize;
    for line in lines {
        let line = line?;
        let line = String::from_utf8_lossy(&line);
        let clean = redact::text(&line);
        if matches!(clean, Cow::Owned(_)) {
            redacted = redacted.saturating_add(1);
        }
        copy.write_all(clean.as_bytes())?;
        copy.write_all(b"\n")?;
    }
    copy.flush()?;
    tracing::debug!(source = %source.display(), redacted_lines = redacted, "debug copy scrubbed");
    complete(&partial)
}

fn partial_of(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".partial");
    PathBuf::from(name)
}

/// Keeps the newest `keep` logs whose names `stamp` recognises, with their call reports; files of
/// any other name are not decanter's and stay.
fn prune(dir: &Path, keep: usize, stamp: fn(&str) -> Option<&str>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), %err, "could not list old debug logs");
            return;
        }
    };
    let mut logs: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((stamp(&name)?.to_owned(), name))
        })
        .collect();
    logs.sort_unstable();
    for (_, name) in logs.iter().take(logs.len().saturating_sub(keep)) {
        let log = dir.join(name);
        for old in [log.with_extension("calls"), log] {
            if let Err(err) = fs::remove_file(&old)
                && err.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(path = %old.display(), %err, "could not remove an old debug log");
            }
        }
    }
}

fn is_stamp(text: &str) -> bool {
    text.len() == STAMP_LEN
        && text.bytes().enumerate().all(|(index, byte)| {
            if index == 8 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// `""`, or the `-N` a second file of the same second gets.
fn is_attempt(text: &str) -> bool {
    text.is_empty()
        || text
            .strip_prefix('-')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

/// `decanter-YYYYmmdd-HHMMSS[-N].log`.
fn session_stamp(name: &str) -> Option<&str> {
    let rest = name.strip_suffix(".log")?.strip_prefix(SESSION_PREFIX)?;
    let stamp = rest.get(..STAMP_LEN)?;
    (is_stamp(stamp) && is_attempt(rest.get(STAMP_LEN..)?)).then_some(stamp)
}

/// `<target>-<kind>-YYYYmmdd-HHMMSS[-N].log`, as [`decanter_ps::run_log::RunLog`] names them.
fn job_stamp(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".log")?;
    ends_in_stamp(stem).or_else(|| {
        let (head, attempt) = stem.rsplit_once('-')?;
        let is_number = !attempt.is_empty() && attempt.bytes().all(|byte| byte.is_ascii_digit());
        is_number.then(|| ends_in_stamp(head)).flatten()
    })
}

fn ends_in_stamp(stem: &str) -> Option<&str> {
    let tail = stem.get(stem.len().checked_sub(STAMP_LEN + 1)?..)?;
    tail.strip_prefix('-').filter(|stamp| is_stamp(stamp))
}
