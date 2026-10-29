//! The play tree: real folders mirroring the install, one link per file to the install's file or
//! the winning mod's. `play/<account>.toml` records every link decanter placed, so reconciling
//! replaces only those and never a file the game wrote (design, section 2).
//!
//! Links are relative symbolic links; on a Windows host, where a symbolic link needs a privilege,
//! hard links on the same volume instead, told apart from the game's files by the size and time
//! they had when placed.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::game::GameMods;
use crate::manifest::{FILES, ModManifest};
use crate::merge::{Owner, Wanted, merge};
use crate::path::{ModId, ModPath};
use crate::{Error, Result, files};

const SCHEMA: u32 = 1;
const MAX_TARGET_BYTES: usize = 4096;

/// A play tree ready to start the game from.
#[derive(Debug)]
#[non_exhaustive]
pub struct Prepared {
    pub root: PathBuf,
    /// The enabled mods, in load order.
    pub mods: Vec<(ModId, String)>,
    pub links: usize,
    /// Files the game wrote into the play tree, which stay as they are.
    pub written_by_game: usize,
}

#[derive(Serialize, Deserialize)]
struct RawRecord {
    schema: u32,
    #[serde(default)]
    links: BTreeMap<String, Placed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Placed {
    target: String,
    /// Hard links only: what the file was when linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stamp: Option<Stamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    len: u64,
    modified: u64,
}

enum State {
    Missing,
    /// What the record keeps for it.
    Current(Placed),
    /// Placed by decanter, now pointing at the wrong file.
    Stale,
    /// The game's own file, or something else decanter did not put there.
    Foreign,
}

pub(crate) fn reconcile(game: &GameMods, account: &str, mods: &[ModManifest]) -> Result<Prepared> {
    let play = game.play_dir(account)?;
    fs::create_dir_all(&play).map_err(Error::io(&play))?;
    let play_metadata = play.symlink_metadata().map_err(Error::io(&play))?;
    if !play_metadata.is_dir() {
        return Err(Error::Escapes(play));
    }
    let record_path = game.record_path(account)?;
    let previous = read_record(&record_path)?;
    let wanted = merge(game, mods)?;
    let mut placement = Placement::default();
    if let Err(err) = placement.place(game, &play, mods, &previous, &wanted) {
        // Recorded anyway, the previous record under what was placed, so that every link decanter
        // made stays known as its own at the next launch rather than taken for the game's files.
        let mut known = previous;
        known.extend(placement.placed);
        write_record(&record_path, &known)?;
        return Err(err);
    }
    write_record(&record_path, &placement.placed)?;
    let Placement {
        placed,
        written_by_game,
    } = placement;
    tracing::info!(
        account,
        root = %play.display(),
        links = placed.len(),
        mods = mods.len(),
        written_by_game,
        "play tree reconciled"
    );
    Ok(Prepared {
        root: play,
        mods: mods
            .iter()
            .map(|manifest| (manifest.id.clone(), manifest.name.clone()))
            .collect(),
        links: placed.len(),
        written_by_game,
    })
}

#[derive(Default)]
struct Placement {
    placed: BTreeMap<ModPath, Placed>,
    written_by_game: usize,
}

impl Placement {
    fn place(
        &mut self,
        game: &GameMods,
        play: &Path,
        mods: &[ModManifest],
        previous: &BTreeMap<ModPath, Placed>,
        wanted: &BTreeMap<String, Wanted>,
    ) -> Result<()> {
        let by_path: BTreeMap<&ModPath, &Wanted> = wanted.values().map(|entry| (&entry.path, entry)).collect();
        for (path, record) in previous {
            if by_path.contains_key(path) {
                continue;
            }
            let link = path.under(play);
            if is_placed(&link, record)? {
                fs::remove_file(&link).map_err(Error::io(&link))?;
            }
        }
        let mod_roots = mods
            .iter()
            .map(|manifest| {
                let files_dir = game.mod_dir(&manifest.id).join(FILES);
                files_dir.canonicalize().map_err(Error::io(&files_dir))
            })
            .collect::<Result<Vec<_>>>()?;
        for entry in wanted.values() {
            if let Some(&Owner::Mod(index)) = entry.providers.last() {
                let root = mod_roots
                    .get(index)
                    .ok_or_else(|| Error::Escapes(entry.source.clone()))?;
                check_inside(&entry.source, root)?;
            }
            let link = entry.path.under(play);
            let kept = match state(&link, previous.get(&entry.path), entry)? {
                State::Current(record) => Some(record),
                State::Stale => {
                    fs::remove_file(&link).map_err(Error::io(&link))?;
                    make_link(&link, entry)?
                }
                State::Missing => make_link(&link, entry)?,
                State::Foreign => None,
            };
            match kept {
                Some(record) => {
                    self.placed.insert(entry.path.clone(), record);
                }
                None => self.written_by_game = self.written_by_game.saturating_add(1),
            }
        }
        Ok(())
    }
}

/// A mod's file must be a plain file inside its mod's `files/`, since anything along the way (a
/// folder swapped for a link) would lead the play tree outside it.
fn check_inside(source: &Path, root: &Path) -> Result<()> {
    let metadata = source.symlink_metadata().map_err(Error::io(source))?;
    if !metadata.is_file() {
        return Err(Error::NotAFile(source.to_path_buf()));
    }
    let real = source.canonicalize().map_err(Error::io(source))?;
    if !real.starts_with(root) {
        return Err(Error::Escapes(source.to_path_buf()));
    }
    Ok(())
}

/// Creates the link's folders and the link; `None` when a file the game wrote sits where one of
/// the folders goes.
fn make_link(link: &Path, entry: &Wanted) -> Result<Option<Placed>> {
    if let Some(parent) = link.parent() {
        match fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(err) if matches!(err.kind(), io::ErrorKind::NotADirectory | io::ErrorKind::AlreadyExists) => {
                tracing::warn!(path = %link.display(), "no link placed, a file of the game is where its folder goes");
                return Ok(None);
            }
            Err(err) => return Err(Error::io(parent)(err)),
        }
    }
    os::link(link, entry).map(Some)
}

