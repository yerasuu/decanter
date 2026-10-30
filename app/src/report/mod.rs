//! Error reports built from the local files and saved inside the library (decisions #982, #991).
//!
//! Everything goes through `decanter-report`: the closed schema, the closed message vocabulary and
//! the machine's own names ([`identity`]) the builders keep out. A report is shown as it would go
//! out; it leaves the machine only through [`send`], on the player's confirmation, and only once
//! per press.
//!
//! Blocking file I/O throughout: off the UI thread.

mod host;
mod identity;
pub mod scrub;
pub mod send;
pub mod sent;

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use decanter_ps::failures::Failure;
use decanter_ps::library;
use decanter_report::sources::day_log::{DayFailure, DayLog};
use decanter_report::sources::failures::{self, FailureRecord};
use decanter_report::sources::game_log::GameLog;
use decanter_report::{PsportInput, Rejection, Report, decanter_report, fingerprint, psport_report};

use crate::i18n::Text;
use scrub::{Cleaned, Scrubber};

/// A run's log is read up to here; a report only draws from the lines it knows, and a longer
/// file is a runaway output.
const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Product {
    Decanter,
    Psport,
}

impl Product {
    /// The partition letter of the ids the server hands out for this product.
    pub const fn id_letter(self) -> char {
        match self {
            Self::Decanter => 'd',
            Self::Psport => 'p',
        }
    }
}

#[derive(Clone)]
pub struct Built {
    pub product: Product,
    /// Pretty-printed: exactly what would go out, and what is saved.
    pub json: String,
    pub fingerprint: String,
    pub date: String,
}

