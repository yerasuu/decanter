//! The user's defaults for titles seen for the first time, `defaults/settings.json` beside
//! `defaults/psport.ini` ([`crate::bbport::Bbport::defaults_ini`]): a title's files start as a copy
//! of them, once, so a later change reaches no title that already has its own. Made the first time
//! they are asked for from the shared file's keys a title inherits, which is what a new title copied
//! before they existed, so nothing changes that day. A key absent here is the port's own default, or
//! decanter's: absent `online_allow_plaintext` is on (#1303).

use std::io;
use std::path::{Path, PathBuf};

use super::{LauncherSettings, title};
use crate::paths::Root;

const FILE: &str = "settings.json";

pub(super) fn path(root: &Root) -> PathBuf {
    root.defaults().join(FILE)
}

impl LauncherSettings {
    /// The defaults' launch settings, made from the shared file where there are none yet. File
    /// I/O: off the UI thread.
    ///
    /// # Errors
    /// The shared file that can't be read or doesn't parse while the defaults are made, or the
    /// defaults' own file.
    pub fn defaults(root: &Root) -> io::Result<Self> {
        let path = path(root);
        if !path.exists() {
            create(root, &path)?;
        }
        Self::read_at(&path, true)
    }

    /// One change of the defaults as they are now. A key no title owns (shared, a file's own
    /// record) is dropped, since no title would ever take it from here.
    ///
    /// # Errors
    /// As [`Self::defaults`], and writing the file.
    pub fn edit_defaults(root: &Root, change: impl FnOnce(&mut Self)) -> io::Result<()> {
        let mut defaults = Self::defaults(root)?;
        change(&mut defaults);
        defaults.values.retain(|key, _| title::is_titles_own(key));
        defaults.write_at(root, &path(root))
    }
}

fn create(root: &Root, path: &Path) -> io::Result<()> {
    let shared = LauncherSettings::read(root)?;
    let values: serde_json::Map<_, _> = shared
        .values
        .into_iter()
        .filter(|(key, _)| title::is_inherited(key))
        .collect();
    let keys = values.len();
    match (LauncherSettings { values, is_title: true }).write_at(root, path) {
        Ok(()) => {
            tracing::info!(keys, to = %path.display(), "launch settings defaults for new titles made");
            Ok(())
        }
        // Another thread made the same file at the same moment and renamed it first.
        Err(_) if path.is_file() => Ok(()),
        Err(err) => Err(err),
    }
}
