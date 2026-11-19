//! Games someone put in a library folder by hand: a folder under a root's `common/` that no
//! manifest names (see [`decanter_core::Library::unclaimed_dirs`]), with a Windows executable
//! at its top, offered to add as an external game where it already is.
//!
//! Only looked for when the user scans a folder from Storage; what each scan found is kept for
//! the session in [`Proposals`], which the External games page lists.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{App, Global};

use super::launchers::Detected;

/// How many proposals one scan keeps; the rest are only counted.
const SHOWN: usize = 50;

/// Executables that ship beside a game but aren't it, by the start of their lowercased name.
const NOT_THE_GAME: [&str; 12] = [
    "unins",
    "uninstall",
    "vc_redist",
    "vcredist",
    "dxsetup",
    "dxwebsetup",
    "dotnetfx",
    "oalinst",
    "physx",
    "ue4prereqsetup",
    "unitycrashhandler",
    "crashpad_handler",
];

/// One folder found, with the library folder it sits in, so the game is filed there.
#[derive(Clone)]
pub struct Unmanaged {
    pub game: Detected,
    pub folder: PathBuf,
    pub root: PathBuf,
}

#[derive(Default)]
pub struct Found {
    pub games: Vec<Unmanaged>,
    /// How many more there were than [`SHOWN`].
    pub more: usize,
}

/// The last scan of each library folder, by profile and folder.
#[derive(Default)]
pub struct Proposals(BTreeMap<(String, PathBuf), Found>);

impl Global for Proposals {}

/// Keeps `found` as what `root` holds for `profile_id`, in place of an earlier scan of it.
pub fn store(profile_id: &str, root: PathBuf, found: Found, cx: &mut App) {
    cx.default_global::<Proposals>()
        .0
        .insert((profile_id.to_owned(), root), found);
}

/// Everything the scans so far found for `profile_id`, with how many more they held back.
pub fn proposed(profile_id: &str, cx: &App) -> Found {
    let mut proposed = Found::default();
    for found in cx
        .try_global::<Proposals>()
        .into_iter()
        .flat_map(|proposals| proposals.0.iter())
        .filter(|((profile, _), _)| profile == profile_id)
        .map(|(_, found)| found)
    {
        proposed.games.extend(found.games.iter().cloned());
        proposed.more = proposed.more.saturating_add(found.more);
    }
    proposed
}

/// Every unclaimed folder in `root`, one of `profile_id`'s library folders, whose game isn't
/// already an external game nor one the user dismissed. Blocking: lists `root`'s `common/`, the
/// top of every folder in it, and the profile's external games.
pub fn detect(profile_id: &str, root: &Path) -> Found {
    let library = match crate::core::library_for_profile(profile_id) {
        Ok(library) => library,
        Err(err) => {
            tracing::debug!(profile_id, %err, "no library to look for unmanaged games in");
            return Found::default();
        }
    };
    let dismissed = match crate::core::db()
        .and_then(|db| db.dismissed_folders(profile_id).map_err(crate::core::StoreError::Query))
    {
        Ok(dismissed) => dismissed,
        Err(err) => {
            // Without the list every dismissed folder would come back, so none is proposed.
            tracing::warn!(profile_id, %err, "could not read dismissed library folders");
            return Found::default();
        }
    };
    let added: HashSet<PathBuf> = super::list(profile_id)
        .iter()
        .filter_map(crate::core::executable_for)
        .collect();
    let mut games = library
        .unclaimed_dirs(root)
        .into_iter()
        .filter(|folder| !dismissed.contains(folder.to_string_lossy().as_ref()))
        // An external game pointed at an executable deeper inside the folder already is it.
        .filter(|folder| !added.iter().any(|executable| executable.starts_with(folder)))
        .filter_map(|folder| {
            let executable = main_executable(&folder)?;
            let name = folder.file_name()?.to_string_lossy().into_owned();
            Some(Unmanaged {
                game: Detected { name, executable },
                folder,
                root: root.to_path_buf(),
            })
        });
    let shown: Vec<_> = games.by_ref().take(SHOWN).collect();
    let more = games.count();
    tracing::info!(profile_id, root = %root.display(), count = shown.len(), more, "scanned library folder for unmanaged games");
    Found { games: shown, more }
}

/// The largest `.exe` at the top of `folder` that isn't a known redistributable, uninstaller or
/// crash handler. `None` when there is none, or two share the largest size, since then which one
/// is the game is a guess.
fn main_executable(folder: &Path) -> Option<PathBuf> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(path = %folder.display(), %err, "could not read library folder");
            return None;
        }
    };
    let mut best: Option<(u64, PathBuf)> = None;
    let mut tied = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !name.ends_with(".exe") || NOT_THE_GAME.iter().any(|helper| name.starts_with(helper)) {
            continue;
        }
        // `DirEntry::metadata` doesn't follow a symlink, so a link is never taken for the game.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let size = metadata.len();
        match &best {
            Some((largest, _)) if size < *largest => {}
            Some((largest, _)) if size == *largest => tied = true,
            _ => {
                best = Some((size, entry.path()));
                tied = false;
            }
        }
    }
    if tied {
        tracing::debug!(path = %folder.display(), "two executables share the largest size, none proposed");
        return None;
    }
    best.map(|(_, executable)| executable)
}

/// Remembers that `folder` isn't to be proposed again in `profile_id`, and stops proposing it.
pub fn dismiss(profile_id: &str, folder: &Path, cx: &mut App) -> Result<(), crate::core::StoreError> {
    crate::achv_try!(
        crate::achv_try!(crate::core::db())
            .dismiss_folder(profile_id, folder)
            .inspect_err(
                |err| tracing::error!(profile_id, folder = %folder.display(), %err, "could not dismiss folder")
            )
            .map_err(crate::core::StoreError::Query)
    );
    for ((profile, _), found) in &mut cx.default_global::<Proposals>().0 {
        if profile == profile_id {
            found.games.retain(|game| game.folder != folder);
        }
    }
    Ok(())
}
