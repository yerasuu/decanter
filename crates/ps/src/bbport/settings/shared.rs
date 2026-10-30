//! Migration step 4 of `docs/ps-per-game-settings.md`: the shared `bbport/settings.json` keeps only
//! what is still read from it once every title has its own file, after a copy of it into
//! `backups/per-game-settings-<date>/shared-settings-<HHMMSS>/`. The file keeps its shape, so an
//! older port's launcher still reads it (an absent key is its default).
//!
//! The cut-over is conservative: only for a checkout whose port reads `PS_CONFIG` (an older one's
//! launches read the shared file whole), once the defaults for new titles exist, every title of the
//! `titles` map has its own file and every title's file is of the current format (so none still
//! takes a key from here). Even then a key leaves only when the defaults or a title's file holds the
//! same value, so no value's last copy goes; the `titles` map goes whole, each entry being in its
//! title's file. The former GTK launcher's keys, which nothing reads, go whenever they are there.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value};

use super::title::{self, FILE};
use super::{LAUNCHER_PASSWORD, LauncherSettings, TITLES, defaults, path};
use crate::bbport::Names;
use crate::paths::Root;
use crate::port::Port;
use crate::title_id::TitleId;

/// Said once per run, since the check runs on every scan of the games.
static TOLD_KEPT: AtomicBool = AtomicBool::new(false);

impl LauncherSettings {
    /// Trims the shared file as this module says, when there is anything to trim. File I/O: off the
    /// UI thread.
    ///
    /// # Errors
    /// The shared file, the defaults or a title's file that can't be read or doesn't parse, or
    /// writing the backup or the shared file; the shared file is left as it was.
    pub fn trim_shared(root: &Root) -> io::Result<()> {
        let path = path(root);
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let shared: Map<String, Value> = serde_json::from_slice(&data)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {err}", path.display())))?;
        let leftovers: Vec<&str> = title::UNREAD
            .into_iter()
            .filter(|key| shared.contains_key(*key))
            .collect();
        let (moved, kept) = match blocker(root, &shared) {
            None => moved_keys(root, &shared)?,
            Some(reason) => {
                if !TOLD_KEPT.swap(true, Ordering::Relaxed) {
                    tracing::info!(
                        reason,
                        "shared launch settings keep the keys titles copied, until every title has its own file"
                    );
                }
                (Vec::new(), Vec::new())
            }
        };
        if leftovers.is_empty() && moved.is_empty() {
            return Ok(());
        }
        let backup = crate::bbport::fresh_backup_dir(root, "shared-settings")?.join(FILE);
        fs::write(&backup, &data)?;
        // Read again and written at once, so a shared key changed since (a port selection) stays.
        Self::edit(root, |settings| {
            for key in leftovers.iter().chain(&moved) {
                settings.values.remove(*key);
            }
        })?;
        tracing::info!(
            gtk_leftovers = leftovers.len(),
            moved = moved.len(),
            kept_only_copy = kept.len(),
            removed = ?leftovers.iter().chain(&moved).collect::<Vec<_>>(),
            kept = ?kept,
            backup = %backup.display(),
            "shared launch settings trimmed to what is still read from them"
        );
        Ok(())
    }
}

fn title_files(root: &Root) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(root.titles())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.parse::<TitleId>().is_ok())
        })
        .map(|entry| entry.path().join(FILE))
        .filter(|path| path.is_file())
}

/// Why the keys titles copied must stay in the shared file for now; `None` once nothing reads them.
fn blocker(root: &Root, shared: &Map<String, Value>) -> Option<&'static str> {
    if !matches!(Names::of_blocking(&Port::Bbport.checkout(root)), Names::Psport) {
        return Some("the checkout's port reads the shared file");
    }
    if !defaults::path(root).is_file() {
        return Some("no defaults for new titles yet");
    }
    let mapped = match shared.get(TITLES) {
        None => true,
        Some(Value::Object(titles)) => titles.keys().all(|id| {
            id.parse::<TitleId>()
                .is_ok_and(|id| root.title(&id).join(FILE).is_file())
        }),
        Some(_) => false,
    };
    if !mapped {
        return Some("a title of the titles map has no file of its own");
    }
    let current = title_files(root).all(|path| title::is_current(&path).unwrap_or(false));
    (!current).then_some("a title's file is of an earlier format or can't be read")
}

/// The keys titles copied that the defaults or a title's file hold with the same value, and those
/// whose only copy is here. What is still read from the shared file is never either.
fn moved_keys<'shared>(
    root: &Root,
    shared: &'shared Map<String, Value>,
) -> io::Result<(Vec<&'shared str>, Vec<&'shared str>)> {
    let copies = core::iter::once(defaults::path(root))
        .chain(title_files(root))
        .map(|path| LauncherSettings::read_at(&path, true).map(|settings| settings.values))
        .collect::<io::Result<Vec<_>>>()?;
    let mut moved = Vec::new();
    let mut kept = Vec::new();
    for (key, value) in shared {
        let key = key.as_str();
        if LauncherSettings::is_shared(key) || key == LAUNCHER_PASSWORD || title::UNREAD.contains(&key) {
            continue;
        }
        if key == TITLES || copies.iter().any(|copy| copy.get(key) == Some(value)) {
            moved.push(key);
        } else {
            kept.push(key);
        }
    }
    Ok((moved, kept))
}
