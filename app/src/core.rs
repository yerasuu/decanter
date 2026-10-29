//! decanter's storage, opened once at startup so every screen can assume
//! it's there.
//!
//! Two halves, and neither has a long-lived handle to share.
//! [`decanter_core::Db`] is a SQLite connection and isn't `Send`, and
//! [`decanter_core::Library`] is just a path — so screens make their own
//! for the query they're making, the way `library::data` always has. Whether
//! the database opened at all is resolved once, in `state::Session`.

use std::path::{Path, PathBuf};

use decanter_core::{Db, InstalledGame, Library, Profile, Storefront};

use crate::i18n::Text;

/// Why the database or a library can't be reached: logged as it is, worded for the UI by
/// [`StoreError::localized`], which is also what it becomes as one of the [`Text`] errors the
/// views show.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no library configured")]
    NoLibrary,
    #[error("library at {} is not mounted", .0.display())]
    Unavailable(PathBuf),
    #[error("database open failed: {0}")]
    Open(decanter_core::Error),
    #[error("database query failed: {0}")]
    Query(decanter_core::Error),
    #[error("{0} has no manifest")]
    NotInstalled(String),
    #[error("manifest write failed: {0}")]
    Manifest(std::io::Error),
}

impl StoreError {
    pub fn localized(&self) -> Text {
        match self {
            Self::NoLibrary => Text::new("messages.errors.no_library"),
            Self::Unavailable(root) => Text::new("messages.errors.library_unavailable").arg("path", root.display()),
            Self::Open(err) => Text::new("messages.errors.db_open").arg("error", err),
            Self::Query(err) => Text::verbatim(err),
            Self::NotInstalled(_) => Text::new("messages.errors.not_installed"),
            Self::Manifest(err) => Text::new("messages.errors.save_manifest").arg("error", err),
        }
    }
}

impl From<StoreError> for Text {
    fn from(err: StoreError) -> Self {
        err.localized()
    }
}

pub fn db() -> Result<Db, StoreError> {
    match Db::open_active() {
        Ok(db) => {
            // Called on nearly every read across the app, so success is
            // trace-level noise — only a failed open is worth a line.
            tracing::trace!("opened db");
            Ok(db)
        }
        Err(decanter_core::Error::LibraryUnavailable(_) | decanter_core::Error::NoLibrary) => Err(no_library()),
        Err(err @ decanter_core::Error::Sqlite(_)) => {
            // Every caller below either propagates this as a hard error or
            // swallows it into a default/empty state; this is the one place
            // that always sees it, so it's the one place that logs it.
            tracing::warn!(%err, "failed to open db");
            Err(StoreError::Open(err))
        }
    }
}

fn no_library() -> StoreError {
    decanter_core::bootstrap::unavailable_root().map_or(StoreError::NoLibrary, StoreError::Unavailable)
}

/// Creates a fresh profile and makes it the active one — the only way a
/// profile comes to exist: onboarding's first-run screen and the navbar
/// switcher's "+ Crear perfil" both call this rather than each rolling
/// their own id/timestamp/activation logic.
///
/// `library_path`: `Some` only when the user picked a folder in the
/// creation form instead of leaving it on the pre-filled default — `None`
/// leaves the profile's `library_path` column unset, same as every profile
/// created before this existed.
pub fn create_profile(name: &str, library_path: Option<&str>) -> Result<Profile, Text> {
    // The first profile makes the first library, where its database goes.
    let library_path = if decanter_core::bootstrap::location() == decanter_core::bootstrap::Location::Unconfigured {
        let root = crate::achv_try!(
            library_path
                .map(std::path::PathBuf::from)
                .or_else(Library::default_path)
                .ok_or_else(no_library)
        );
        crate::achv_try!(std::fs::create_dir_all(&root).map_err(|err| {
            Text::new("messages.errors.create_dir")
                .arg("path", root.display())
                .arg("error", err)
        }));
        crate::achv_try!(
            decanter_core::bootstrap::activate(&root)
                .map_err(|err| Text::new("messages.errors.save_libraries").arg("error", err))
        );
        None
    } else {
        library_path.filter(|path| !is_database_root(Path::new(path)))
    };
    let db = db()?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let profile = crate::achv_try!(
        db.upsert_profile(&id, name, now)
            .inspect_err(|err| tracing::error!(profile_name = name, %err, "failed to create profile"))
            .map_err(Text::verbatim)
    );
    if let Some(path) = library_path {
        crate::achv_try!(
            db.set_profile_library_path(&profile.id, Some(path))
                .inspect_err(|err| tracing::error!(profile_id = %profile.id, path, %err, "failed to set new profile's library path"))
                .map_err(Text::verbatim)
        );
    }
    crate::achv_try!(
        db.set_active_profile(&profile.id)
            .inspect_err(|err| tracing::error!(profile_id = %profile.id, %err, "failed to activate new profile"))
            .map_err(Text::verbatim)
    );
    decanter_core::bootstrap::remember_profile(&profile.id);
    tracing::info!(profile_id = %profile.id, profile_name = %profile.name, "profile created");
    Ok(profile)
}

