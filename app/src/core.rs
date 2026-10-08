//! decanter's storage, opened once at startup so every screen can assume
//! it's there.
//!
//! Two halves, and neither has a long-lived handle to share.
//! [`decanter_core::Db`] is a SQLite connection and isn't `Send`, and
//! [`decanter_core::Library`] is just a path — so screens make their own
//! for the query they're making, the way `library::data` always has. Whether
//! the database opened at all is resolved once, in `state::Session`.

use decanter_core::{Db, InstalledGame, Library, Profile, ProfileKind, Storefront, default_db_path};

pub fn db() -> Result<Db, String> {
    match Db::open(default_db_path()) {
        Ok(db) => {
            // Called on nearly every read across the app, so success is
            // trace-level noise — only a failed open is worth a line.
            tracing::trace!("opened db");
            Ok(db)
        }
        Err(err) => {
            // Every caller below either propagates this as a hard error or
            // swallows it into a default/empty state; this is the one place
            // that always sees it, so it's the one place that logs it.
            tracing::warn!(%err, "failed to open db");
            Err(format!("abriendo la base de datos: {err}"))
        }
    }
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
pub fn create_profile(name: &str, kind: ProfileKind, library_path: Option<&str>) -> Result<Profile, String> {
    let db = db()?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let profile = crate::achv_try!(
        db.upsert_profile(&id, name, kind, now)
            .inspect_err(|err| tracing::error!(profile_name = name, %err, "failed to create profile"))
            .map_err(|err| err.to_string())
    );
    if let Some(path) = library_path {
        crate::achv_try!(
            db.set_profile_library_path(&profile.id, Some(path))
                .inspect_err(|err| tracing::error!(profile_id = %profile.id, path, %err, "failed to set new profile's library path"))
                .map_err(|err| err.to_string())
        );
    }
    crate::achv_try!(
        db.set_active_profile(&profile.id)
            .inspect_err(|err| tracing::error!(profile_id = %profile.id, %err, "failed to activate new profile"))
            .map_err(|err| err.to_string())
    );
    tracing::info!(profile_id = %profile.id, profile_name = %profile.name, "profile created");
    Ok(profile)
}

/// Where a library lives when nothing more specific is known — every
/// profile before per-profile locations existed, and the fallback for an
/// account that (for whatever reason) resolves to no profile at all.
pub fn library() -> Result<Library, String> {
    match Library::default_path() {
        Some(path) => Ok(Library::new(path)),
        None => {
            tracing::warn!("no data directory for this platform");
            Err("no hay directorio de datos en esta plataforma".to_string())
        }
    }
}

/// The `Library` `profile_id` actually uses — its own chosen location if
/// it's ever set one, otherwise [`library`]'s shared default.
pub fn library_for_profile(profile_id: &str) -> Result<Library, String> {
    let db = db()?;
    let chosen = crate::achv_try!(
        db.profile_library_path(profile_id)
            .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to read profile library path, using default"))
            .map_err(|err| err.to_string())
    );
    match chosen {
        Some(path) => Ok(Library::new(path)),
        None => library(),
    }
}

/// Same as [`library_for_profile`], resolved from an account id instead —
/// every account-scoped call site in this module has one of these in
/// scope, not a `profile_id`. Falls back to [`library`] for an account
/// that resolves to no profile (shouldn't happen in practice, but an
/// orphaned account is still better served by the shared default than a
/// hard failure).
pub fn library_for_account(account_id: &str) -> Result<Library, String> {
    let db = db()?;
    let profile_id = crate::achv_try!(
        db.profile_id_for_account(account_id, Storefront::Gog)
            .inspect_err(|err| tracing::warn!(account_id, %err, "failed to resolve profile for account, using default"))
            .map_err(|err| err.to_string())
    );
    match profile_id {
        Some(profile_id) => library_for_profile(&profile_id),
        None => library(),
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
    library.read_manifest(account_id, Storefront::Gog, game_id)
}

/// Sets (or clears) which local folder syncs against this game's Cloud
/// Saves bucket — usually pre-filled from [`detect_cloud_save_locations`],
/// but always overridable by hand.
pub fn set_cloud_save_path(account_id: &str, product_id: &str, path: Option<&str>) -> Result<(), String> {
    let library = crate::achv_try!(library_for_account(account_id));
    let mut current =
        crate::achv_try!(installed_game(account_id, product_id).ok_or("el juego ya no figura como instalado"));
    current.cloud_save_path = path.filter(|value| !value.is_empty()).map(str::to_string);
    library
        .write_manifest(&current)
        .inspect(|()| tracing::debug!(account_id, product_id, "cloud save path updated"))
        .inspect_err(|err| tracing::error!(account_id, product_id, %err, "failed to save cloud save path"))
        .map_err(|err| format!("guardando el manifiesto: {err}"))
}

/// Asks GOG's remote-config where this game keeps its saves. Empty means
/// the game has no auto-detectable location (cloud saves off for this
/// build, or GOG reports none) — the caller falls back to letting the user
/// pick a folder by hand.
pub async fn detect_cloud_save_locations(
    account_id: &str,
    game: &InstalledGame,
) -> Result<Vec<decanter_gog::save_location::SaveLocation>, String> {
    let library = crate::achv_try!(library_for_account(account_id));
    let install_dir = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .ok_or("directorio de instalación inválido")
    );
    let prefix =
        crate::achv_try!(prefix_for(account_id, &game.game_id).ok_or("no se pudo resolver el prefijo de este juego"));
    let prefix_drive_c = prefix.drive_c();
    tracing::debug!(account_id, game_id = %game.game_id, "detecting cloud save locations");
    decanter_gog::save_location::detect(account_id, &game.game_id, &install_dir, &prefix_drive_c)
        .await
        .inspect(|locations| {
            tracing::debug!(account_id, game_id = %game.game_id, found = locations.len(), "cloud save locations detected");
        })
        // Recoverable: the caller falls back to letting the user pick a folder by hand.
        .inspect_err(|err| tracing::warn!(account_id, game_id = %game.game_id, %err, "cloud save location detection failed"))
        .map_err(|err| err.to_string())
}

/// Stamps the manifest with when cloud saves last synced cleanly — the
/// "changed since when" threshold the next sync classifies against.
pub fn record_cloud_save_sync(account_id: &str, product_id: &str, last_sync: i64) -> Result<(), String> {
    let library = crate::achv_try!(library_for_account(account_id));
    let mut current =
        crate::achv_try!(installed_game(account_id, product_id).ok_or("el juego ya no figura como instalado"));
    current.cloud_save_last_sync = Some(last_sync);
    library
        .write_manifest(&current)
        // Failure is already warned about by every caller of this function; just note the outcome here.
        .inspect(|()| tracing::debug!(account_id, product_id, last_sync, "cloud save sync timestamp recorded"))
        .map_err(|err| format!("guardando el manifiesto: {err}"))
}

/// Games added by hand — an arbitrary executable, not something decanter
/// installed. Kept apart from the GOG catalog entirely: a shortcut has no
/// product to fetch metadata for, so it's just whatever the user typed.
pub fn shortcuts(account_id: &str) -> Vec<InstalledGame> {
    let shortcuts: Vec<InstalledGame> = library_for_account(account_id)
        .map(|library| library.scan(account_id))
        .unwrap_or_else(|_| {
            tracing::debug!(account_id, "reporting no shortcuts, library unavailable");
            Default::default()
        })
        .into_values()
        .filter(|game| game.storefront == Storefront::Unspecified)
        .collect();
    tracing::debug!(account_id, count = shortcuts.len(), "listed shortcuts");
    shortcuts
}

/// Adds an executable as a shortcut. `id` is a fresh id for it — the
/// "product id" a real GOG game would have, standing in for one so a
/// shortcut can reuse every path that's keyed by (account, `game_id)`:
/// `compatdata`, playtime, the DLC-less manifest shape.
pub fn add_shortcut(account_id: &str, id: &str, name: &str, executable: &std::path::Path) -> Result<(), String> {
    let library = crate::achv_try!(library_for_account(account_id));
    let result = library
        .write_manifest(&InstalledGame {
            account_id: account_id.to_string(),
            storefront: Storefront::Unspecified,
            game_id: id.to_string(),
            name: name.to_string(),
            // Never actually used as a directory — a shortcut owns no
            // files under `common/` — but the manifest format needs some
            // single path component here, and the id is already unique.
            install_dir: id.to_string(),
            build_id: String::new(),
            size_on_disk: None,
            launch_executable: Some(executable.to_string_lossy().into_owned()),
            last_played: None,
            proton: None,
            branch: None,
            dlcs: Default::default(),
            cloud_save_path: None,
            cloud_save_last_sync: None,
        })
        .map_err(|err| format!("guardando el atajo: {err}"));
    match &result {
        Ok(()) => tracing::info!(account_id, id, name, "shortcut added"),
        Err(err) => tracing::error!(account_id, id, err, "failed to add shortcut"),
    }
    result
}

/// Removes a shortcut and its prefix. There are no `common/` files to
/// decide about — a shortcut never had any.
pub fn remove_shortcut(account_id: &str, id: &str) -> Result<(), String> {
    let library = crate::achv_try!(library_for_account(account_id));
    crate::achv_try!(
        library
            .remove_manifest(account_id, Storefront::Unspecified, id)
            .inspect_err(|err| tracing::error!(account_id, id, %err, "failed to remove shortcut manifest"))
            .map_err(|err| format!("borrando el atajo: {err}"))
    );
    if let Some(compatdata) = library.compatdata(account_id, id) {
        if let Err(err) = std::fs::remove_dir_all(&compatdata) {
            // Best-effort: the manifest is already gone, so the shortcut
            // itself vanished either way — this just leaves an orphan
            // prefix directory behind on disk.
            tracing::warn!(account_id, id, path = %compatdata.display(), %err, "could not remove shortcut prefix directory");
        }
    }
    tracing::info!(account_id, id, "shortcut removed");
    Ok(())
}

/// Where a game runs: its own prefix, per account, created by umu on first
/// launch.
pub fn prefix_for(account_id: &str, game_id: &str) -> Option<decanter_runtime::Prefix> {
    let Some(library) = library_for_account(account_id).ok() else {
        tracing::debug!(account_id, game_id, "no prefix, library unavailable");
        return None;
    };
    let Some(compatdata) = library.compatdata(account_id, game_id) else {
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
    let pinned = game.proton.clone().or_else(|| {
        // `db()` already warned about why, if it failed.
        let db = db().ok()?;
        db.default_proton()
            .inspect_err(|err| tracing::warn!(game_id = %game.game_id, %err, "failed to read default proton"))
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

/// Shuts Steam down, swaps the staged `shortcuts.vdf` into place, and
/// starts it back up — the button behind [`steam_shortcuts_pending`]'s
/// banner. Runs on a blocking worker since it waits (synchronously, with
/// its own timeout) for the real Steam process to exit.
pub async fn restart_steam_to_apply_pending() -> Result<(), String> {
    crate::achv_try!(
        tokio::task::spawn_blocking(|| {
            let steam_root = decanter_core::steam::steam_root()
                .ok_or_else(|| "no se encontro una instalacion de Steam".to_string())?;
            let user_id = decanter_core::steam::local_user_id(&steam_root)
                .ok_or_else(|| "no se pudo resolver el usuario local de Steam".to_string())?;
            decanter_core::steam::apply_pending_and_restart_steam(&steam_root, &user_id)
                .inspect_err(|err| tracing::error!(%err, "failed to restart steam to apply pending shortcuts"))
                .map_err(|err| err.to_string())
        })
        .await
        .map_err(|err| format!("la tarea interna fallo: {err}"))
    )
}