pub enum Error {
    NoFailure,
    Rejected(Vec<Rejection>),
    Read { what: &'static str, err: io::Error },
    Serialize(serde_json::Error),
    Exists(PathBuf),
    Write { path: PathBuf, err: io::Error },
}

/// The private attachment as it goes out, with what was cleaned from it so the player is told before
/// sending.
#[derive(Clone)]
pub struct Attachment {
    pub bytes: Vec<u8>,
    pub cleaned: Cleaned,
}

/// The private attachment: the failure's run log, scrubbed and cut to its last [`send::MAX_ATTACHMENT`] bytes.
pub fn attachment(log: &Path) -> Result<Attachment, Error> {
    let tail = || -> io::Result<Vec<u8>> {
        let mut file = fs::File::open(log)?;
        let len = file.metadata()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(send::MAX_ATTACHMENT as u64)))?;
        let mut bytes = Vec::new();
        file.take(send::MAX_ATTACHMENT as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let bytes = tail().map_err(|err| Error::Read {
        what: "ps.report.what.run_log",
        err,
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let (scrubbed, cleaned) = Scrubber::collect().log(&text);
    let mut start = scrubbed.len().saturating_sub(send::MAX_ATTACHMENT);
    while !scrubbed.is_char_boundary(start) {
        start += 1;
    }
    Ok(Attachment {
        bytes: scrubbed.into_bytes().split_off(start),
        cleaned,
    })
}

impl Error {
    pub fn text(&self) -> Text {
        match self {
            Self::NoFailure => Text::new("ps.report.errors.no_failure"),
            Self::Rejected(rejections) => Text::new("ps.report.errors.rejected").arg("fields", describe(rejections)),
            Self::Read { what, err } => Text::new("ps.report.errors.read")
                .text("what", Text::new(what))
                .arg("error", err),
            Self::Serialize(err) => Text::new("ps.report.errors.serialize").arg("error", err),
            Self::Exists(path) => Text::new("ps.report.errors.exists").arg("path", path.display()),
            Self::Write { path, err } => Text::new("ps.report.errors.write")
                .arg("path", path.display())
                .arg("error", err),
        }
    }
}

/// Field and rule, never the value, since a value is what could be private.
fn describe(rejections: &[Rejection]) -> String {
    rejections
        .iter()
        .map(|rejection| {
            let path = if rejection.path.is_empty() {
                "report"
            } else {
                &rejection.path
            };
            format!("{path}: {:?}", rejection.rule)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn read_capped(path: &Path, what: &'static str) -> Result<String, Error> {
    let read = || {
        let mut bytes = Vec::new();
        fs::File::open(path)?.take(MAX_LOG_BYTES).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    read().map_err(|err| Error::Read { what, err })
}

fn finish(report: &Report) -> Result<Built, Error> {
    let (product, date) = match report {
        Report::Decanter(report) => (Product::Decanter, report.date.clone()),
        Report::Psport(report) => (Product::Psport, report.date.clone()),
    };
    Ok(Built {
        product,
        json: serde_json::to_string_pretty(report).map_err(Error::Serialize)?,
        fingerprint: fingerprint(report),
        date,
    })
}

/// A psport report of one `failures.jsonl` entry, with its run's log and its game's data from
/// the dump's `param.sfo`.
pub fn psport(failure: &Failure) -> Result<Built, Error> {
    let line = serde_json::to_string(failure).map_err(Error::Serialize)?;
    let record: Option<FailureRecord> = failures::parse(&line).next();
    let log = match &failure.log {
        Some(path) => Some(GameLog::parse(&read_capped(path, "ps.report.what.run_log")?)),
        None => None,
    };
    let game = library::scan(std::slice::from_ref(&failure.game_dir))
        .into_iter()
        .find(|game| game.dir == failure.game_dir);
    let input = PsportInput {
        record: record.as_ref(),
        log: log.as_ref(),
        title_id: game.as_ref().map(|game| game.title_id.as_str()),
        title_name: game.as_ref().map(|game| game.title.as_str()),
    };
    let report = psport_report(&input, &host::host(), &identity::collect()).map_err(Error::Rejected)?;
    finish(&report)
}

/// A decanter report of one failure of the day log.
pub fn decanter(failure: &DayFailure) -> Result<Built, Error> {
    let report = decanter_report(failure, &host::host(), &identity::collect()).map_err(Error::Rejected)?;
    finish(&report)
}

/// The failures of the day log of `day` (`YYYY-MM-DD`), or of the latest day that has one, oldest
/// first.
pub fn day_failures(day: Option<&str>) -> Result<Vec<DayFailure>, Error> {
    let dir = logs_dir();
    let path = match day {
        Some(day) => dir.join(format!("decanter-{day}.log")),
        None => {
            let mut days: Vec<PathBuf> = fs::read_dir(&dir)
                .map_err(|err| Error::Read {
                    what: "ps.report.what.log_folder",
                    err,
                })?
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("decanter-") && name.ends_with(".log"))
                })
                .collect();
            days.sort();
            days.pop().ok_or(Error::NoFailure)?
        }
    };
    Ok(DayLog::parse(&read_capped(&path, "ps.report.what.day_log")?).failures)
}

fn logs_dir() -> PathBuf {
    decanter_core::bootstrap::library_data_dir(decanter_ps::paths::active().library()).join("logs")
}

pub fn reports_dir() -> PathBuf {
    decanter_core::bootstrap::library_data_dir(decanter_ps::paths::active().library()).join("reports")
}

/// Writes the report to `out`, or to `<library>/decanter/reports/<date>-<fingerprint>.json`.
/// A file that exists is never replaced: the player may have kept it on purpose.
pub fn save(built: &Built, out: Option<&Path>) -> Result<PathBuf, Error> {
    let path = out.map_or_else(
        || reports_dir().join(format!("{}-{}.json", built.date, built.fingerprint)),
        Path::to_path_buf,
    );
    let write = |path: &Path| -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(built.json.as_bytes())?;
        file.write_all(b"\n")
    };
    match write(&path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), fingerprint = built.fingerprint, "saved an error report");
            Ok(path)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(Error::Exists(path)),
        Err(err) => Err(Error::Write { path, err }),
    }
}