fn state(link: &Path, record: Option<&Placed>, entry: &Wanted) -> Result<State> {
    match link.symlink_metadata() {
        Ok(metadata) => os::state(link, &metadata, record, entry),
        // A file of the game where one of the link's folders goes: make_link then leaves it be.
        Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(State::Missing),
        Err(err) => Err(Error::io(link)(err)),
    }
}

fn is_placed(link: &Path, record: &Placed) -> Result<bool> {
    match link.symlink_metadata() {
        Ok(metadata) => Ok(os::is_placed(link, &metadata, record)),
        Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(false),
        Err(err) => Err(Error::io(link)(err)),
    }
}

#[cfg(unix)]
mod os {
    use std::fs::Metadata;
    use std::path::Path;

    use super::{Placed, State, Wanted};
    use crate::{Error, Result};

    pub(super) fn state(link: &Path, metadata: &Metadata, record: Option<&Placed>, entry: &Wanted) -> Result<State> {
        if !metadata.file_type().is_symlink() {
            return Ok(State::Foreign);
        }
        let current = std::fs::read_link(link).map_err(Error::io(link))?;
        if current.as_os_str() == entry.target.as_str() {
            // Also a link placed by a run cut short before its record was written.
            return Ok(State::Current(Placed {
                target: entry.target.clone(),
                stamp: None,
            }));
        }
        if record.is_some_and(|record| current.as_os_str() == record.target.as_str()) {
            return Ok(State::Stale);
        }
        Ok(State::Foreign)
    }

    /// A link that can't be read is left where it is, as anything not known to be decanter's.
    pub(super) fn is_placed(link: &Path, metadata: &Metadata, record: &Placed) -> bool {
        metadata.file_type().is_symlink()
            && std::fs::read_link(link).is_ok_and(|current| current.as_os_str() == record.target.as_str())
    }

