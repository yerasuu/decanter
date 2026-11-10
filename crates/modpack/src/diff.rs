// SPDX-License-Identifier: GPL-3.0-or-later
//! A pack against the mods a game has, recomputed on every apply so the apply keeps no progress of
//! its own (design, section 5.3): what is there, what is there at another version than the pin, and
//! what is missing, matched by the platform ids of each mod's origin or by its archive's hash.

use decanter_mods::{ModId, ModManifest, ModOrigin};

use crate::{Entry, EntryKey, LocalNotes, Pack, Source, Track};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
    Missing,
    /// The mod the entry pins, or any version of it for [`Track::Latest`].
    Present {
        id: ModId,
        adopted: bool,
    },
    /// The entry's mod at another version or file than the pin, or with another archive hash.
    Outdated {
        id: ModId,
        adopted: bool,
    },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EntryDiff {
    pub key: EntryKey,
    pub status: Status,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Diff {
    /// In the pack's file order.
    pub entries: Vec<EntryDiff>,
    /// The game's mods no entry matched, which applying the pack never moves or removes.
    pub extra: Vec<ModId>,
}

impl Diff {
    #[must_use]
    #[inline]
    pub fn status(&self, key: &EntryKey) -> Option<&Status> {
        self.entries
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| &entry.status)
    }
}

/// `installed` is the game's mods, `GameMods::list`.
///
/// A matched mod is adopted unless `notes` records that this pack installed it, so a lost or
/// foreign notes file never makes a mod the pack's to delete; notes of another pack are ignored.
#[must_use]
#[inline]
pub fn diff(pack: &Pack, installed: &[ModManifest], notes: Option<&LocalNotes>) -> Diff {
    let notes = notes.filter(|notes| *notes.pack() == pack.id);
    let mut claimed = vec![false; installed.len()];
    let mut found: Vec<Option<(usize, bool)>> = vec![None; pack.entries.len()];

    // The notes first, since they are the only link a local folder has; then exact matches, so a
    // second copy of a mod at another version is not taken for an outdated pinned one.
    for (slot, entry) in found.iter_mut().zip(&pack.entries) {
        let Some(note) = notes.and_then(|notes| notes.get(&entry.key)) else {
            continue;
        };
        let Some(index) = installed.iter().position(|manifest| manifest.id == note.id) else {
            continue;
        };
        let fit = match (&entry.source, &entry.sha256) {
            (Source::Local { .. }, None) => Some(true),
            _ => compare(entry, installed.get(index)),
        };
        if let (Some(exact), Some(taken)) = (fit, claimed.get_mut(index))
            && !*taken
        {
            *taken = true;
            *slot = Some((index, exact));
        }
    }
    for wanted in [true, false] {
        for (slot, entry) in found.iter_mut().zip(&pack.entries) {
            if slot.is_some() {
                continue;
            }
            let hit = installed
                .iter()
                .zip(claimed.iter_mut())
                .enumerate()
                .find(|(_, (manifest, taken))| !**taken && compare(entry, Some(manifest)) == Some(wanted));
            if let Some((index, (_, taken))) = hit {
                *taken = true;
                *slot = Some((index, wanted));
            }
        }
    }

    let entries = pack
        .entries
        .iter()
        .zip(found)
        .map(|(entry, slot)| {
            let status = slot
                .and_then(|(index, exact)| installed.get(index).map(|manifest| (manifest, exact)))
                .map_or(Status::Missing, |(manifest, exact)| {
                    let id = manifest.id.clone();
                    let adopted = !notes
                        .and_then(|notes| notes.get(&entry.key))
                        .is_some_and(|note| note.installed && note.id == id);
                    if exact {
                        Status::Present { id, adopted }
                    } else {
                        Status::Outdated { id, adopted }
                    }
                });
            EntryDiff {
                key: entry.key.clone(),
                status,
            }
        })
        .collect();
    let extra = installed
        .iter()
        .zip(&claimed)
        .filter(|&(_, taken)| !taken)
        .map(|(manifest, _)| manifest.id.clone())
        .collect();
    Diff { entries, extra }
}

/// `None` when `manifest` is not the entry's mod; `Some(true)` when it is the one the entry pins.
fn compare(entry: &Entry, manifest: Option<&ModManifest>) -> Option<bool> {
    let manifest = manifest?;
    let same_hash = match (&entry.sha256, &manifest.source_sha256) {
        (Some(wanted), Some(have)) => wanted.as_str() == have,
        _ => true,
    };
    let same_version = match &entry.source {
        Source::Local { .. } => {
            let wanted = entry.sha256.as_ref()?;
            return (manifest.source_sha256.as_deref() == Some(wanted.as_str())).then_some(true);
        }
        Source::Thunderstore { package, version } => {
            let origin = origin(manifest, entry)?;
            if origin.package != *package {
                return None;
            }
            origin.version == *version
        }
        Source::Nexus {
            domain, mod_id, file, ..
        } => {
            // decanter-nexus packs `<domain>-<mod>-<file>`; a domain may hold `-`, the numbers not.
            let origin = origin(manifest, entry)?;
            let mut parts = origin.package.rsplitn(3, '-');
            let have_file = parts.next()?.parse::<u64>().ok()?;
            let have_mod = parts.next()?.parse::<u64>().ok()?;
            if parts.next()? != domain || have_mod != *mod_id {
                return None;
            }
            have_file == *file
        }
        Source::Modio { game, mod_id, file, .. } => {
            // decanter-modio packs `<game>-<mod>` and keeps the file as the version's first token.
            let origin = origin(manifest, entry)?;
            if origin.package != format!("{game}-{mod_id}") {
                return None;
            }
            let have_file = origin
                .version
                .split_once('-')
                .map_or(origin.version.as_str(), |(have, _)| have);
            have_file.parse::<u64>().ok()? == *file
        }
    };
    // A latest entry's hash is of the version last seen, so another version rightly differs.
    Some(entry.track == Track::Latest || (same_version && same_hash))
}

fn origin<'manifest>(manifest: &'manifest ModManifest, entry: &Entry) -> Option<&'manifest ModOrigin> {
    manifest
        .origin
        .as_ref()
        .filter(|origin| origin.platform == entry.source.platform())
}
