//! The last commit each port built at, so a build that breaks once the checkout moved falls back
//! to it instead of leaving the port unusable, and the commit that broke, so the automatic update
//! doesn't move onto it again. Kept beside the checkouts, inside the library.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::os::linux::prebuilt::valid_commit;
use crate::paths::Root;
use crate::port::Port;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failed {
    pub commit: String,
    /// The failing build's log file name, under [`crate::run_log::dir`]: a name rather than a
    /// path, since the library may move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Record {
    /// Full commit of the last build that succeeded or was fetched, with nothing local in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub good: Option<String>,
    /// Set when the checkout was put back on `good` after this commit failed to build; cleared
    /// once a build succeeds anywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<Failed>,
}

fn valid_log_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|extension| extension == "log")
        && !name.starts_with('.')
        && !name.contains('/')
}

impl Record {
    fn path(root: &Root, port: Port) -> PathBuf {
        port.checkout(root)
            .with_file_name(format!("{}-builds.json", port.name().to_lowercase()))
    }

    /// Drops whatever doesn't hold up, since the commits reach git and the log name a path.
    fn validated(mut self, port: Port) -> Self {
        if self.good.as_deref().is_some_and(|commit| !valid_commit(commit)) {
            tracing::warn!(
                port = port.name(),
                good = self.good,
                "ignoring an invalid last built commit"
            );
            self.good = None;
        }
        if let Some(failed) = self.failed.as_mut() {
            if failed.log.as_deref().is_some_and(|name| !valid_log_name(name)) {
                tracing::warn!(
                    port = port.name(),
                    log = failed.log,
                    "ignoring an invalid failed build log name"
                );
                failed.log = None;
            }
            if !valid_commit(&failed.commit) {
                tracing::warn!(
                    port = port.name(),
                    failed = failed.commit,
                    "ignoring an invalid failed commit"
                );
                self.failed = None;
            }
        }
        self
    }

    fn read(root: &Root, port: Port) -> io::Result<Self> {
        match fs::read(Self::path(root, port)) {
            Ok(data) => match serde_json::from_slice::<Self>(&data) {
                Ok(record) => Ok(record.validated(port)),
                Err(err) => Err(io::Error::new(io::ErrorKind::InvalidData, err)),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    /// A missing file records nothing; an unreadable one too, said in the log. File I/O: off the
    /// UI thread.
    pub fn load(root: &Root, port: Port) -> Self {
        Self::read(root, port).unwrap_or_else(|err| {
            tracing::warn!(port = port.name(), %err, "could not read the port's build record");
            Self::default()
        })
    }

    /// One change against the file as it is now. Fails rather than overwrite a file it can't read
    /// or parse.
    fn edit(root: &Root, port: Port, change: impl FnOnce(&mut Self)) -> io::Result<()> {
        let mut record = match Self::read(root, port) {
            Ok(record) => record,
            Err(err) => return Err(err),
        };
        change(&mut record);
        match serde_json::to_string_pretty(&record) {
            Ok(mut text) => {
                text.push('\n');
                crate::files::write_atomic(&Self::path(root, port), text.as_bytes())
            }
            Err(err) => Err(io::Error::other(err)),
        }
    }

    /// After the port built, or was fetched, at `commit`. A commit other than the last good one
    /// ends any fallback: the port moved on. File I/O: off the UI thread.
    ///
    /// # Errors
    ///
    /// The record can't be read, parsed or written.
    pub fn built(root: &Root, port: Port, commit: &str) -> io::Result<()> {
        if Self::read(root, port).is_ok_and(|record| record.good.as_deref() == Some(commit)) {
            return Ok(());
        }
        Self::edit(root, port, |record| {
            record.good = Some(commit.to_owned());
            record.failed = None;
        })
    }

    /// Before the checkout is put back on the last good commit. File I/O: off the UI thread.
    ///
    /// # Errors
    ///
    /// The record can't be read, parsed or written.
    pub fn failed(root: &Root, port: Port, commit: &str, log: Option<String>) -> io::Result<()> {
        Self::edit(root, port, |record| {
            record.failed = Some(Failed {
                commit: commit.to_owned(),
                log: log.filter(|name| valid_log_name(name)),
            });
        })
    }

    /// Whether the automatic update should leave `commit` alone: it failed to build before.
    /// `commit` may be abbreviated, as git shows it, since git only abbreviates to a prefix no
    /// other object in the checkout shares.
    pub fn failed_at(&self, commit: &str) -> bool {
        !commit.is_empty()
            && self
                .failed
                .as_ref()
                .is_some_and(|failed| failed.commit.starts_with(commit))
    }

    pub fn failed_log(&self, root: &Root) -> Option<PathBuf> {
        self.failed
            .as_ref()
            .and_then(|failed| failed.log.as_deref())
            .map(|name| crate::run_log::dir(root).join(name))
    }
}