    pub(super) fn link(link: &Path, entry: &Wanted) -> Result<Placed> {
        std::os::unix::fs::symlink(&entry.target, link).map_err(Error::io(link))?;
        Ok(Placed {
            target: entry.target.clone(),
            stamp: None,
        })
    }
}

#[cfg(windows)]
mod os {
    use std::fs::Metadata;
    use std::path::Path;
    use std::time::UNIX_EPOCH;

    use super::{Placed, Stamp, State, Wanted};
    use crate::{Error, Result};

    fn stamp(metadata: &Metadata) -> Stamp {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX));
        Stamp {
            len: metadata.len(),
            modified,
        }
    }

    /// A hard link is the file itself, so what tells decanter's apart from the game's is that it
    /// still has the size and time it had when placed. An update replaces the install's file by
    /// rename, which leaves the link on the old one: it is then stale.
    pub(super) fn state(_link: &Path, metadata: &Metadata, record: Option<&Placed>, entry: &Wanted) -> Result<State> {
        let Some(record) = record else {
            return Ok(State::Foreign);
        };
        if !metadata.is_file() {
            return Ok(State::Foreign);
        }
        let now = stamp(metadata);
        let source = entry.source.metadata().map_err(Error::io(&entry.source))?;
        // Equal to the source's: still the same file, also after the game rewrote it in place.
        if record.target == entry.target && now == stamp(&source) {
            return Ok(State::Current(Placed {
                target: entry.target.clone(),
                stamp: Some(now),
            }));
        }
        if record.stamp == Some(now) {
            return Ok(State::Stale);
        }
        Ok(State::Foreign)
    }

    pub(super) fn is_placed(_link: &Path, metadata: &Metadata, record: &Placed) -> bool {
        metadata.is_file() && record.stamp == Some(stamp(metadata))
    }

    pub(super) fn link(link: &Path, entry: &Wanted) -> Result<Placed> {
        std::fs::hard_link(&entry.source, link).map_err(Error::io(link))?;
        let metadata = link.metadata().map_err(Error::io(link))?;
        Ok(Placed {
            target: entry.target.clone(),
            stamp: Some(stamp(&metadata)),
        })
    }
}

fn read_record(path: &Path) -> Result<BTreeMap<ModPath, Placed>> {
    let Some(raw) = files::read::<RawRecord>(path)? else {
        return Ok(BTreeMap::new());
    };
    if raw.schema != SCHEMA {
        return Err(Error::malformed(path, &"unknown schema"));
    }
    let mut links = BTreeMap::new();
    for (text, placed) in raw.links {
        if placed.target.len() > MAX_TARGET_BYTES {
            return Err(Error::malformed(path, &"a link target is longer than decanter writes"));
        }
        let link = ModPath::parse(&text).map_err(|err| Error::malformed(path, &err))?;
        links.insert(link, placed);
    }
    Ok(links)
}

fn write_record(path: &Path, links: &BTreeMap<ModPath, Placed>) -> Result<()> {
    let raw = RawRecord {
        schema: SCHEMA,
        links: links
            .iter()
            .map(|(link, placed)| (link.as_str().to_owned(), placed.clone()))
            .collect(),
    };
    files::write(path, &raw)
}

pub(crate) fn written_by_game(game: &GameMods, account: &str) -> Result<Vec<ModPath>> {
    let play = game.play_dir(account)?;
    let record = read_record(&game.record_path(account)?)?;
    let mut found = Vec::new();
    let mut pending = vec![(play, String::new())];
    while let Some((dir, prefix)) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(Error::io(&dir)(err)),
        };
        for entry in entries {
            let entry = entry.map_err(Error::io(&dir))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
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
            let Ok(path) = ModPath::parse(&relative) else {
                continue;
            };
            let placed = match record.get(&path) {
                Some(placed) => {
                    let metadata = entry.path().symlink_metadata().map_err(Error::io(&entry.path()))?;
                    os::is_placed(&entry.path(), &metadata, placed)
                }
                None => false,
            };
            if !placed {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}