/// Every known library with its profiles, the active one first. The other
/// libraries' databases are only read; one whose disk isn't there has
/// `None`, since nothing can say which profiles it holds until it's back.
pub fn profiles_by_library() -> Vec<(std::path::PathBuf, Option<Vec<Profile>>)> {
    let active = decanter_core::bootstrap::active_root();
    let own = db()
        .and_then(|db| db.list_profiles().map_err(crate::core::StoreError::Query))
        .inspect_err(|err| tracing::warn!(%err, "failed to list profiles"))
        .unwrap_or_default();
    let others = decanter_core::bootstrap::known_roots()
        .into_iter()
        .filter(|root| active.as_ref() != Some(root))
        .map(|root| {
            let path = decanter_core::bootstrap::db_path(&root);
            let profiles = root.is_dir().then(|| {
                if !path.is_file() {
                    return Vec::new();
                }
                Db::open_read_only(&path)
                    .and_then(|db| db.list_profiles())
                    .inspect_err(
                        |err| tracing::warn!(path = %path.display(), %err, "cannot read another library's profiles"),
                    )
                    .unwrap_or_default()
            });
            (root, profiles)
        });
    active
        .clone()
        .map(|root| (root, Some(own)))
        .into_iter()
        .chain(others)
        .collect()
}

/// Makes `profile_id`, which lives in the library at `root`, the active
/// profile, and that library the active one.
pub fn switch_library(root: &std::path::Path, profile_id: &str) -> Result<(), Text> {
    crate::achv_try!(
        Db::open(decanter_core::bootstrap::db_path(root))
            .and_then(|db| db.set_active_profile(profile_id))
            .map_err(|err| Text::new("messages.errors.db_open").arg("error", err))
    );
    crate::achv_try!(
        decanter_core::bootstrap::activate(root)
            .map_err(|err| Text::new("messages.errors.save_libraries").arg("error", err))
    );
    decanter_core::bootstrap::remember_profile(profile_id);
    Ok(())
}

/// The library the database lives in — what a profile that never chose a
/// location of its own uses, and the fallback for an account that (for
/// whatever reason) resolves to no profile at all.
pub fn library() -> Result<Library, StoreError> {
    match decanter_core::bootstrap::active_root() {
        Some(root) => Ok(Library::new(root)),
        None => {
            tracing::warn!("the active library isn't available");
            Err(no_library())
        }
    }
}

/// Whether `path` is the library the open database lives in, which a profile reaches with its
/// library path unset.
fn is_database_root(path: &Path) -> bool {
    decanter_core::bootstrap::active_root().is_some_and(|root| root == path)
}

/// Unsets every profile's library path that names the library its database lives in. Unset reads
/// the same, and keeps following that library when it moves; spelled out, the path goes stale
/// once the folder moves outside decanter, and the profile then finds none of its games there.
pub fn forget_redundant_library_paths() {
    if decanter_core::bootstrap::active_root().is_none() {
        return;
    }
    // `db()` already warned about why, if it failed.
    let Ok(db) = db() else {
        return;
    };
    let profiles = match db.list_profiles() {
        Ok(profiles) => profiles,
        Err(err) => {
            tracing::warn!(%err, "cannot list profiles to check their library paths");
            return;
        }
    };
    for profile in profiles.iter().filter(|profile| {
        profile
            .library_path
            .as_deref()
            .is_some_and(|path| is_database_root(Path::new(path)))
    }) {
        match db.set_profile_library_path(&profile.id, None) {
            Ok(()) => tracing::info!(
                profile_id = %profile.id,
                path = profile.library_path.as_deref(),
                "profile's library path is its database's own library, now unset to follow it"
            ),
            Err(err) => {
                tracing::warn!(profile_id = %profile.id, %err, "cannot unset the profile's redundant library path");
            }
        }
    }
}

