//! The automatic part of the Steam compatibility switches (decision #1588), run at startup and on
//! every Storage rescan rather than by a watcher, so a card or drive mounted since is picked up
//! the next time either happens. Nothing here runs without a Steam install.
//!
//! - Finding Steam's games: every library folder Steam lists that is mounted becomes a library
//!   folder of the profile, and every game Steam has installed a Steam entry. One the user removed
//!   stays removed. Steam's own install is never made a library folder, since its `steamapps`
//!   holds Steam's list of library folders, which a library's `libraryfolders.vdf` would replace;
//!   its games are added all the same.
//! - Listing decanter's library folders in Steam: each one that is a `steamapps` folder goes into
//!   Steam's `libraryfolders.vdf` by its parent, the way Steam names a library folder; turned off,
//!   or a folder removed, only what decanter added comes out.
//!
//! Blocking: reads Steam's and the library's files.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use decanter_core::{Db, Library, SteamMark, steam};
use gpui::{App, AppContext as _};

use crate::state::AppState;

static SYNC: Mutex<()> = Mutex::new(());

/// Runs whatever the switches leave automatic, finding games for `profile_id` when there is one,
/// and returns whether the profile got library folders or Steam entries the library shown has to
/// read again.
pub fn sync(profile_id: Option<&str>) -> bool {
    // Startup, a Storage rescan and a switch toggled can each start one; each reads Steam's list
    // and writes it back, so two at once would lose one's change.
    let _serialized = SYNC.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(steam_root) = steam::steam_root() else {
        tracing::debug!("no steam installation, nothing for steam compatibility to do");
        return false;
    };
    let db = match crate::core::db() {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(%err, "cannot open the database, steam compatibility skipped");
            return false;
        }
    };
    let automatic = match db.steam_automatic() {
        Ok(automatic) => automatic,
        Err(err) => {
            tracing::warn!(%err, "cannot read the steam compatibility settings, skipped");
            return false;
        }
    };
    let mut is_changed = false;
    if automatic.discover
        && let Some(profile_id) = profile_id
    {
        is_changed |= add_folders(&db, &steam_root, profile_id) > 0;
        is_changed |= crate::external::steam::entries::add_installed(profile_id) > 0;
    }
    register_folders(&db, &steam_root, automatic.register_folders);
    is_changed
}

/// [`sync`] once at startup, on a background thread, then the library shown re-read if the active
/// profile got anything from it.
pub fn on_start(cx: &App) {
    let state = AppState::get(cx).clone();
    let profile_id = state.session.read(cx).active_profile.clone();
    let work = cx.background_spawn(async move { sync(profile_id.as_deref()) });
    cx.spawn(async move |cx| {
        if work.await {
            state
                .library
                .update(cx, |library, cx| library.refresh_all_install_state(cx));
        }
    })
    .detach();
}

/// The library folders decanter added to Steam's list; empty when they can't be read, which leaves
/// every entry of that list Steam's.
fn registered_folders() -> Vec<PathBuf> {
    crate::core::db()
        .and_then(|db| db.steam_registered_folders().map_err(crate::core::StoreError::Query))
        .inspect_err(|err| tracing::warn!(%err, "cannot read the library folders decanter added to steam"))
        .unwrap_or_default()
}

