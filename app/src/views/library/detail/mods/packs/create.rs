//! A pack made from the game's installed mods (design, section 4): every mod becomes an entry with
//! its origin and pinned version, in this account's load order with its on or off state, its
//! target and its DLL modes; the mods only other accounts have follow, off. A mod with no origin is
//! a `local` entry, recognised by its archive's hash, or, imported from a folder, by this
//! machine's notes alone, which no one else can reproduce.

use std::collections::HashSet;

use decanter_modpack::{Entry, EntryKey, LocalNotes, Pack, Source};
use decanter_mods::{GameMods, ModManifest, ModOrigin};

/// Leaves room for a `-NNN` suffix within the format's 32 characters.
const MAX_KEY_STEM: usize = 27;
const MAX_TEXT_CHARS: usize = 200;

/// The pack and the notes that link each entry to the mod it was made from, so a folder entry is
/// recognised and the pack counts every mod as adopted, never as one it may delete.
pub(super) fn from_installed(
    mods: &GameMods,
    account: &str,
    name: String,
    title: &str,
) -> decanter_mods::Result<(Pack, LocalNotes)> {
    let all = mods.list()?;
    let slots = mods.load_order(account)?;
    let in_order = slots
        .iter()
        .filter_map(|slot| {
            all.iter()
                .find(|manifest| manifest.id == slot.id)
                .map(|manifest| (manifest, slot.enabled))
        })
        .chain(
            all.iter()
                .filter(|manifest| !slots.iter().any(|slot| slot.id == manifest.id))
                .map(|manifest| (manifest, false)),
        );
    let mut pack = Pack::new(name);
    pack.game.title = Some(clip(title)).filter(|title| !title.is_empty());
    pack.game.thunderstore = decanter_thunderstore::GameSettings::read(mods)
        .ok()
        .flatten()
        .map(|settings| settings.community.to_string());
    pack.game.nexus = decanter_nexus::GameSettings::read(mods)
        .ok()
        .flatten()
        .map(|settings| settings.domain.to_string());
    pack.game.modio = decanter_modio::GameSettings::read(mods)
        .ok()
        .flatten()
        .map(|settings| settings.game.get());
    let mut notes = LocalNotes::new(pack.id.clone());
    let mut keys = HashSet::new();
    for (index, (manifest, enabled)) in in_order.enumerate() {
        let order = u32::try_from(index)
            .unwrap_or(u32::MAX)
            .saturating_add(1)
            .saturating_mul(10);
        let Some(key) = unique_key(&manifest.name, &mut keys) else {
            tracing::warn!(mod_id = %manifest.id, "no pack entry key could be made for the mod, left out");
            continue;
        };
        notes.record(key.clone(), manifest.id.clone(), false);
        pack.entries.push(entry(manifest, key, order, enabled));
    }
    tracing::info!(pack = %pack.id, entries = pack.entries.len(), "mod pack made from the installed mods");
    Ok((pack, notes))
}

fn entry(manifest: &ModManifest, key: EntryKey, order: u32, enabled: bool) -> Entry {
    let mut entry = Entry::new(key, order, source(manifest));
    entry.enabled = enabled;
    entry.target.clone_from(&manifest.target);
    entry.name = Some(clip(&manifest.name)).filter(|name| !name.is_empty());
    entry.author = manifest
        .origin
        .as_ref()
        .and_then(|origin| origin.credits.as_ref())
        .and_then(|credits| credits.author.as_deref())
        .map(clip)
        .filter(|author| !author.is_empty());
    entry.sha256 = manifest.source_sha256.as_deref().and_then(|hash| hash.parse().ok());
    entry.size = entry
        .sha256
        .as_ref()
        .map(|_| manifest.source_size)
        .filter(|&size| (1..=decanter_mods::MAX_MOD_BYTES).contains(&size));
    entry.dll_overrides.clone_from(&manifest.dll_overrides);
    entry
}

/// The platform's ids from the mod's origin; a mod from no platform, or from one whose origin no
/// longer reads back, is the user's own file.
fn source(manifest: &ModManifest) -> Source {
    manifest
        .origin
        .as_ref()
        .and_then(provider_source)
        .unwrap_or_else(|| Source::Local {
            file_name: file_name(manifest),
        })
}

fn provider_source(origin: &ModOrigin) -> Option<Source> {
    match origin.platform.as_str() {
        decanter_thunderstore::PLATFORM => Some(Source::Thunderstore {
            package: origin.package.clone(),
            version: origin.version.clone(),
        }),
        decanter_nexus::PLATFORM => {
            let nexus = decanter_nexus::NexusOrigin::read(origin).ok()??;
            Some(Source::Nexus {
                domain: nexus.domain.to_string(),
                mod_id: nexus.mod_id.to_string().parse().ok()?,
                file: nexus.file_id.to_string().parse().ok()?,
                version: Some(nexus.version).filter(|version| !version.is_empty()),
            })
        }
        decanter_modio::PLATFORM => {
            let modio = decanter_modio::Origin::from_mod_origin(origin)?;
            Some(Source::Modio {
                game: modio.game.get(),
                mod_id: modio.mod_id.get(),
                file: modio.file.get(),
                version: decanter_modio::Origin::version_of(origin).map(str::to_owned),
            })
        }
        other => {
            tracing::warn!(
                platform = other,
                "mod origin of an unknown platform, kept as a local entry"
            );
            None
        }
    }
}

/// What the user is asked for when the pack wants the file again: the archive's or folder's own
/// name, which the format takes as one file name.
fn file_name(manifest: &ModManifest) -> String {
    let named = clip(&manifest.source_file_name.replace(['/', '\\'], "_"));
    if named.is_empty() { clip(&manifest.name) } else { named }
}

fn clip(text: &str) -> String {
    text.trim()
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .take(MAX_TEXT_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `[a-z0-9_-]` from the mod's name, made unique within the pack with a number.
fn unique_key(name: &str, taken: &mut HashSet<String>) -> Option<EntryKey> {
    let mut stem = String::with_capacity(MAX_KEY_STEM);
    for character in name.chars().flat_map(char::to_lowercase) {
        if stem.len() >= MAX_KEY_STEM {
            break;
        }
        if character.is_ascii_alphanumeric() {
            stem.push(character);
        } else if !stem.is_empty() && !stem.ends_with('-') {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-');
    let stem = if stem.is_empty() { "mod" } else { stem };
    let mut candidate = stem.to_owned();
    let mut number = 1_u32;
    while taken.contains(&candidate) {
        number = number.saturating_add(1);
        candidate = format!("{stem}-{number}");
    }
    taken.insert(candidate.clone());
    candidate.parse().ok()
}