/// The `Library` `profile_id` actually uses — its own chosen location if
/// it's ever set one, otherwise [`library`]'s shared default — together
/// with every other library folder it keeps games in.
pub fn library_for_profile(profile_id: &str) -> Result<Library, StoreError> {
    let db = db()?;
    let chosen = crate::achv_try!(
        db.profile_library_path(profile_id)
            .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to read profile library path, using default"))
            .map_err(StoreError::Query)
    );
    let primary = match chosen {
        Some(path) => Library::new(path),
        None => crate::achv_try!(library()),
    };
    // Only folders that are there: a disk that's unplugged reads as one
    // with nothing on it rather than failing the whole library.
    let extra = db
        .profile_extra_libraries(profile_id)
        .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to read extra library folders, using main only"))
        .unwrap_or_default()
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir());
    Ok(primary.with_extra_roots(extra))
}

/// The library folder `profile_id` chose for itself when it is not there (moved by hand, or its disk
/// unplugged), so callers can say so instead of reading it as a library with no games, or making it.
pub fn missing_library(profile_id: &str) -> Option<std::path::PathBuf> {
    let chosen = db().ok()?.profile_library_path(profile_id).ok().flatten()?;
    let path = std::path::PathBuf::from(chosen);
    (!path.is_dir()).then_some(path)
}

/// Makes `picked` (or the `steamapps` folder under it, the way Steam lays
/// out a library folder) one more place `profile_id` keeps games, and
/// returns that folder.
pub fn add_library_folder(profile_id: &str, picked: &std::path::Path) -> Result<std::path::PathBuf, Text> {
    let root = Library::root_under(picked);
    let library = library_for_profile(profile_id)?;
    // Nested folders would see each other's games twice, or miss them.
    if let Some(clash) = library
        .roots()
        .into_iter()
        .find(|existing| existing.starts_with(&root) || root.starts_with(existing))
    {
        return Err(Text::new("storage.errors.nested")
            .arg("path", root.display())
            .arg("folder", clash.display()));
    }
    crate::achv_try!(std::fs::create_dir_all(&root).map_err(|err| {
        Text::new("messages.errors.create_dir")
            .arg("path", root.display())
            .arg("error", err)
    }));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    crate::achv_try!(
        db()?
            .add_profile_library(profile_id, &root.to_string_lossy(), now)
            .map_err(|err| Text::new("storage.errors.add").arg("error", err))
    );
    if let Err(err) = library_for_profile(profile_id)
        .map_err(|err| err.to_string())
        .and_then(|library| library.write_libraryfolders().map_err(|err| err.to_string()))
    {
        // Informational only: decanter reads its folders from the database.
        tracing::warn!(profile_id, %err, "could not update libraryfolders.vdf");
    }
    Ok(root)
}

/// `profile_id`'s library folders whose disk isn't there — an unplugged
/// drive or card. Their games' manifests are on that disk too, so nothing
/// can say which games they are until it's back.
pub fn offline_library_folders(profile_id: &str) -> Vec<std::path::PathBuf> {
    db().ok()
        .and_then(|db| db.profile_extra_libraries(profile_id).ok())
        .unwrap_or_default()
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| !path.is_dir())
        .collect()
}

