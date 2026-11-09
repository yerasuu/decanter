//! A store's downloads a run left unfinished, found from the record each staging folder keeps, so
//! the app lists them after a restart beside GOG's and carries them on through the store's install,
//! or deletes them through its discard. Shared by the stores whose staging is
//! `<downloading>/<account>/<product>/` with a `download.json` record, a log and a `files/` folder.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use decanter_core::{Library, Storefront};
use serde::{Deserialize, Serialize};

use super::files::component;
use super::{PauseReason, Paused, Room, UnfinishedDownload};
use crate::journal::LockError;

const RECORD: &str = "download.json";

/// Where one store stages its downloads and how it names them.
pub struct Layout {
    pub storefront: Storefront,
    /// The store's name in logs.
    pub store: &'static str,
    /// Every account's staged downloads in a library root, apart from the other stores'.
    pub downloading: fn(&Path) -> PathBuf,
    /// Whether a folder name is one of the store's product ids.
    pub is_id: fn(&str) -> bool,
    /// The staging's log file and its folder of finished files.
    pub log: &'static str,
    pub files: &'static str,
}

impl Layout {
    /// Where `account_id`'s download of `product_id` is staged in `root`: outside `common/`, so a
    /// stopped download never leaves partial files where the game lives, and on its disk, so the
    /// finished files move in by rename.
    ///
    /// # Errors
    ///
    /// An id that can't be a folder name.
    #[inline]
    pub fn staging_dir(&self, root: &Path, account_id: &str, product_id: &str) -> io::Result<PathBuf> {
        Ok((self.downloading)(root)
            .join(component(account_id, "account id")?)
            .join(component(product_id, "product id")?))
    }
}

/// What a staging folder holds beyond its files: what the progress log can't say, so a restart
/// lists the download as it was left.
#[derive(Serialize, Deserialize)]
pub struct Record {
    title: String,
    is_repair: bool,
    /// The game a DLC goes into, so a restart lists it as that game's DLC.
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    total_bytes: Option<u64>,
    /// `None` while a run owns it, so a crash reads as one.
    #[serde(default)]
    stopped: Option<Stopped>,
}

#[derive(Serialize, Deserialize)]
struct Stopped {
    reason: PauseReason,
    bytes_done: u64,
    room: Option<Room>,
}

impl Record {
    #[inline]
    #[must_use]
    pub fn new(title: &str, is_repair: bool) -> Self {
        Self {
            title: title.to_owned(),
            is_repair,
            parent_id: None,
            total_bytes: None,
            stopped: None,
        }
    }

    #[inline]
    #[must_use]
    pub fn with_parent(mut self, parent_id: Option<&str>) -> Self {
        self.parent_id = parent_id.map(str::to_owned);
        self
    }

    #[inline]
    pub const fn planned(&mut self, total_bytes: u64) {
        self.total_bytes = Some(total_bytes);
    }

    #[inline]
    pub const fn stopped(&mut self, paused: &Paused) {
        self.stopped = Some(Stopped {
            reason: paused.reason,
            bytes_done: paused.bytes_done,
            room: paused.room,
        });
    }

    /// Writes it into `staging`. A failure costs only how the download is listed after a restart,
    /// so it is logged and the download goes on.
    #[inline]
    pub async fn save(&self, staging: &Path) {
        let path = staging.join(RECORD);
        let written = match serde_json::to_vec(self) {
            Ok(bytes) => crate::journal::write_atomic(&path, &bytes).await,
            Err(err) => Err(io::Error::other(err)),
        };
        if let Err(err) = written {
            tracing::warn!(path = %path.display(), error = %err, "couldn't write a download record");
        }
    }
}

