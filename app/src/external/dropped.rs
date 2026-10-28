//! Games added by dropping their executable on the window (decision
//! #1065): registered straight away with what [`super::probe`] reads from
//! the file, through the same [`super::save`] as the form, and undone as a
//! whole from the notice that follows.

use std::path::{Path, PathBuf};

use decanter_core::{GameArt, GameSystem, platform};

use super::probe::{self, Found, Image, Kind, Refusal};
use crate::i18n::Text;

/// What one drop did, file by file.
#[derive(Default)]
pub struct Outcome {
    pub added: Vec<Registered>,
    /// Dropped again: already in the library under this id.
    pub existing: Vec<Registered>,
    pub refused: Vec<Refused>,
}

pub struct Registered {
    pub id: String,
    pub name: String,
}

pub struct Refused {
    /// The file's name, as dropped.
    pub file: String,
    pub reason: Text,
}

/// Registers every program among `paths` as an external game of
/// `profile_id`, or finds the one it already is. Blocking: reads each file
/// and writes manifests and images.
pub fn register(profile_id: &str, paths: &[PathBuf]) -> Outcome {
    // By canonical executable, with the game's id and name.
    let mut known: Vec<(PathBuf, String, String)> = super::list(profile_id)
        .into_iter()
        .filter_map(|game| {
            let executable = crate::core::executable_for(&game)?;
            let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
            Some((executable, game.game_id, game.name))
        })
        .collect();
    let mut outcome = Outcome::default();
    for path in paths {
        let file = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let found = match runnable(path) {
            Ok(found) => found,
            Err(reason) => {
                outcome.refused.push(Refused { file, reason });
                continue;
            }
        };
        if let Some((_, id, name)) = known.iter().find(|(executable, ..)| *executable == found.executable) {
            tracing::info!(path = %path.display(), game_id = %id, "dropped program is already a game");
            outcome.existing.push(Registered {
                id: id.clone(),
                name: name.clone(),
            });
            continue;
        }
        let executable = found.executable.clone();
        match add(profile_id, found) {
            Ok(game) => {
                known.push((executable, game.id.clone(), game.name.clone()));
                outcome.added.push(game);
            }
            Err(reason) => outcome.refused.push(Refused { file, reason }),
        }
    }
    tracing::info!(
        profile_id,
        added = outcome.added.len(),
        existing = outcome.existing.len(),
        refused = outcome.refused.len(),
        "dropped files handled"
    );
    outcome
}

/// Removes the games one drop added, with their prefixes and images.
/// Blocking.
pub fn undo(profile_id: &str, ids: &[String]) {
    for id in ids {
        if let Err(err) = super::remove(profile_id, id) {
            tracing::error!(profile_id, id, %err, "could not undo a dropped game");
        }
    }
    tracing::info!(profile_id, count = ids.len(), "dropped games undone");
}

/// What [`probe::probe`] finds at `path`, if it is a program this build
/// can run.
fn runnable(path: &Path) -> Result<Found, Text> {
    match probe::probe(path) {
        Ok(found) if runs_here(found.kind) => Ok(found),
        Ok(_) => {
            tracing::info!(path = %path.display(), "dropped program is for another system");
            Err(Text::new("external.drop.refused.other_system"))
        }
        Err(refusal) => {
            tracing::info!(path = %path.display(), %refusal, "dropped file refused");
            Err(localized(&refusal))
        }
    }
}

/// Whether a program built for `kind` runs from this build: Windows ones
/// under Proton on Linux or as themselves on Windows, Linux ones only on
/// Linux.
fn runs_here(kind: Kind) -> bool {
    match kind {
        Kind::Windows => platform::game_system() == GameSystem::Windows,
        Kind::Linux => crate::launch::in_prefixes(),
    }
}

fn add(profile_id: &str, found: Found) -> Result<Registered, Text> {
    let library = crate::achv_try!(super::library_in(profile_id, None));
    let id = uuid::Uuid::new_v4().to_string();
    let art = store_art(library.root(), &id, found.art);
    let draft = super::Draft {
        name: found.title,
        executable: found.executable,
        art,
        umu: None,
        prefix_of: None,
        library_root: None,
    };
    let name = draft.name.clone();
    if let Err(err) = super::save(profile_id, &id, draft) {
        remove_art(library.root(), &id);
        return Err(err);
    }
    tracing::info!(profile_id, id, name, "dropped program added as a game");
    Ok(Registered { id, name })
}

/// Where the images read from a dropped program are kept: inside the
/// library, in a folder of the game's own so removing the game removes
/// them.
pub(super) fn art_dir(root: &Path, id: &str) -> Option<PathBuf> {
    // A uuid is all a game id this folder is made for can be, and never a
    // path that reaches outside it.
    uuid::Uuid::parse_str(id).ok()?;
    Some(
        decanter_core::bootstrap::library_data_dir(root)
            .join("external")
            .join(id),
    )
}

/// Writes each image found, as the form's own file picks are kept: a
/// `file://` URL. One that can't be written is left out.
fn store_art(root: &Path, id: &str, art: probe::Art) -> GameArt {
    let Some(dir) = art_dir(root, id) else {
        return GameArt::default();
    };
    if [&art.icon, &art.cover, &art.hero].iter().all(|image| image.is_none()) {
        return GameArt::default();
    }
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), %err, "could not create the folder for a dropped game's images");
        return GameArt::default();
    }
    let write = |slot: &str, image: Option<Image>| {
        let image = image?;
        let path = dir.join(format!("{slot}.{}", image.extension));
        std::fs::write(&path, &image.bytes)
            .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "could not keep a dropped game's image"))
            .ok()?;
        Some(format!("file://{}", path.display()))
    };
    GameArt {
        icon: write("icon", art.icon),
        cover: write("cover", art.cover),
        hero: write("hero", art.hero),
    }
}

pub(super) fn remove_art(root: &Path, id: &str) {
    let Some(dir) = art_dir(root, id) else {
        return;
    };
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(dir = %dir.display(), %err, "could not remove an external game's images"),
    }
}

fn localized(refusal: &Refusal) -> Text {
    match refusal {
        Refusal::Missing => Text::new("external.drop.refused.missing"),
        Refusal::Directory => Text::new("external.drop.refused.directory"),
        Refusal::Special => Text::new("external.drop.refused.special"),
        Refusal::Unreadable(err) => Text::new("external.drop.refused.unreadable").arg("error", err),
        Refusal::Library => Text::new("external.drop.refused.library"),
        Refusal::NotExecutable => Text::new("external.drop.refused.not_executable"),
        Refusal::Unsupported if crate::launch::in_prefixes() => Text::new("external.drop.refused.unsupported"),
        Refusal::Unsupported => Text::new("external.drop.refused.unsupported_windows"),
    }
}
