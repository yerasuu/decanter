// SPDX-License-Identifier: GPL-3.0-or-later
//! The Amazon downloads a run left unfinished, found from the record each staging folder keeps, so
//! the app lists them after a restart beside GOG's and carries them on through [`super::install`],
//! or deletes them through [`super::discard`].

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use decanter_core::{Library, Storefront};
use decanter_runtime::install::{PauseReason, Paused, Room, UnfinishedDownload};
use decanter_runtime::journal::LockError;
use serde::{Deserialize, Serialize};

use super::{FILES, LOG, disk};
use crate::library::is_id;

const RECORD: &str = "download.json";

/// What a staging folder holds beyond its files: what the progress log can't say, so a restart
/// lists the download as it was left.
#[derive(Serialize, Deserialize)]
pub(super) struct Record {
    title: String,
    is_repair: bool,
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
    pub(super) fn new(title: &str, is_repair: bool) -> Self {
        Self {
            title: title.to_owned(),
            is_repair,
            total_bytes: None,
            stopped: None,
        }
    }

    pub(super) const fn planned(&mut self, total_bytes: u64) {
        self.total_bytes = Some(total_bytes);
    }

    pub(super) const fn stopped(&mut self, paused: &Paused) {
        self.stopped = Some(Stopped {
            reason: paused.reason,
            bytes_done: paused.bytes_done,
            room: paused.room,
        });
    }

    /// Writes it into `staging`. A failure costs only how the download is listed after a restart,
    /// so it is logged and the download goes on.
    pub(super) async fn save(&self, staging: &Path) {
        let path = staging.join(RECORD);
        let written = match serde_json::to_vec(self) {
            Ok(bytes) => decanter_runtime::journal::write_atomic(&path, &bytes).await,
            Err(err) => Err(io::Error::other(err)),
        };
        if let Err(err) = written {
            tracing::warn!(path = %path.display(), error = %err, "couldn't write the amazon download record");
        }
    }
}

/// Every unfinished Amazon download in `library`'s folders, for the app to list when it starts.
///
/// A staging folder without a readable record (from a build before records) is logged and left
/// out: nothing says what it was.
#[inline]
pub async fn unfinished_downloads(library: &Library) -> Vec<UnfinishedDownload> {
    let roots = library.roots();
    let scan = tokio::task::spawn_blocking(move || roots.iter().flat_map(|root| in_root(root)).collect::<Vec<_>>());
    match scan.await {
        Ok(found) => {
            tracing::info!(count = found.len(), "scanned for unfinished amazon downloads");
            found
        }
        Err(err) => {
            tracing::warn!(error = %err, "scanning for unfinished amazon downloads stopped");
            Vec::new()
        }
    }
}

fn in_root(root: &Path) -> Vec<UnfinishedDownload> {
    let mut found = Vec::new();
    for account in subfolders(&disk::downloading_dir(root)) {
        let Some(account_id) = name_of(&account) else {
            continue;
        };
        for staging in subfolders(&account) {
            let Some(product_id) = name_of(&staging) else {
                continue;
            };
            // Found by its folder, so one whose name a download would not be staged under is not
            // decanter's to resume.
            if !is_id(product_id)
                || disk::staging_dir(root, account_id, product_id).ok().as_deref() != Some(staging.as_path())
            {
                tracing::warn!(staging = %staging.display(), "not an amazon staging folder, skipping it");
                continue;
            }
            if let Some(download) = unfinished(root, account_id, product_id, &staging) {
                found.push(download);
            }
        }
    }
    found
}

fn unfinished(root: &Path, account_id: &str, product_id: &str, staging: &Path) -> Option<UnfinishedDownload> {
    let record = match read_record(staging) {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(staging = %staging.display(), error = %err, "amazon staging folder without a readable record, not listed");
            return None;
        }
    };
    let in_use = match decanter_runtime::journal::lock(staging) {
        Ok(_held) => false,
        Err(LockError::Busy(_)) => true,
        Err(err @ LockError::Io { .. }) => {
            tracing::warn!(staging = %staging.display(), error = %err, "couldn't tell whether an amazon download is running");
            false
        }
    };
    let (reason, bytes_done, room) = record.stopped.map_or_else(
        || (PauseReason::ClosedUnexpectedly, staged_bytes(staging), None),
        |stop| (stop.reason, stop.bytes_done, stop.room),
    );
    Some(UnfinishedDownload {
        storefront: Storefront::AmazonGames,
        account_id: account_id.to_owned(),
        product_id: product_id.to_owned(),
        parent_id: None,
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
        paused_since: last_written(staging),
        is_queued_behind_game: false,
    })
}

fn read_record(staging: &Path) -> io::Result<Record> {
    let bytes = std::fs::read(staging.join(RECORD))?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

/// What a run that recorded no stop had finished, since only a stop records its byte count.
fn staged_bytes(staging: &Path) -> u64 {
    let files = staging.join(FILES);
    match decanter_core::directory_size(&files) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
        Err(err) => {
            tracing::warn!(dir = %files.display(), error = %err, "couldn't measure an amazon staging folder");
            0
        }
    }
}

fn last_written(staging: &Path) -> Option<SystemTime> {
    [RECORD, LOG]
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

fn subfolders(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect(),
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %err, "couldn't list unfinished amazon downloads");
            }
            Vec::new()
        }
    }
}