/// Stops keeping games in `root`. Whatever is on that disk stays there.
pub fn remove_library_folder(profile_id: &str, root: &std::path::Path) -> Result<(), Text> {
    crate::achv_try!(
        db()?
            .remove_profile_library(profile_id, &root.to_string_lossy())
            .map_err(|err| Text::new("storage.errors.remove").arg("error", err))
    );
    // Re-adding the folder later starts from its default cache again.
    if let Err(err) = db()
        .map_err(|err| err.to_string())
        .and_then(|db| db.set_library_cache(root, None).map_err(|err| err.to_string()))
    {
        tracing::warn!(profile_id, root = %root.display(), %err, "could not forget the folder's cache location");
    }
    if let Err(err) = library_for_profile(profile_id)
        .map_err(|err| err.to_string())
        .and_then(|library| library.write_libraryfolders().map_err(|err| err.to_string()))
    {
        tracing::warn!(profile_id, %err, "could not update libraryfolders.vdf");
    }
    Ok(())
}

/// Same as [`library_for_profile`], resolved from an account id instead —
/// every account-scoped call site in this module has one of these in
/// scope, not a `profile_id`. An external game is filed under its profile's
/// id in place of an account (see `crate::external`), so a profile id
/// resolves to that profile's own library too. Falls back to [`library`]
/// for an account that resolves to no profile (shouldn't happen in
/// practice, but an orphaned account is still better served by the shared
/// default than a hard failure).
pub fn library_for_account(account_id: &str) -> Result<Library, StoreError> {
    let db = db()?;
    let profile_id = crate::achv_try!(
        db.profile_id_for_account(account_id, Storefront::Gog)
            .inspect_err(|err| tracing::warn!(account_id, %err, "failed to resolve profile for account, using default"))
            .map_err(StoreError::Query)
    );
    if let Some(profile_id) = profile_id {
        return library_for_profile(&profile_id);
    }
    let is_profile = db
        .list_profiles()
        .inspect_err(|err| tracing::warn!(account_id, %err, "failed to list profiles, using default library"))
        .is_ok_and(|profiles| profiles.iter().any(|profile| profile.id == account_id));
    if is_profile {
        library_for_profile(account_id)
    } else {
        library()
    }
}

/// Everything this account has installed, newest manifest shape or not.
/// An unreadable library reads as "nothing installed", which is a better
/// failure than a screen that won't render.
pub fn installed(account_id: &str) -> std::collections::BTreeMap<String, InstalledGame> {
    library_for_account(account_id)
        .map(|library| {
            let games = library.scan(account_id);
            tracing::debug!(account_id, count = games.len(), "scanned installed games");
            games
        })
        // `library()` already warned about why; this is just the local fallout.
        .unwrap_or_else(|_| {
            tracing::debug!(account_id, "reporting nothing installed, library unavailable");
            Default::default()
        })
}

pub fn installed_game(account_id: &str, game_id: &str) -> Option<InstalledGame> {
    let Some(library) = library_for_account(account_id).ok() else {
        tracing::debug!(
            account_id,
            game_id,
            "reporting game as not installed, library unavailable"
        );
        return None;
    };
    installed_game_in(&library, account_id, game_id)
}

/// [`installed_game`] against a `library` the caller already resolved, so a pass over many games
/// reads the profile's library settings once instead of once per game.
pub fn installed_game_in(library: &Library, account_id: &str, game_id: &str) -> Option<InstalledGame> {
    // An external game's id is a uuid, never a GOG product id, so trying
    // both storefronts can't pick up the wrong game.
    library
        .read_manifest(account_id, Storefront::Gog, game_id)
        .or_else(|| library.read_manifest(account_id, Storefront::Unspecified, game_id))
}

/// Sets (or clears) which local folder syncs against this game's Cloud
/// Saves bucket — usually pre-filled from [`detect_cloud_save_locations`],
/// but always overridable by hand.
pub fn set_cloud_save_path(account_id: &str, product_id: &str, path: Option<&str>) -> Result<(), StoreError> {
    let library = crate::achv_try!(library_for_account(account_id));
    let mut current = crate::achv_try!(
        installed_game(account_id, product_id).ok_or_else(|| StoreError::NotInstalled(product_id.to_owned()))
    );
    current.cloud_save_path = path.filter(|value| !value.is_empty()).map(str::to_string);
    library
        .write_manifest(&current)
        .inspect(|()| tracing::debug!(account_id, product_id, "cloud save path updated"))
        .inspect_err(|err| tracing::error!(account_id, product_id, %err, "failed to save cloud save path"))
        .map_err(StoreError::Manifest)
}

