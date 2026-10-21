//! Each library folder's cache location, as Almacenamiento sets it and as
//! launches read it: games' shader caches go to the location of the folder
//! the game lives in (see [`decanter_core::shader_cache_env`]).

use std::path::{Path, PathBuf};

use crate::i18n::Text;

/// `folder`'s cache location, and whether the user picked it rather than
/// it being the default inside the folder.
pub fn location(folder: &Path) -> (PathBuf, bool) {
    let chosen = crate::core::db().ok().and_then(|db| {
        db.library_cache(folder)
            .inspect_err(|err| tracing::warn!(folder = %folder.display(), %err, "could not read the cache location, using the default"))
            .ok()
            .flatten()
    });
    chosen.map_or_else(
        || (decanter_core::default_cache_root(folder), false),
        |chosen| (PathBuf::from(chosen), true),
    )
}

/// The cache folder of a game living in `folder`, made if missing so the
/// driver can write to it on first launch.
pub fn game_dir(folder: &Path, group: &str, id: &str) -> Option<PathBuf> {
    let dir = decanter_core::game_cache_dir(&location(folder).0, group, id)?;
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(err) => {
            tracing::warn!(dir = %dir.display(), %err, "could not create the game's cache folder, the drivers keep their own");
            None
        }
    }
}

/// The shader cache variables for a PC game's launch, pointed into its
/// cache folder in the location of the library folder holding it.
#[cfg(proton)]
pub fn launch_env(game: &decanter_core::InstalledGame) -> Vec<(String, String)> {
    let folder = match crate::core::library_for_account(&game.account_id) {
        Ok(library) => library
            .root_of(&game.account_id, &game.game_id)
            .unwrap_or_else(|| library.root().to_path_buf()),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library to place the game's cache in, the drivers keep their own");
            return Vec::new();
        }
    };
    game_dir(&folder, decanter_core::cache_group(game.storefront), &game.game_id)
        .map(|dir| {
            tracing::debug!(game_id = %game.game_id, dir = %dir.display(), "shader caches for this launch");
            decanter_core::shader_cache_env(&dir)
                .map(|(name, value)| (name.to_owned(), value))
                .collect()
        })
        .unwrap_or_default()
}

/// Points `folder`'s caches at `picked`, or back at its default with `None`.
/// `picked` comes from a folder dialog, so it is checked here, once: an
/// absolute, writable directory outside any game's files. Picking one of
/// the profile's library folders means that folder's own cache location.
pub fn set(profile_id: &str, folder: &Path, picked: Option<&Path>) -> Result<(), Text> {
    let db = crate::achv_try!(crate::core::db());
    let Some(picked) = picked else {
        return db
            .set_library_cache(folder, None)
            .map_err(|err| Text::new("storage.cache.errors.save").arg("error", err));
    };
    if !picked.is_absolute() || !picked.is_dir() {
        return Err(Text::new("storage.cache.errors.not_dir").arg("path", picked.display()));
    }
    let roots: Vec<PathBuf> = crate::core::library_for_profile(profile_id)
        .map(|library| library.roots())
        .unwrap_or_default()
        .into_iter()
        .chain(
            db.profile_extra_libraries(profile_id)
                .unwrap_or_default()
                .into_iter()
                .map(PathBuf::from),
        )
        .collect();
    let inside_games = roots.iter().any(|root| {
        [
            root.join("common"),
            root.join("compatdata"),
            root.join("manifests"),
            decanter_ps::paths::games_in(root),
        ]
        .iter()
        .any(|games| picked.starts_with(games))
    });
    if inside_games {
        return Err(Text::new("storage.cache.errors.inside_games").arg("path", picked.display()));
    }
    let cache = roots
        .iter()
        .find(|root| root.as_path() == picked)
        .map_or_else(|| picked.to_path_buf(), |root| decanter_core::default_cache_root(root));
    crate::achv_try!(std::fs::create_dir_all(&cache).map_err(|err| {
        Text::new("messages.errors.create_dir")
            .arg("path", cache.display())
            .arg("error", err)
    }));
    let probe = cache.join(".decanter-write-check");
    crate::achv_try!(
        std::fs::write(&probe, b"")
            .and_then(|()| std::fs::remove_file(&probe))
            .map_err(|err| Text::new("storage.cache.errors.not_writable")
                .arg("path", cache.display())
                .arg("error", err))
    );
    let chosen = (cache != decanter_core::default_cache_root(folder)).then_some(cache.as_path());
    db.set_library_cache(folder, chosen)
        .map_err(|err| Text::new("storage.cache.errors.save").arg("error", err))
}
