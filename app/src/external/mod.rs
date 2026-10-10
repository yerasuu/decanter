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
pub mod launchers;
pub mod transfer;
pub mod umu;

use std::path::{Path, PathBuf};

use decanter_core::{Game, GameArt, InstallState, InstalledGame, Storefront};
use decanter_gog::{GameKind, LibraryEntry};

/// The umu `GAMEID` every external game runs as: umu's own "unknown game"
/// id, which lets protonfixes look the game up in the UMU database rather
/// than under an id no database has.
pub const UMU_ID: &str = "umu-default";

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
    /// The UMU database entry it launches as; `None` is `umu-default`.
    pub umu: Option<UmuEntry>,
    /// The external game whose prefix this one shares — set for a game a
    /// launcher installed, which has to run where that launcher did.
    pub prefix_of: Option<String>,
    /// The library folder a new game goes in; `None` is the main one. An
    /// edit stays wherever the game already is.
    pub library_root: Option<PathBuf>,
}

/// One row of the UMU database, as far as launching cares.
#[derive(Clone, PartialEq, Eq)]
pub struct UmuEntry {
    pub id: String,
    /// `None` for umu's own store (the database's `none`).
    pub store: Option<String>,
}

/// Writes `id`'s manifest — a new game, or an edit of one already there.
/// An edit keeps what the form doesn't show (Proton pin, last played).
pub fn save(profile_id: &str, id: &str, draft: Draft) -> Result<(), String> {
    let library = crate::achv_try!(library_in(profile_id, draft.library_root.as_deref()));
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
        umu_id: draft.umu.as_ref().map(|umu| umu.id.clone()),
        umu_store: draft.umu.and_then(|umu| umu.store),
        prefix_of: draft.prefix_of,
        art: draft.art,
    };
    library
        .write_manifest(&game)
        .inspect(|()| tracing::info!(profile_id, id, name = %game.name, "external game saved"))
        .inspect_err(|err| tracing::error!(profile_id, id, %err, "failed to save external game"))
        .map_err(|err| format!("guardando el juego: {err}"))
}

/// Forgets an external game (manifest, favorites, playtime, collections)
/// and deletes its prefix, which is also where anything its installer put
/// under `C:\` lives — along with every game a launcher in that prefix
/// installed. A game that only shares a launcher's prefix leaves it be.
/// Files it was pointed at outside the prefix are left alone: decanter
/// never put them there.
pub fn remove(profile_id: &str, id: &str) -> Result<(), String> {
    let library = crate::achv_try!(crate::core::library_for_profile(profile_id));
    let removed = library.read_manifest(profile_id, Storefront::Unspecified, id);
    // Games a launcher installed live in its prefix, which is about to go:
    // they go with it rather than pointing at nothing.
    for hosted in list(profile_id)
        .into_iter()
        .filter(|game| game.prefix_of.as_deref() == Some(id))
    {
        crate::achv_try!(remove(profile_id, &hosted.game_id));
    }
    crate::achv_try!(
        library
            .remove_manifest(profile_id, Storefront::Unspecified, id)
            .inspect_err(|err| tracing::error!(profile_id, id, %err, "failed to remove external game manifest"))
            .map_err(|err| format!("borrando el juego: {err}"))
    );
    // A hosted game's prefix is its launcher's, still in use.
    let owns_prefix = removed.is_none_or(|game| game.prefix_of.is_none());
    if owns_prefix && let Some(compatdata) = library.compatdata(profile_id, id) {
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
    // Same best-effort reasoning: rows for an id nothing will ever show
    // again are only clutter.
    if let Err(err) = crate::core::db().and_then(|db| {
        db.forget_product(profile_id, Storefront::Unspecified, id)
            .map_err(|err| err.to_string())
    }) {
        tracing::warn!(profile_id, id, %err, "could not forget external game's favorites, playtime and collections");
    }
    tracing::info!(profile_id, id, "external game removed");
    Ok(())
}
/// Runs a setup program inside `id`'s prefix and waits for it, and for
/// anything it left running there, to finish. The prefix is the one the
/// game will then launch in, so whatever the installer writes under
/// `C:\` — files, registry keys, runtimes — is there for the game.
pub async fn run_installer(
    profile_id: &str,
    id: &str,
    installer: &Path,
    proton: Option<String>,
    library_root: Option<&Path>,
) -> Result<(), String> {
    let prefix =
        crate::achv_try!(prefix_in(profile_id, id, library_root).ok_or("no se pudo resolver el prefijo del juego"));
    // The same id the game itself launches with, so protonfixes treats
    // both alike.
    let launch = decanter_runtime::Launch::new(installer, UMU_ID).proton(crate::core::proton_or_default(proton));
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
pub fn drive_c(profile_id: &str, id: &str, library_root: Option<&Path>) -> Option<PathBuf> {
    prefix_in(profile_id, id, library_root).map(|prefix| prefix.drive_c())
}

/// `profile_id`'s library with new games aimed at `root`, if one was
/// picked.
pub fn library_in(profile_id: &str, root: Option<&Path>) -> Result<decanter_core::Library, String> {
    crate::core::library_for_profile(profile_id).map(|library| match root {
        Some(root) => library.targeting(root),
        None => library,
    })
}

/// The prefix `id` has, or gets, in the folder it's being added to — before
/// its manifest exists to say where it lives.
fn prefix_in(profile_id: &str, id: &str, root: Option<&Path>) -> Option<decanter_runtime::Prefix> {
    library_in(profile_id, root)
        .ok()?
        .compatdata(profile_id, id)
        .map(decanter_runtime::Prefix::new)
}
