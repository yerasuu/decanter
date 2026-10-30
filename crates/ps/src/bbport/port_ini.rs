//! Which port ini a title's settings live in: `titles/<TITLE_ID>/psport.ini`, its own, for a
//! port that reads `PS_CONFIG`. The in-game menu writes into whatever file that names, so one
//! game's choices never reach another. A title's file starts as a copy of the user's defaults the
//! first time it is asked for, themselves made from the shared one, so nothing changes for the player
//! that day; the shared file stays what an older port reads.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Bbport, Ini, Names};
use crate::paths::Root;
use crate::port::Port;
use crate::title_id::TitleId;

/// Written by the in-game menu on every save; dropped when the UI language changes.
pub(super) const MENU_LANGUAGE: &str = "menu_language";

/// Whose settings a [`PortIni`] holds, and the launch settings with it
/// ([`super::LauncherSettings::of_title`]): a title keeps both files or neither.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IniScope {
    Title,
    /// Shared by every title, since the checkout's port doesn't read `PS_CONFIG`.
    OlderPort,
    /// Shared by every title, since the title id is not one a folder may be named after.
    UnusualTitleId,
}

pub struct PortIni {
    pub path: PathBuf,
    pub scope: IniScope,
}

impl PortIni {
    /// What the port is told as `PS_CONFIG`: only a title's own file, since a port without it
    /// reads the shared one anyway.
    #[must_use]
    pub fn config(&self) -> Option<&Path> {
        (self.scope == IniScope::Title).then_some(self.path.as_path())
    }
}

impl Bbport {
    /// `title_id`'s port ini, made from the defaults ([`Self::defaults_ini`]) when the title has
    /// none yet. File I/O: off the UI thread.
    ///
    /// # Errors
    /// Backing up the shared ini before the first title gets its own, making the defaults, or
    /// writing the title's copy.
    pub fn title_ini(root: &Root, title_id: &str) -> io::Result<PortIni> {
        let names = Names::of_blocking(&Port::Bbport.checkout(root));
        let shared = shared_ini(root, names);
        let id = match own_title(names, title_id) {
            Ok(id) => id,
            Err(scope) => return Ok(PortIni { path: shared, scope }),
        };
        let path = root.title(&id).join(names.ini());
        if !path.exists() {
            create_from_defaults(root, &path)?;
        }
        Ok(PortIni {
            path,
            scope: IniScope::Title,
        })
    }

    /// The port ini a title seen for the first time starts from, `defaults/psport.ini`. Made the
    /// first time it is asked for from the shared ini but its in-game menu language, which follows
    /// `PS_MENU_LANGUAGE` unless a menu save pins it: the same file new titles copied before, so
    /// nothing changes that day. File I/O: off the UI thread.
    ///
    /// # Errors
    /// Writing the defaults.
    pub fn defaults_ini(root: &Root) -> io::Result<PathBuf> {
        let path = root.defaults().join(Names::Psport.ini());
        if path.exists() {
            return Ok(path);
        }
        let shared = shared_ini(root, Names::Psport);
        let mut ini = Ini::load(&shared);
        ini.remove(MENU_LANGUAGE);
        match ini.save(&path) {
            Ok(()) => {
                tracing::info!(from = %shared.display(), to = %path.display(), "port settings defaults for new titles made");
                Ok(path)
            }
            // Another thread made the same file at the same moment and renamed it first.
            Err(_) if path.is_file() => Ok(path),
            Err(err) => Err(err),
        }
    }

