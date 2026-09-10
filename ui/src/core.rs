//! achv's storage, opened once at startup so every screen can assume
//! it's there.
//!
//! Two halves, and neither has a long-lived handle to share.
//! [`achv_core::Db`] is a SQLite connection and isn't `Send`, and
//! [`achv_core::Library`] is just a path — so screens make their own
//! for the query they're making, the way `library::data` always has. What
//! this resource carries is the *readiness*: the database opened.

use achv_core::{Db, InstalledGame, Library, Profile, Storefront, default_db_path};

/// Marker for "storage is ready". Screens match on it to tell the loading
/// and failed-to-open states apart from a working app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Core;

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
pub fn create_profile(name: &str) -> Result<Profile, String> {
    let db = db()?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let profile = crate::achv_try!(
        db.upsert_profile(&id, name, now)
            .inspect_err(|err| tracing::error!(profile_name = name, %err, "failed to create profile"))
            .map_err(|err| err.to_string())
    );
    crate::achv_try!(
        db.set_active_profile(&profile.id)
            .inspect_err(|err| tracing::error!(profile_id = %profile.id, %err, "failed to activate new profile"))
            .map_err(|err| err.to_string())
    );
    tracing::info!(profile_id = %profile.id, profile_name = %profile.name, "profile created");
    Ok(profile)
}

pub fn library() -> Result<Library, String> {
    match Library::default_path() {
        Some(path) => Ok(Library::new(path)),
        None => {
            tracing::warn!("no data directory for this platform");
            Err("no hay directorio de datos en esta plataforma".to_string())
        }
    }
}

pub async fn init() -> Result<Core, String> {
    crate::achv_try!(db());
    tracing::debug!("storage ready");
    Ok(Core)
}

/// Everything this account has installed, newest manifest shape or not.
/// An unreadable library reads as "nothing installed", which is a better
/// failure than a screen that won't render.
pub fn installed(account_id: &str) -> std::collections::BTreeMap<String, InstalledGame> {
    library()
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
    let Some(library) = library().ok() else {
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
    let library = crate::achv_try!(library());
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
) -> Result<Vec<achv_gog::save_location::SaveLocation>, String> {
    let library = crate::achv_try!(library());
    let install_dir = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .ok_or("directorio de instalación inválido")
    );
    let prefix =
        crate::achv_try!(prefix_for(account_id, &game.game_id).ok_or("no se pudo resolver el prefijo de este juego"));
    let prefix_drive_c = prefix.drive_c();
    tracing::debug!(account_id, game_id = %game.game_id, "detecting cloud save locations");
    achv_gog::save_location::detect(account_id, &game.game_id, &install_dir, &prefix_drive_c)
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
    let library = crate::achv_try!(library());
    let mut current =
        crate::achv_try!(installed_game(account_id, product_id).ok_or("el juego ya no figura como instalado"));
    current.cloud_save_last_sync = Some(last_sync);
    library
        .write_manifest(&current)
        // Failure is already warned about by every caller of this function; just note the outcome here.
        .inspect(|()| tracing::debug!(account_id, product_id, last_sync, "cloud save sync timestamp recorded"))
        .map_err(|err| format!("guardando el manifiesto: {err}"))
}

/// Games added by hand — an arbitrary executable, not something achv
/// installed. Kept apart from the GOG catalog entirely: a shortcut has no
/// product to fetch metadata for, so it's just whatever the user typed.
pub fn shortcuts(account_id: &str) -> Vec<InstalledGame> {
    let shortcuts: Vec<InstalledGame> = library()
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
    let library = crate::achv_try!(library());
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
    let library = crate::achv_try!(library());
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
pub fn prefix_for(account_id: &str, game_id: &str) -> Option<achv_runtime::Prefix> {
    let Some(library) = library().ok() else {
        tracing::debug!(account_id, game_id, "no prefix, library unavailable");
        return None;
    };
    let Some(compatdata) = library.compatdata(account_id, game_id) else {
        tracing::warn!(account_id, game_id, "could not resolve compatdata path for prefix");
        return None;
    };
    Some(achv_runtime::Prefix::new(compatdata))
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
    let Some(library) = library().ok() else {
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
    let Some(relative) = achv_runtime::sanitize_relative_path(executable) else {
        tracing::warn!(game_id = %game.game_id, executable, "launch executable path rejected as unsafe");
        return None;
    };
    Some(root.join(relative))
}

/// Which Proton a game runs on: its own pin, else the app-wide default,
/// else whatever umu resolves.
pub fn proton_for(game: &InstalledGame) -> achv_runtime::Proton {
    let pinned = game.proton.clone().or_else(|| {
        // `db()` already warned about why, if it failed.
        let db = db().ok()?;
        db.default_proton()
            .inspect_err(|err| tracing::warn!(game_id = %game.game_id, %err, "failed to read default proton"))
            .ok()
            .flatten()
    });
    match pinned {
        Some(proton) => achv_runtime::Proton::Named(proton),
        None => achv_runtime::Proton::Latest,
    }
}