/// Every unfinished download of `layout`'s store in `library`'s folders, for the app to list when
/// it starts.
///
/// A staging folder without a readable record (from a build before records) is logged and left
/// out: nothing says what it was.
#[inline]
pub async fn unfinished_downloads(library: &Library, layout: &'static Layout) -> Vec<UnfinishedDownload> {
    let roots = library.roots();
    let scan =
        tokio::task::spawn_blocking(move || roots.iter().flat_map(|root| in_root(root, layout)).collect::<Vec<_>>());
    match scan.await {
        Ok(found) => {
            tracing::info!(
                store = layout.store,
                count = found.len(),
                "scanned for unfinished downloads"
            );
            found
        }
        Err(err) => {
            tracing::warn!(store = layout.store, error = %err, "scanning for unfinished downloads stopped");
            Vec::new()
        }
    }
}

fn in_root(root: &Path, layout: &Layout) -> Vec<UnfinishedDownload> {
    let mut found = Vec::new();
    for account in subfolders(&(layout.downloading)(root), layout) {
        let Some(account_id) = name_of(&account) else {
            continue;
        };
        for staging in subfolders(&account, layout) {
            let Some(product_id) = name_of(&staging) else {
                continue;
            };
            // Found by its folder, so one whose name a download would not be staged under is not
            // decanter's to resume.
            if !(layout.is_id)(product_id)
                || layout.staging_dir(root, account_id, product_id).ok().as_deref() != Some(staging.as_path())
            {
                tracing::warn!(store = layout.store, staging = %staging.display(), "not a staging folder, skipping it");
                continue;
            }
            if let Some(download) = unfinished(root, account_id, product_id, &staging, layout) {
                found.push(download);
            }
        }
    }
    found
}

fn unfinished(
    root: &Path,
    account_id: &str,
    product_id: &str,
    staging: &Path,
    layout: &Layout,
) -> Option<UnfinishedDownload> {
    let record = match read_record(staging) {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(store = layout.store, staging = %staging.display(), error = %err, "staging folder without a readable record, not listed");
            return None;
        }
    };
    let in_use = match crate::journal::lock(staging) {
        Ok(_held) => false,
        Err(LockError::Busy(_)) => true,
        Err(err @ LockError::Io { .. }) => {
            tracing::warn!(store = layout.store, staging = %staging.display(), error = %err, "couldn't tell whether a download is running");
            false
        }
    };
    let (reason, bytes_done, room) = record.stopped.map_or_else(
        || (PauseReason::ClosedUnexpectedly, staged_bytes(staging, layout), None),
        |stop| (stop.reason, stop.bytes_done, stop.room),
    );
    Some(UnfinishedDownload {
        storefront: layout.storefront,
        account_id: account_id.to_owned(),
        product_id: product_id.to_owned(),
        parent_id: record.parent_id,
        title: record.title,
        is_repair: record.is_repair,
        is_language: false,
        build_id: None,
        reason,
        room,
        bytes_done,
        total_bytes: record.total_bytes,
        root: root.to_path_buf(),
        in_use,
        paused_since: last_written(staging, layout),
        is_queued_behind_game: false,
    })
}

fn read_record(staging: &Path) -> io::Result<Record> {
    let bytes = std::fs::read(staging.join(RECORD))?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

/// What a run that recorded no stop had finished, since only a stop records its byte count.
fn staged_bytes(staging: &Path, layout: &Layout) -> u64 {
    let files = staging.join(layout.files);
    match decanter_core::directory_size(&files) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
        Err(err) => {
            tracing::warn!(store = layout.store, dir = %files.display(), error = %err, "couldn't measure a staging folder");
            0
        }
    }
}

fn last_written(staging: &Path, layout: &Layout) -> Option<SystemTime> {
    [RECORD, layout.log]
        .iter()
        .filter_map(|name| {
            std::fs::metadata(staging.join(name))
                .and_then(|meta| meta.modified())
                .ok()
        })
        .max()
}

fn name_of(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

fn subfolders(dir: &Path, layout: &Layout) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect(),
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(store = layout.store, dir = %dir.display(), error = %err, "couldn't list unfinished downloads");
            }
            Vec::new()
        }
    }
}