    /// Drops the in-game menu's language from the shared ini and every title's, so the next run
    /// follows `PS_MENU_LANGUAGE` again: the menu writes `menu_language` whenever it saves any
    /// setting, which would otherwise keep it in the language that was current then. File I/O:
    /// off the UI thread.
    ///
    /// # Errors
    /// The first file that could not be rewritten; the rest are still visited.
    pub fn forget_menu_language(root: &Root) -> io::Result<()> {
        let titles = fs::read_dir(root.titles())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.parse::<TitleId>().is_ok())
            })
            .map(|entry| entry.path().join(Names::Psport.ini()))
            .filter(|path| path.is_file());
        let mut first_error = None;
        let shared = shared_ini(root, Names::of_blocking(&Port::Bbport.checkout(root)));
        for path in core::iter::once(shared).chain(titles) {
            if let Err(err) = forget_in(&path) {
                tracing::warn!(path = %path.display(), %err, "in-game menu language not dropped");
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// The folder name `title_id`'s own settings live under, or why they stay in the shared files:
/// a port spelled `names` that doesn't read `PS_CONFIG`, or an id no folder may be named after.
pub(super) fn own_title(names: Names, title_id: &str) -> Result<TitleId, IniScope> {
    if !matches!(names, Names::Psport) {
        return Err(IniScope::OlderPort);
    }
    title_id.parse().map_err(|err| {
        tracing::warn!(%err, "settings shared with every title");
        IniScope::UnusualTitleId
    })
}

/// The ini every title shared before each got its own, spelled as the checkout's port reads it
/// (`names`). A port that reads `psport.ini` gets a copy of the `bbport.ini` an older one left, so
/// the first edit here doesn't start from its defaults; the port makes the same copy at launch, and
/// an older tag keeps reading `bbport.ini`.
fn shared_ini(root: &Root, names: Names) -> PathBuf {
    let path = root.bbport_data().join(names.ini());
    let older = root.bbport_data().join(Names::EarlyPsport.ini());
    if matches!(names, Names::Psport) && !path.exists() && older.exists() {
        match fs::copy(&older, &path) {
            Ok(_) => tracing::info!(from = %older.display(), to = %path.display(), "port settings copied"),
            Err(err) => tracing::warn!(from = %older.display(), %err, "port settings not copied"),
        }
    }
    path
}

fn forget_in(path: &Path) -> io::Result<()> {
    let mut ini = Ini::load(path);
    if ini.get(MENU_LANGUAGE).is_none() {
        return Ok(());
    }
    ini.remove(MENU_LANGUAGE);
    ini.save(path)
}

/// The shared ini is backed up once, before the first title takes a copy of the defaults made from
/// it, since from then on decanter no longer writes it for a port that reads `PS_CONFIG`.
fn create_from_defaults(root: &Root, path: &Path) -> io::Result<()> {
    if !root.titles().exists() {
        back_up_shared(root, &[Names::Psport.ini(), Names::Bbport.ini()])?;
    }
    let defaults = Bbport::defaults_ini(root)?;
    let contents = fs::read(&defaults)?;
    match crate::files::write_atomic(path, &contents) {
        Ok(()) => {
            tracing::info!(from = %defaults.display(), to = %path.display(), "title port settings created");
            Ok(())
        }
        // Another thread made the same copy at the same moment and renamed it first.
        Err(_) if path.is_file() => Ok(()),
        Err(err) => Err(err),
    }
}

/// `name` in `id`'s own folder, a copy of the shared file the first time it is asked for, after a
/// backup of the shared one before the first title takes its copy. With no shared file there is
/// nothing to copy and the title's is left to its first save, since an empty file is not one the
/// port can read.
pub(super) fn title_copy(root: &Root, id: &TitleId, name: &str) -> io::Result<PathBuf> {
    let path = root.title(id).join(name);
    if path.exists() {
        return Ok(path);
    }
    if !any_title_has(root, name) {
        back_up_shared(root, &[name])?;
    }
    let shared = root.bbport_data().join(name);
    let contents = match fs::read(&shared) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(path),
        Err(err) => return Err(err),
    };
    match crate::files::write_atomic(&path, &contents) {
        Ok(()) => {
            tracing::info!(title_id = id.as_str(), from = %shared.display(), to = %path.display(), "title file created");
            Ok(path)
        }
        // Another thread made the same copy at the same moment and renamed it first.
        Err(_) if path.is_file() => Ok(path),
        Err(err) => Err(err),
    }
}

pub(super) fn any_title_has(root: &Root, name: &str) -> bool {
    fs::read_dir(root.titles())
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().join(name).is_file())
}

/// `backups/per-game-settings-<date>/` with those of the shared files `names` there are in the
/// port's data folder. A file already in it is kept, so a second attempt the same day never
/// replaces the first copy.
pub(super) fn back_up_shared(root: &Root, names: &[&str]) -> io::Result<()> {
    let dir = root.backups().join(format!("per-game-settings-{}", today()));
    fs::create_dir_all(&dir)?;
    let mut copied = 0_usize;
    for &name in names {
        let (from, to) = (root.bbport_data().join(name), dir.join(name));
        if !from.is_file() || to.exists() {
            continue;
        }
        fs::copy(&from, &to)?;
        copied = copied.saturating_add(1);
    }
    tracing::info!(dir = %dir.display(), copied, "shared settings backed up before per-title settings");
    Ok(())
}

/// `HHMMSS`, in UTC, telling apart two backups of one day.
pub(super) fn now_hms() -> String {
    let secs = crate::failures::unix_secs(std::time::SystemTime::now()) % 86_400;
    format!("{:02}{:02}{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// `YYYY-MM-DD`, in UTC.
pub(super) fn today() -> String {
    crate::failures::format_time(crate::failures::unix_secs(std::time::SystemTime::now()))
        .chars()
        .take(10)
        .collect()
}