/// Asks GOG's remote-config where this game keeps its saves. Empty means
/// the game has no auto-detectable location (cloud saves off for this
/// build, or GOG reports none) — the caller falls back to letting the user
/// pick a folder by hand.
pub async fn detect_cloud_save_locations(
    account_id: &str,
    game: &InstalledGame,
) -> Result<Vec<decanter_gog::save_location::SaveLocation>, Text> {
    let library = crate::achv_try!(library_for_account(account_id));
    let install_dir = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .ok_or_else(|| Text::new("messages.errors.install_dir"))
    );
    let folders = crate::achv_try!(user_folders(account_id, &game.game_id));
    tracing::debug!(account_id, game_id = %game.game_id, "detecting cloud save locations");
    decanter_gog::save_location::detect(account_id, &game.game_id, &install_dir, &folders)
        .await
        .inspect(|locations| {
            tracing::debug!(account_id, game_id = %game.game_id, found = locations.len(), "cloud save locations detected");
        })
        // Recoverable: the caller falls back to letting the user pick a folder by hand.
        .inspect_err(|err| tracing::warn!(account_id, game_id = %game.game_id, %err, "cloud save location detection failed"))
        .map_err(Text::verbatim)
}

/// The account folders a game sees: the ones inside its prefix under
/// Proton, else the user's own (Windows and macOS).
fn user_folders(account_id: &str, game_id: &str) -> Result<decanter_gog::save_location::UserFolders, Text> {
    if crate::launch::in_prefixes() {
        return prefix_for(account_id, game_id)
            .map(|prefix| decanter_gog::save_location::UserFolders::in_prefix(&prefix.drive_c()))
            .ok_or_else(|| Text::new("messages.errors.prefix"));
    }
    decanter_gog::save_location::UserFolders::native().ok_or_else(|| {
        tracing::warn!(account_id, game_id, "could not resolve this account's folders");
        Text::new("messages.errors.user_folders")
    })
}

/// Stamps the manifest with when cloud saves last synced cleanly — the
/// "changed since when" threshold the next sync classifies against.
pub fn record_cloud_save_sync(account_id: &str, product_id: &str, last_sync: i64) -> Result<(), StoreError> {
    let library = crate::achv_try!(library_for_account(account_id));
    let mut current = crate::achv_try!(
        installed_game(account_id, product_id).ok_or_else(|| StoreError::NotInstalled(product_id.to_owned()))
    );
    current.cloud_save_last_sync = Some(last_sync);
    library
        .write_manifest(&current)
        // Failure is already warned about by every caller of this function; just note the outcome here.
        .inspect(|()| tracing::debug!(account_id, product_id, last_sync, "cloud save sync timestamp recorded"))
        .map_err(StoreError::Manifest)
}

/// Where a game runs: its own prefix, per account, created by umu on first
/// launch.
pub fn prefix_for(account_id: &str, game_id: &str) -> Option<decanter_runtime::Prefix> {
    let Some(library) = library_for_account(account_id).ok() else {
        tracing::debug!(account_id, game_id, "no prefix, library unavailable");
        return None;
    };
    // A game a launcher installed runs in that launcher's prefix.
    let owner = installed_game(account_id, game_id)
        .and_then(|game| game.prefix_of)
        .unwrap_or_else(|| game_id.to_owned());
    let Some(compatdata) = library.compatdata(account_id, &owner) else {
        tracing::warn!(account_id, game_id, "could not resolve compatdata path for prefix");
        return None;
    };
    Some(decanter_runtime::Prefix::new(compatdata))
}

