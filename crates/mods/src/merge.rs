//! What the play tree should hold: every file of the install, then each enabled mod's in load
//! order, merged the way a Windows game sees paths (case-insensitively, keeping the install's
//! spelling of anything it already has).

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::game::GameMods;
use crate::manifest::{FILES, ModManifest};
use crate::path::ModPath;
use crate::{Error, Result};

/// From a link's folder in `play/<account>/` up to the game's mods folder, past the link's own
/// parents: `play` and `<account>`.
const UP_TO_GAME_DIR: usize = 2;
/// From the game's mods folder up to the library root: `<game>`, `<store>`, `mods`, `decanter`.
const UP_TO_ROOT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Game,
    /// Index into the enabled mods, in load order.
    Mod(usize),
}

#[derive(Debug)]
pub(crate) struct Wanted {
    /// As the play tree spells it.
    pub(crate) path: ModPath,
    /// The winning file on disk.
    pub(crate) source: PathBuf,
    /// The winning file relative to the link's folder, which is what a link holds so that moving
    /// the library keeps every link valid.
    pub(crate) target: String,
    /// Everything that has this path, the winner last.
    pub(crate) providers: Vec<Owner>,
}

/// Keyed by the folded path.
pub(crate) fn merge(game: &GameMods, mods: &[ModManifest]) -> Result<BTreeMap<String, Wanted>> {
    let mut files = BTreeMap::new();
    let mut dirs = HashMap::new();
    for path in walk_install(&game.install)? {
        record_dirs(&path, &mut dirs);
        let target = relative_target(
            &path,
            UP_TO_GAME_DIR + UP_TO_ROOT,
            &format!("common/{}/{path}", game.install_name),
        );
        let source = path.under(&game.install);
        match files.entry(path.folded()) {
            // Wine opens one of two names differing only in case, whichever it finds first; the play
            // tree keeps the first one walked.
            Entry::Occupied(_) => {
                tracing::warn!(path = %path, "install file left out of the play tree, another differs only in case");
            }
            Entry::Vacant(slot) => {
                slot.insert(Wanted {
                    source,
                    target,
                    providers: vec![Owner::Game],
                    path,
                });
            }
        }
    }
    for (index, manifest) in mods.iter().enumerate() {
        let mod_dir = game.mod_dir(&manifest.id);
        for file in &manifest.files {
            let in_game = manifest.game_path(file)?;
            let Some(path) = respell(&in_game, &files, &mut dirs) else {
                tracing::warn!(mod_id = %manifest.id, path = %in_game, "mod file left out, a file of that name is a folder");
                continue;
            };
            let key = path.folded();
            if dirs.contains_key(&key) {
                tracing::warn!(mod_id = %manifest.id, path = %in_game, "mod file left out, a folder has its name");
                continue;
            }
            let source = file.path.under(&mod_dir.join(FILES));
            let target = relative_target(&path, UP_TO_GAME_DIR, &format!("{}/{FILES}/{}", manifest.id, file.path));
            files
                .entry(key)
                .and_modify(|wanted: &mut Wanted| {
                    wanted.source.clone_from(&source);
                    wanted.target.clone_from(&target);
                    wanted.providers.push(Owner::Mod(index));
                })
                .or_insert_with(|| Wanted {
                    path,
                    source,
                    target,
                    providers: vec![Owner::Mod(index)],
                });
        }
    }
    Ok(files)
}

/// `path` with every folder the play tree already has spelled the way it has it, and its folders
/// recorded for the mods after it; `None` when one of its folders is a file.
fn respell(path: &ModPath, files: &BTreeMap<String, Wanted>, dirs: &mut HashMap<String, String>) -> Option<ModPath> {
    let mut folded = String::new();
    let mut spelled = String::new();
    let mut components = path.components().peekable();
    while let Some(name) = components.next() {
        push_component(&mut folded, &name.to_lowercase());
        if components.peek().is_none() {
            if let Some(existing) = files.get(&folded) {
                return Some(existing.path.clone());
            }
            push_component(&mut spelled, name);
            break;
        }
        if files.contains_key(&folded) {
            return None;
        }
        let candidate = if spelled.is_empty() {
            name.to_owned()
        } else {
            format!("{spelled}/{name}")
        };
        spelled.clone_from(dirs.entry(folded.clone()).or_insert(candidate));
    }
    ModPath::parse(&spelled).ok()
}

/// The install's spelling of each folder of `path`, which mods' paths take.
fn record_dirs(path: &ModPath, dirs: &mut HashMap<String, String>) {
    let mut folded = String::new();
    let mut spelled = String::new();
    let mut components = path.components().peekable();
    while let Some(name) = components.next() {
        if components.peek().is_none() {
            break;
        }
        push_component(&mut folded, &name.to_lowercase());
        push_component(&mut spelled, name);
        dirs.entry(folded.clone()).or_insert_with(|| spelled.clone());
    }
}

fn push_component(path: &mut String, name: &str) {
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(name);
}

/// `rest` as seen from the folder of the link at `path` in the play tree, climbing `up` folders
/// past the link's own parents.
fn relative_target(path: &ModPath, up: usize, rest: &str) -> String {
    let climbs = path.depth().saturating_sub(1).saturating_add(up);
    let mut target = "../".repeat(climbs);
    target.push_str(rest);
    target
}

/// Every file of the install, without following links into folders: the play tree mirrors what
/// the game would see there, DLC and the Galaxy hook included.
fn walk_install(install: &Path) -> Result<Vec<ModPath>> {
    let mut found = Vec::new();
    let mut pending = vec![(install.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in fs::read_dir(&dir).map_err(Error::io(&dir))? {
            let entry = entry.map_err(Error::io(&dir))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                tracing::warn!(path = %entry.path().display(), "install file left out of the play tree, its name is not UTF-8");
                continue;
            };
            let relative = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let file_type = entry.file_type().map_err(Error::io(&entry.path()))?;
            if file_type.is_dir() {
                pending.push((entry.path(), relative));
                continue;
            }
            // A link to a folder would make that folder writable through the play tree.
            if file_type.is_symlink() && !fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file()) {
                tracing::warn!(path = %entry.path().display(), "install link left out of the play tree, it is not a file");
                continue;
            }
            match ModPath::parse(&relative) {
                Ok(path) => found.push(path),
                Err(err) => tracing::warn!(%err, "install file left out of the play tree"),
            }
        }
    }
    Ok(found)
}
