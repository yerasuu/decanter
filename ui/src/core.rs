//! achv's storage, opened once at startup so every screen can assume
//! it's there.
//!
//! Two halves, and neither has a long-lived handle to share.
//! [`achv_core::Db`] is a SQLite connection and isn't `Send`, and
//! [`achv_core::Library`] is just a path — so screens make their own
//! for the query they're making, the way `library::data` always has. What
//! this resource carries is the *readiness*: the database opened, and the
//! profiles bottles-next left behind were adopted.

use achv_core::{Db, InstalledGame, Library, Storefront, default_db_path};

/// Marker for "storage is ready". Screens match on it to tell the loading
/// and failed-to-open states apart from a working app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Core;

pub fn db() -> Result<Db, String> {
    Db::open(default_db_path()).map_err(|err| format!("abriendo la base de datos: {err}"))
}

pub fn library() -> Result<Library, String> {
    Library::default_path()
        .map(Library::new)
        .ok_or_else(|| "no hay directorio de datos en esta plataforma".to_string())
}

pub async fn init() -> Result<Core, String> {
    let db = crate::achv_try!(db());
    // Once, on the first run after achv stopped reading bottles-next's
    // config: without it every cached row would be keyed by a profile that
    // no longer exists. Not fatal — an app with nothing adopted still
    // works, it just starts empty.
    match achv_core::adopt::adopt_bottles_next_state(&db) {
        Ok(adopted) if !adopted.is_empty() => {
            tracing::info!("adopted {} profile(s) from bottles-next", adopted.profiles);
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("could not adopt bottles-next state: {err}"),
    }
    Ok(Core)
}

/// Everything this profile has installed, newest manifest shape or not.
/// An unreadable library reads as "nothing installed", which is a better
/// failure than a screen that won't render.
pub fn installed(profile_id: &str) -> std::collections::BTreeMap<String, InstalledGame> {
    library().map(|library| library.scan(profile_id)).unwrap_or_default()
}

pub fn installed_game(profile_id: &str, game_id: &str) -> Option<InstalledGame> {
    let Some(library) = library().ok() else { return None };
    library.read_manifest(profile_id, Storefront::Gog, game_id)
}

/// Sets (or clears) which local folder syncs against this game's Cloud
/// Saves bucket — usually pre-filled from [`detect_cloud_save_locations`],
/// but always overridable by hand.
pub fn set_cloud_save_path(profile_id: &str, product_id: &str, path: Option<&str>) -> Result<(), String> {
    let library = crate::achv_try!(library());
    let mut current =
        crate::achv_try!(installed_game(profile_id, product_id).ok_or("el juego ya no figura como instalado"));
    current.cloud_save_path = path.filter(|value| !value.is_empty()).map(str::to_string);
    library
        .write_manifest(&current)
        .map_err(|err| format!("guardando el manifiesto: {err}"))
}

/// Asks GOG's remote-config where this game keeps its saves. Empty means
/// the game has no auto-detectable location (cloud saves off for this
/// build, or GOG reports none) — the caller falls back to letting the user
/// pick a folder by hand.
pub async fn detect_cloud_save_locations(
    profile_id: &str,
    game: &InstalledGame,
) -> Result<Vec<achv_gog::save_location::SaveLocation>, String> {
    let library = crate::achv_try!(library());
    let install_dir = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .ok_or("directorio de instalación inválido")
    );
    let prefix =
        crate::achv_try!(prefix_for(profile_id, &game.game_id).ok_or("no se pudo resolver el prefijo de este juego"));
    let prefix_drive_c = prefix.drive_c();
    achv_gog::save_location::detect(profile_id, &game.game_id, &install_dir, &prefix_drive_c)
        .await
        .map_err(|err| err.to_string())
}

/// Stamps the manifest with when cloud saves last synced cleanly — the
/// "changed since when" threshold the next sync classifies against.
pub fn record_cloud_save_sync(profile_id: &str, product_id: &str, last_sync: i64) -> Result<(), String> {
    let library = crate::achv_try!(library());
    let mut current =
        crate::achv_try!(installed_game(profile_id, product_id).ok_or("el juego ya no figura como instalado"));
    current.cloud_save_last_sync = Some(last_sync);
    library
        .write_manifest(&current)
        .map_err(|err| format!("guardando el manifiesto: {err}"))
}

/// Games added by hand — an arbitrary executable, not something achv
/// installed. Kept apart from the GOG catalog entirely: a shortcut has no
/// product to fetch metadata for, so it's just whatever the user typed.
pub fn shortcuts(profile_id: &str) -> Vec<InstalledGame> {
    library()
        .map(|library| library.scan(profile_id))
        .unwrap_or_default()
        .into_values()
        .filter(|game| game.storefront == Storefront::Unspecified)
        .collect()
}

/// Adds an executable as a shortcut. `id` is a fresh id for it — the
/// "product id" a real GOG game would have, standing in for one so a
/// shortcut can reuse every path that's keyed by (profile, `game_id)`:
/// `compatdata`, playtime, the DLC-less manifest shape.
pub fn add_shortcut(profile_id: &str, id: &str, name: &str, executable: &std::path::Path) -> Result<(), String> {
    let library = crate::achv_try!(library());
    library
        .write_manifest(&InstalledGame {
            profile_id: profile_id.to_string(),
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
        .map_err(|err| format!("guardando el atajo: {err}"))
}

/// Removes a shortcut and its prefix. There are no `common/` files to
/// decide about — a shortcut never had any.
pub fn remove_shortcut(profile_id: &str, id: &str) -> Result<(), String> {
    let library = crate::achv_try!(library());
    crate::achv_try!(
        library
            .remove_manifest(profile_id, Storefront::Unspecified, id)
            .map_err(|err| format!("borrando el atajo: {err}"))
    );
    if let Some(compatdata) = library.compatdata(profile_id, id) {
        let _ = std::fs::remove_dir_all(compatdata);
    }
    Ok(())
}

/// Where a game runs: its own prefix, per profile, created by umu on first
/// launch.
pub fn prefix_for(profile_id: &str, game_id: &str) -> Option<achv_runtime::Prefix> {
    let Some(library) = library().ok() else { return None };
    let Some(compatdata) = library.compatdata(profile_id, game_id) else {
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
    let Some(library) = library().ok() else { return None };
    let Some(root) = library.install_dir(&game.install_dir) else {
        return None;
    };
    // Manifest paths are relative and forward-slashed; rejecting anything
    // that climbs keeps a hand-edited manifest from reaching outside the
    // install directory.
    let Some(relative) = achv_runtime::sanitize_relative_path(executable) else {
        return None;
    };
    Some(root.join(relative))
}

/// Which Proton a game runs on: its own pin, else the app-wide default,
/// else whatever umu resolves.
pub fn proton_for(game: &InstalledGame) -> achv_runtime::Proton {
    let pinned = game.proton.clone().or_else(|| {
        let Some(db) = db().ok() else { return None };
        db.default_proton().ok().flatten()
    });
    match pinned {
        Some(proton) => achv_runtime::Proton::Named(proton),
        None => achv_runtime::Proton::Latest,
    }
}
