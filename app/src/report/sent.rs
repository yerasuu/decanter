//! The reports this machine has sent, in `<library>/decanter/reports/sent.jsonl`: the only place
//! the delete code of a report is kept, so the file is private to its owner where the platform
//! says so.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::send::{Error, is_hex, is_id, server_of};
use super::{Product, reports_dir};

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub delete_code: String,
    pub url: String,
    pub fingerprint: String,
    pub product: Product,
    /// The report's own day, which with the fingerprint tells that it was sent already.
    pub day: String,
}

impl Entry {
    /// A line of a file the player could have edited or damaged: it ends up in a request header and
    /// in a link, so it is held to the shapes the server hands out.
    fn is_valid(&self) -> bool {
        is_id(&self.id)
            && self.id.starts_with(self.product.id_letter())
            && is_hex(&self.delete_code, 32)
            && is_hex(&self.fingerprint, 16)
            && server_of(&self.url, &self.id).is_some()
            && chrono::NaiveDate::parse_from_str(&self.day, "%Y-%m-%d").is_ok()
    }
}

fn path() -> PathBuf {
    reports_dir().join("sent.jsonl")
}

fn failed(path: &Path, err: io::Error) -> Error {
    Error::Registry {
        path: path.to_owned(),
        err,
    }
}

fn private_options() -> OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut options = OpenOptions::new();
        options.mode(0o600);
        options
    }
    #[cfg(not(unix))]
    OpenOptions::new()
}

/// Windows has no modes, so the file's access list is cut to this user, SYSTEM and Administrators
/// with `icacls` (no `unsafe` and no crate for the Win32 security API). The well-known SIDs are
/// used for the last two because the group names are localised. `icacls` has no `--`, so the path
/// is checked instead, and the user name is held to a strict set of characters.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) {
    use std::os::windows::process::CommandExt as _;
    use std::process::Command;
    use std::sync::Once;

    /// `CREATE_NO_WINDOW`, so `icacls` doesn't flash a console over decanter.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    static WARNED: Once = Once::new();

    let user = std::env::var("USERNAME").unwrap_or_default();
    let is_user_valid = !user.is_empty()
        && user.len() <= 64
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        && !user.starts_with([' ', '-']);
    let is_path_valid = path.is_absolute()
        && path
            .to_str()
            .is_some_and(|text| !text.starts_with('-') && !text.contains('"'));
    let restricted = is_user_valid
        && is_path_valid
        && Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{user}:(F)"))
            .args(["*S-1-5-18:(F)", "*S-1-5-32-544:(F)"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|output| output.status.success());
    if !restricted {
        WARNED.call_once(|| tracing::warn!("could not restrict sent.jsonl to its owner; its access list is unchanged"));
    }
}

pub fn load() -> Result<Vec<Entry>, Error> {
    let path = path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(failed(&path, err)),
    };
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            serde_json::from_str::<Entry>(line)
                .ok()
                .filter(Entry::is_valid)
                .or_else(|| {
                    tracing::warn!("skipped a line of sent.jsonl that is not a sent report");
                    None
                })
        })
        .collect())
}

pub fn get(id: &str) -> Result<Option<Entry>, Error> {
    Ok(load()?.into_iter().find(|entry| entry.id == id))
}

pub fn find(fingerprint: &str, day: &str) -> Result<Option<Entry>, Error> {
    Ok(load()?
        .into_iter()
        .find(|entry| entry.fingerprint == fingerprint && entry.day == day))
}

pub fn record(entry: &Entry) -> Result<(), Error> {
    let path = path();
    let mut line = serde_json::to_string(entry).map_err(|err| failed(&path, io::Error::other(err)))?;
    line.push('\n');
    let append = || -> io::Result<()> {
        if let Some(dir) = path.parent() {
            decanter_core::private::create_dir(dir)?;
        }
        #[cfg(windows)]
        let is_new = !path.exists();
        private_options()
            .append(true)
            .create(true)
            .open(&path)?
            .write_all(line.as_bytes())?;
        #[cfg(windows)]
        if is_new {
            restrict_to_owner(&path);
        }
        Ok(())
    };
    append().map_err(|err| failed(&path, err))
}

/// Rewrites the file without `id`, through a private temporary file renamed over it, so a failure
/// half-way leaves the old file whole.
pub fn forget(id: &str) -> Result<(), Error> {
    let path = path();
    let kept = load()?.into_iter().filter(|entry| entry.id != id).try_fold(
        String::new(),
        |mut text, entry| -> Result<String, Error> {
            text.push_str(&serde_json::to_string(&entry).map_err(|err| failed(&path, io::Error::other(err)))?);
            text.push('\n');
            Ok(text)
        },
    )?;
    let rewrite = || -> io::Result<()> {
        let mut temp = path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        private_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?
            .write_all(kept.as_bytes())?;
        #[cfg(windows)]
        restrict_to_owner(&temp);
        fs::rename(&temp, &path)
    };
    rewrite().map_err(|err| failed(&path, err))
}