/// [`steam::apply_pending_and_restart_steam`] with the library folders decanter added, which are
/// then forgotten where the list that landed no longer has them: no longer decanter's to take out.
///
/// # Errors
/// As [`steam::apply_pending_and_restart_steam`].
pub fn apply_pending_and_restart(
    steam_root: &Path,
    user_id: Option<&str>,
    user_confirmed_closed: bool,
) -> Result<(), steam::ApplyError> {
    let _serialized = SYNC.lock().unwrap_or_else(PoisonError::into_inner);
    let own = registered_folders();
    steam::apply_pending_and_restart_steam(steam_root, user_id, &own, user_confirmed_closed)?;
    let listed: Vec<PathBuf> = steam::library_folders(steam_root)
        .into_iter()
        .map(|folder| folder.path)
        .collect();
    let kept: Vec<PathBuf> = own.iter().filter(|path| listed.contains(path)).cloned().collect();
    if kept.len() == own.len() {
        return Ok(());
    }
    let saved = crate::core::db().and_then(|db| {
        db.set_steam_registered_folders(&kept)
            .map_err(crate::core::StoreError::Query)
    });
    if let Err(err) = saved {
        tracing::warn!(%err, "cannot record the library folders decanter added to steam");
    }
    Ok(())
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every library folder `profile_id` keeps, one whose disk is unplugged too.
fn profile_roots(db: &Db, profile_id: &str) -> Vec<PathBuf> {
    let extra = db
        .profile_extra_libraries(profile_id)
        .inspect_err(|err| tracing::warn!(profile_id, %err, "cannot read the profile's library folders"))
        .unwrap_or_default();
    crate::core::library_for_profile(profile_id)
        .map(|library| library.roots())
        .unwrap_or_default()
        .into_iter()
        .chain(extra.into_iter().map(PathBuf::from))
        .collect()
}

/// Adds each library folder Steam lists that is mounted and not one of `profile_id`'s yet, nor in
/// one, nor holding one, returning how many it added.
fn add_folders(db: &Db, steam_root: &Path, profile_id: &str) -> usize {
    let removed = match db.steam_marked(profile_id, SteamMark::RemovedFolder) {
        Ok(removed) => removed,
        Err(err) => {
            tracing::warn!(profile_id, %err, "cannot read removed library folders, steam's not added");
            return 0;
        }
    };
    let own = registered_folders();
    let steam_own = canonical(steam_root);
    let mut known: Vec<PathBuf> = profile_roots(db, profile_id)
        .iter()
        .map(|root| canonical(root))
        .collect();
    let (mut added, mut unavailable, mut skipped) = (0_usize, 0_usize, 0_usize);
    for folder in steam::library_folders(steam_root) {
        if own.contains(&folder.path) || canonical(&folder.path) == steam_own {
            continue;
        }
        if !folder.is_present {
            unavailable = unavailable.saturating_add(1);
            continue;
        }
        let root = Library::root_under(&folder.path);
        let item = root.to_string_lossy();
        if removed.contains(item.as_ref()) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        let root_canonical = canonical(&root);
        if let Some(existing) = known
            .iter()
            .find(|existing| existing.starts_with(&root_canonical) || root_canonical.starts_with(existing))
        {
            if *existing == root_canonical {
                mark_steam_folder(db, profile_id, &item);
            }
            continue;
        }
        match crate::core::add_library_folder(profile_id, &folder.path) {
            Ok(root) => {
                tracing::info!(profile_id, root = %root.display(), "added a library folder of steam's");
                mark_steam_folder(db, profile_id, &root.to_string_lossy());
                known.push(root_canonical);
                added = added.saturating_add(1);
            }
            Err(err) => {
                tracing::warn!(profile_id, folder = %folder.path.display(), %err, "could not add a library folder of steam's");
            }
        }
    }
    tracing::debug!(
        profile_id,
        added,
        unavailable,
        removed_by_user = skipped,
        "checked steam's library folders"
    );
    added
}

fn mark_steam_folder(db: &Db, profile_id: &str, root: &str) {
    if let Err(err) = db.set_steam_marked(profile_id, SteamMark::SteamFolder, root, true) {
        tracing::warn!(profile_id, root, %err, "could not record a library folder as steam's");
    }
}

/// Every library folder of every profile that Steam can list (a `steamapps` folder, named by its
/// parent), except Steam's own library folders: one the user took out of Steam stays out.
fn decanter_folders(db: &Db) -> Vec<PathBuf> {
    let profiles = match db.list_profiles() {
        Ok(profiles) => profiles,
        Err(err) => {
            tracing::warn!(%err, "cannot list profiles to list their library folders in steam");
            return Vec::new();
        }
    };
    let steams = match db.steam_marked_anywhere(SteamMark::SteamFolder) {
        Ok(steams) => steams,
        Err(err) => {
            tracing::warn!(%err, "cannot read which library folders are steam's, none listed in steam");
            return Vec::new();
        }
    };
    let mut folders: Vec<PathBuf> = Vec::new();
    for root in profiles.iter().flat_map(|profile| profile_roots(db, &profile.id)) {
        if steams.contains(root.to_string_lossy().as_ref()) || root.file_name().is_none_or(|name| name != "steamapps") {
            continue;
        }
        if let Some(folder) = root
            .parent()
            .filter(|folder| !folders.iter().any(|known| known == folder))
        {
            folders.push(folder.to_path_buf());
        }
    }
    folders
}

fn register_folders(db: &Db, steam_root: &Path, is_enabled: bool) {
    let own = match db.steam_registered_folders() {
        Ok(own) => own,
        Err(err) => {
            tracing::warn!(%err, "cannot read the library folders decanter added to steam, steam's list left alone");
            return;
        }
    };
    let wanted = if is_enabled { decanter_folders(db) } else { Vec::new() };
    if own.is_empty() && wanted.is_empty() {
        return;
    }
    let registration = match steam::register(steam_root, &own, &wanted) {
        Ok(registration) => registration,
        Err(err) => {
            tracing::warn!(%err, "could not list decanter's library folders in steam");
            return;
        }
    };
    // A staged change still has to take out of what Steam saves the folders it dropped, so those
    // stay recorded until it lands.
    let mut record: Vec<PathBuf> = if registration.is_staged {
        own.clone()
    } else {
        own.iter().filter(|path| wanted.contains(path)).cloned().collect()
    };
    record.extend(registration.added);
    record.sort();
    record.dedup();
    if record == own {
        return;
    }
    if let Err(err) = db.set_steam_registered_folders(&record) {
        tracing::warn!(%err, "cannot record the library folders decanter added to steam");
    }
}
