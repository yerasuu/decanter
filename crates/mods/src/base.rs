//! Whether an update changed a game file an enabled mod replaces or adds (design, section 3): the
//! set file keeps each such file's hash as it was when the mods were applied, compared again only
//! when the game's build changes, so a launch hashes nothing until an update. A changed file marks
//! every enabled mod that touches it *needs attention*: left out of the play tree until the user
//! keeps it, which records the new file, or disables it.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::game::GameMods;
use crate::import::hex;
use crate::manifest::{FILES, ModManifest};
use crate::merge::{Owner, merge};
use crate::path::{ModId, ModPath};
use crate::set::{ABSENT, ModSet};
use crate::{Error, Result};

const HASH_BUFFER: usize = 1 << 20;

/// The game paths the mods touch, keyed folded since a mod's spelling of a file the update adds
/// may differ from the install's.
struct Touched {
    /// As the play tree spells it, where the install's file is.
    path: ModPath,
    in_install: bool,
    /// Indexes into the mods, in load order.
    owners: Vec<usize>,
}

/// Compares the files `mods` touch against `set`'s base when `build_id` is not the one it was
/// taken at, and records the files not seen before; `true` when `set` changed.
pub(crate) fn check(game: &GameMods, set: &mut ModSet, build_id: &str, mods: &[ModManifest]) -> Result<bool> {
    let touched = touched(game, mods)?;
    let build_changed = set.base.build_id.as_deref() != Some(build_id);
    let mut changed = false;
    let mut hashed = 0_usize;
    for (key, file) in &touched {
        let known = set.base.files.get(key);
        if known.is_some() && !build_changed {
            continue;
        }
        let current = if file.in_install {
            hashed = hashed.saturating_add(1);
            hash_file(&file.path.under(&game.install))?
        } else {
            ABSENT.to_owned()
        };
        match known {
            None => {
                set.base.files.insert(key.clone(), current);
                changed = true;
            }
            Some(base) if *base != current => {
                for manifest in file.owners.iter().filter_map(|&index| mods.get(index)) {
                    let paths = set.attention.entry(manifest.id.clone()).or_default();
                    if !paths.contains(&file.path) {
                        paths.push(file.path.clone());
                        changed = true;
                    }
                }
            }
            Some(_) => {}
        }
    }
    if build_changed {
        set.base.build_id = Some(build_id.to_owned());
        changed = true;
    }
    let before = set.base.files.len();
    set.base.files.retain(|key, _| touched.contains_key(key));
    changed |= set.base.files.len() != before;
    if changed {
        tracing::info!(
            build_id,
            build_changed,
            files = touched.len(),
            hashed,
            needing_attention = set.attention.len(),
            "mods checked against the game's files"
        );
    }
    Ok(changed)
}

/// Records the game's files as they are now for the paths that put `id` in need of attention, and
/// lets it back into the play tree.
pub(crate) fn keep(game: &GameMods, set: &mut ModSet, id: &ModId) -> Result<()> {
    let Some(paths) = set.attention.remove(id) else {
        return Ok(());
    };
    for path in &paths {
        let on_disk = path.under(&game.install);
        let current = if on_disk.is_file() {
            hash_file(&on_disk)?
        } else {
            ABSENT.to_owned()
        };
        set.base.files.insert(folded(path)?, current);
    }
    tracing::info!(mod_id = %id, files = paths.len(), "mod kept after the game's files changed");
    Ok(())
}

fn touched(game: &GameMods, mods: &[ModManifest]) -> Result<BTreeMap<ModPath, Touched>> {
    let mut touched = BTreeMap::new();
    for wanted in merge(game, mods)?.into_values() {
        let owners: Vec<usize> = wanted
            .providers
            .iter()
            .filter_map(|provider| match *provider {
                Owner::Mod(index) => Some(index),
                Owner::Game => None,
            })
            .collect();
        if owners.is_empty() {
            continue;
        }
        touched.insert(
            folded(&wanted.path)?,
            Touched {
                in_install: wanted.providers.contains(&Owner::Game),
                path: wanted.path,
                owners,
            },
        );
    }
    Ok(touched)
}

fn folded(path: &ModPath) -> Result<ModPath> {
    ModPath::parse(&path.folded())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(Error::io(path))?;
    let mut buffer = vec![0_u8; HASH_BUFFER];
    let mut hasher = Sha256::new();
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hasher.update(buffer.get(..count).unwrap_or_default()),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(Error::io(path)(err)),
        }
    }
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

/// The mod's files whose content is no longer what was imported, such as a config the game
/// rewrote in place through its link; missing ones count too.
pub(crate) fn changed_files(game: &GameMods, manifest: &ModManifest) -> Result<Vec<ModPath>> {
    let files_dir = game.mod_dir(&manifest.id).join(FILES);
    let mut changed = Vec::new();
    for file in &manifest.files {
        let on_disk = file.path.under(&files_dir);
        let matches = match on_disk.symlink_metadata() {
            Ok(metadata) if metadata.is_file() && metadata.len() == file.size => {
                hash_file(&on_disk)?.strip_prefix("sha256:") == Some(file.sha256.as_str())
            }
            Ok(_) => false,
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => return Err(Error::io(&on_disk)(err)),
        };
        if !matches {
            changed.push(file.path.clone());
        }
    }
    tracing::info!(mod_id = %manifest.id, files = manifest.files.len(), changed = changed.len(), "mod files checked");
    Ok(changed)
}