/// The host path to a game's executable, or `None` when the manifest names
/// none — the "instalado, pero sin ejecutable detectado" case.
///
/// A shortcut (`Storefront::Unspecified`) stores its own absolute host
/// path directly rather than a path relative to an install directory
/// under `common/` — it was never installed there, it points at wherever
/// the user's file already lived.
pub fn executable_for(game: &InstalledGame) -> Option<std::path::PathBuf> {
    let Some(executable) = game.launch_executable.as_deref() else {
        return None;
    };
    if game.storefront == Storefront::Unspecified {
        return Some(std::path::PathBuf::from(executable));
    }
    let Some(library) = library_for_account(&game.account_id).ok() else {
        tracing::debug!(game_id = %game.game_id, "no executable path, library unavailable");
        return None;
    };
    let Some(root) = library.install_dir(&game.install_dir) else {
        tracing::warn!(game_id = %game.game_id, install_dir = %game.install_dir, "could not resolve install directory");
        return None;
    };
    // Manifest paths are relative and forward-slashed; rejecting anything
    // that climbs keeps a hand-edited manifest from reaching outside the
    // install directory.
    let Some(relative) = decanter_runtime::sanitize_relative_path(executable) else {
        tracing::warn!(game_id = %game.game_id, executable, "launch executable path rejected as unsafe");
        return None;
    };
    Some(root.join(relative))
}

/// Which Proton a game runs on: its own pin, else the app-wide default,
/// else whatever umu resolves.
pub fn proton_for(game: &InstalledGame) -> decanter_runtime::Proton {
    proton_or_default(game.proton.clone())
}

/// `pin` if there is one, else the app-wide default, else whatever umu
/// resolves — [`proton_for`] without a manifest to read the pin from.
pub fn proton_or_default(pin: Option<String>) -> decanter_runtime::Proton {
    let pinned = pin.or_else(|| {
        // `db()` already warned about why, if it failed.
        let db = db().ok()?;
        db.default_proton()
            .inspect_err(|err| tracing::warn!(%err, "failed to read default proton"))
            .ok()
            .flatten()
    });
    match pinned {
        Some(proton) => decanter_runtime::Proton::Named(proton),
        None => decanter_runtime::Proton::Latest,
    }
}

/// Whether a Steam shortcut export is staged and waiting for Steam to be
/// restarted to take effect — see `decanter_core::steam::export`'s own
/// doc for why a write can't always go straight to `shortcuts.vdf`.
/// `false` for anything that can't be resolved (no Steam install, no
/// local user) — nothing pending is exactly what that looks like too.
#[must_use]
pub fn steam_shortcuts_pending() -> bool {
    let Some(steam_root) = decanter_core::steam::steam_root() else {
        return false;
    };
    let Some(user_id) = decanter_core::steam::local_user_id(&steam_root) else {
        return false;
    };
    decanter_core::steam::has_pending_changes(&steam_root, &user_id)
}

/// Why [`restart_steam_to_apply_pending`] didn't apply the staged change.
pub enum RestartSteamError {
    /// Whether Steam is open can't be told from here (a container without
    /// Steam's `.steam` folder); only the user can confirm it's closed.
    NeedsCloseConfirmation,
    Failed(Text),
}

/// Shuts Steam down, swaps the staged `shortcuts.vdf` into place, and
/// starts it back up — the button behind [`steam_shortcuts_pending`]'s
/// banner. Runs on a blocking worker since it waits (synchronously, with
/// its own timeout) for the real Steam process to exit.
pub async fn restart_steam_to_apply_pending(user_confirmed_closed: bool) -> Result<(), RestartSteamError> {
    crate::achv_try!(
        tokio::task::spawn_blocking(move || {
            let steam_root = decanter_core::steam::steam_root()
                .ok_or_else(|| RestartSteamError::Failed(Text::new("settings.general.steam.no_install")))?;
            let user_id = decanter_core::steam::local_user_id(&steam_root)
                .ok_or_else(|| RestartSteamError::Failed(Text::new("settings.general.steam.no_user")))?;
            decanter_core::steam::apply_pending_and_restart_steam(&steam_root, &user_id, user_confirmed_closed)
                .inspect_err(|err| tracing::error!(%err, "failed to restart steam to apply pending shortcuts"))
                .map_err(|err| match err {
                    decanter_core::steam::ApplyError::SteamStateUnknown => RestartSteamError::NeedsCloseConfirmation,
                    decanter_core::steam::ApplyError::Io(err) => RestartSteamError::Failed(Text::verbatim(err)),
                })
        })
        .await
        .map_err(|err| RestartSteamError::Failed(Text::new("common.errors.internal_task").arg("error", err)))
    )
}
