//! The Windows programs added to the Raven section (decision #1069): filed like external games,
//! one manifest per program read and written by the same [`Library`] code, but in Raven's own
//! folder (`<raven>/manifests/<profile>/`) so they never show as games of the library, each with a
//! Raven prefix of its own at `<raven>/prefixes/<id>`. The program's file is never copied, changed
//! or moved.

use std::path::{Path, PathBuf};

use decanter_core::{GameArt, InstalledGame, Library, Storefront};

use crate::external::probe::{self, Kind};
use crate::i18n::Text;

/// Raven's folder of the active library, read as a library of its own.
fn store() -> Option<Library> {
    decanter_runtime::runner::raven_dir().map(Library::new)
}

/// Every program of `profile_id`, by name.
pub fn list(profile_id: &str) -> Vec<InstalledGame> {
    let Some(store) = store() else {
        return Vec::new();
    };
    let mut programs: Vec<InstalledGame> = store.scan(profile_id).into_values().collect();
    programs.sort_by_cached_key(|program| program.name.to_lowercase());
    programs
}

/// Whether `game_id` of `account_id` is one of the section's programs rather than a game of the
/// library: where its manifest is filed is what decides how it starts.
pub fn is_program(account_id: &str, game_id: &str) -> bool {
    store()
        .and_then(|store| store.manifest_path(account_id, Storefront::Unspecified, game_id))
        .is_some_and(|path| path.is_file())
}

/// The program's Raven prefix; `None` for an id that is no uuid, which then never names a folder.
pub fn prefix(game_id: &str) -> Option<PathBuf> {
    uuid::Uuid::parse_str(game_id).ok()?;
    decanter_runtime::runner::raven_dir().map(|dir| dir.join("prefixes").join(game_id))
}

/// Adds the Windows program at `path` to `profile_id`'s section, or finds the one it already is.
/// Blocking: reads the file and writes the manifest.
///
/// # Errors
/// When the file is no Windows program or the manifest can't be written; the text is ready to show.
pub fn add(profile_id: &str, path: &Path) -> Result<InstalledGame, Text> {
    let found = match probe::probe(path) {
        Ok(found) if found.kind == Kind::Windows => found,
        Ok(_) => return Err(Text::new("raven.errors.not_windows")),
        Err(refusal) => {
            tracing::info!(path = %path.display(), %refusal, "file refused for Raven");
            return Err(crate::external::dropped::localized(&refusal));
        }
    };
    let executable = found.executable.to_string_lossy().into_owned();
    if let Some(existing) = list(profile_id)
        .into_iter()
        .find(|program| program.launch_executable.as_deref() == Some(executable.as_str()))
    {
        tracing::info!(profile_id, id = %existing.game_id, "program is already in the Raven section");
        return Ok(existing);
    }
    let Some(store) = store() else {
        return Err(Text::new("raven.errors.no_library"));
    };
    let id = uuid::Uuid::new_v4().to_string();
    let program = InstalledGame {
        account_id: profile_id.to_owned(),
        storefront: Storefront::Unspecified,
        game_id: id.clone(),
        name: found.title,
        // The manifest format needs a single path component here; nothing is installed.
        install_dir: id.clone(),
        build_id: String::new(),
        size_on_disk: None,
        size_fingerprint: None,
        launch_executable: Some(executable),
        last_played: None,
        proton: None,
        branch: None,
        dlcs: Default::default(),
        cloud_save_path: None,
        cloud_save_last_sync: None,
        umu_id: None,
        umu_store: None,
        prefix_of: None,
        art: GameArt::default(),
    };
    store.write_manifest(&program).map_err(|err| {
        tracing::error!(profile_id, id, %err, "could not add a program to the Raven section");
        Text::new("raven.errors.save").arg("error", err)
    })?;
    tracing::info!(profile_id, id, name = %program.name, "program added to the Raven section");
    Ok(program)
}

/// Forgets the program and deletes its Raven prefix. Blocking.
///
/// # Errors
/// When the manifest or the prefix can't be removed; the text is ready to show.
pub fn remove(profile_id: &str, id: &str) -> Result<(), Text> {
    let Some(store) = store() else {
        return Err(Text::new("raven.errors.no_library"));
    };
    store
        .remove_manifest(profile_id, Storefront::Unspecified, id)
        .map_err(|err| Text::new("raven.errors.remove").arg("error", err))?;
    if let Some(prefix) = prefix(id) {
        match std::fs::remove_dir_all(&prefix) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::error!(profile_id, id, %err, "could not remove a Raven prefix");
                return Err(Text::new("raven.errors.remove").arg("error", err));
            }
        }
    }
    tracing::info!(profile_id, id, "program removed from the Raven section");
    Ok(())
}

/// Stamps the program's manifest with when its session ended. Best-effort, as for a game.
pub fn record_last_played(program: &InstalledGame, now: i64) {
    let Some(store) = store() else {
        return;
    };
    let mut current = store
        .read_manifest(&program.account_id, Storefront::Unspecified, &program.game_id)
        .unwrap_or_else(|| program.clone());
    current.last_played = Some(now);
    if let Err(err) = store.write_manifest(&current) {
        tracing::warn!(id = %program.game_id, %err, "could not record when a Raven program last ran");
    }
}
