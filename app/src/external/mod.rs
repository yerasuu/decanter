//! Games added from outside any storefront: an executable the user points
//! at, or installs with its own setup program first, the way Heroic's
//! "add game" does. Each runs through the same runner as a GOG install —
//! its own prefix, its own Proton pin — but has no catalog entry behind
//! it, so its art comes from [`art`] instead.
//!
//! Filed under the profile itself rather than one of its accounts: an
//! external game isn't owned by any storefront, so there's no account to
//! pick, and the profile's id stands in for one in every `(account,
//! game_id)` key — manifest directory, `compatdata`, playtime. Its id is a
//! uuid, which no GOG product id ever looks like.

pub mod art;
pub mod import;
mod transfer;

use std::path::{Path, PathBuf};

use decanter_core::{Game, GameArt, InstallState, InstalledGame, Storefront};
use decanter_gog::{GameKind, LibraryEntry};

/// Every external game in `profile_id`. An unreadable library reads as
/// none, the same as [`crate::core::installed`].
pub fn list(profile_id: &str) -> Vec<InstalledGame> {
    let Ok(library) = crate::core::library_for_profile(profile_id) else {
        tracing::debug!(profile_id, "reporting no external games, library unavailable");
        return Vec::new();
    };
    let mut games: Vec<InstalledGame> = library
        .scan(profile_id)
        .into_values()
        .filter(|game| game.storefront == Storefront::Unspecified)
        .collect();
    games.sort_by_cached_key(|game| game.name.to_lowercase());
    tracing::debug!(profile_id, count = games.len(), "listed external games");
    games
}

/// The library row for an external game, shaped like a GOG one so every
/// shelf, filter and sort treats both alike.
pub fn entry(game: &InstalledGame) -> LibraryEntry {
    LibraryEntry {
        game: Game {
            account_id: game.account_id.clone(),
            background_url: game.art.hero.clone(),
            cover_url: game.art.cover.clone(),
            description: None,
            developer: None,
            genres: Vec::new(),
            icon_url: game.art.icon.clone(),
            id: game.game_id.clone(),
            install_state: Some(InstallState {
                installed: true,
                install_size_bytes: game.size_on_disk,
                ..Default::default()
            }),
            publisher: None,
            rating: None,
            release_date: None,
            storefront: Storefront::Unspecified,
            title: game.name.clone(),
        },
        kind: GameKind::Game,
        purchased_at: None,
        library_rank: None,
        installable: true,
        parent_id: None,
    }
}

/// What the add/edit form hands over.
pub struct Draft {
    pub name: String,
    pub executable: PathBuf,
    pub art: GameArt,
}

/// Writes `id`'s manifest — a new game, or an edit of one already there.
/// An edit keeps what the form doesn't show (Proton pin, last played).
pub fn save(profile_id: &str, id: &str, draft: Draft) -> Result<(), String> {
    let library = crate::achv_try!(crate::core::library_for_profile(profile_id));
    let previous = library.read_manifest(profile_id, Storefront::Unspecified, id);
    let game = InstalledGame {
        account_id: profile_id.to_owned(),
        storefront: Storefront::Unspecified,
        game_id: id.to_owned(),
        name: draft.name,
        // Never used as a directory — an external game owns no files
        // under `common/` — but the manifest format needs a single path
        // component here, and the id is already unique.
        install_dir: id.to_owned(),
        build_id: String::new(),
        size_on_disk: None,
        launch_executable: Some(draft.executable.to_string_lossy().into_owned()),
        last_played: previous.as_ref().and_then(|game| game.last_played),
        proton: previous.and_then(|game| game.proton),
        branch: None,
        dlcs: Default::default(),
        cloud_save_path: None,
        cloud_save_last_sync: None,
        art: draft.art,
    };
    library
        .write_manifest(&game)
        .inspect(|()| tracing::info!(profile_id, id, name = %game.name, "external game saved"))
        .inspect_err(|err| tracing::error!(profile_id, id, %err, "failed to save external game"))
        .map_err(|err| format!("guardando el juego: {err}"))
}

/// Forgets an external game and deletes its prefix — which is also where
/// anything its installer put under `C:\` lives. Files it was pointed at
/// outside the prefix are left alone: decanter never put them there.
pub fn remove(profile_id: &str, id: &str) -> Result<(), String> {
    let library = crate::achv_try!(crate::core::library_for_profile(profile_id));
    crate::achv_try!(
        library
            .remove_manifest(profile_id, Storefront::Unspecified, id)
            .inspect_err(|err| tracing::error!(profile_id, id, %err, "failed to remove external game manifest"))
            .map_err(|err| format!("borrando el juego: {err}"))
    );
    if let Some(compatdata) = library.compatdata(profile_id, id) {
        match std::fs::remove_dir_all(&compatdata) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            // Best-effort: the manifest is already gone, so the game
            // vanished either way — this only leaves an orphan prefix.
            Err(err) => {
                tracing::warn!(profile_id, id, path = %compatdata.display(), %err, "could not remove external game prefix")
            }
        }
    }
    tracing::info!(profile_id, id, "external game removed");
    Ok(())
}
/// Runs a setup program inside `id`'s prefix and waits for it, and for
/// anything it left running there, to finish. The prefix is the one the
/// game will then launch in, so whatever the installer writes under
/// `C:\` — files, registry keys, runtimes — is there for the game.
pub async fn run_installer(profile_id: &str, id: &str, installer: &Path, proton: Option<String>) -> Result<(), String> {
    let prefix =
        crate::achv_try!(crate::core::prefix_for(profile_id, id).ok_or("no se pudo resolver el prefijo del juego"));
    // Same `umu-<id>` the game itself launches as, so protonfixes treats
    // both as the same title.
    let launch = decanter_runtime::Launch::new(installer, format!("umu-{id}"))
        .store("none")
        .proton(crate::core::proton_or_default(proton));
    tracing::info!(profile_id, id, installer = %installer.display(), "running external game installer");
    let session = crate::achv_try!(
        decanter_runtime::start(&prefix, launch)
            .await
            .inspect_err(|err| tracing::error!(profile_id, id, %err, "installer failed to start"))
            .map_err(|err| format!("no se pudo iniciar el instalador: {err}"))
    );
    // Many installers exit non-zero on a perfectly good install (or when
    // the user closes a "finished" dialog), so a failure status is only
    // logged — what the user picks as the executable afterwards is what
    // decides whether the install worked.
    if let Err(err) = session.wait().await {
        tracing::warn!(profile_id, id, %err, "installer ended with a failure status");
    }
    tracing::info!(profile_id, id, "external game installer finished");
    Ok(())
}

/// Where an installer run through [`run_installer`] puts things, for the
/// executable picker to start in.
pub fn drive_c(profile_id: &str, id: &str) -> Option<PathBuf> {
    crate::core::prefix_for(profile_id, id).map(|prefix| prefix.drive_c())
}
