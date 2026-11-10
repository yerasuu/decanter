// SPDX-License-Identifier: GPL-3.0-or-later
//! The pack file's TOML shapes and the checks that turn them into a [`Pack`] (design, section
//! 8.2). Every field is optional here so that a missing or misplaced one is reported by name, and
//! only the typed pack is ever serialised, so no field the format lacks can be written.

use std::collections::{HashMap, HashSet};

use decanter_mods::{DllMode, DllOverride, MAX_MOD_BYTES, ModPath};
use serde::{Deserialize, Serialize};

use crate::id::{display_text, token};
use crate::{
    Entry, EntryKey, Error, Field, GameHint, MAX_EDGES, MAX_ENTRIES, Pack, Reason, Result, SCHEMA, Source, Track,
};

/// `requires`, `after` and `dll_overrides` of one entry (design, section 8.2).
const MAX_LIST: usize = 64;
const MAX_PACKAGE_PART: usize = 128;
const MAX_COMMUNITY: usize = 64;
const MAX_DOMAIN: usize = 64;
const MAX_VERSION: usize = 128;
/// Nexus' numbers are counters below twelve digits, the bound decanter-nexus parses them with.
const MAX_NEXUS_ID: u64 = 999_999_999_999;

#[derive(Deserialize)]
struct Probe {
    schema: Option<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPack {
    schema: u32,
    id: Option<String>,
    name: Option<String>,
    revision: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default)]
    game: RawGame,
    #[serde(default, rename = "entry", skip_serializing_if = "Vec::is_empty")]
    entries: Vec<RawEntry>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawGame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thunderstore: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nexus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modio: Option<u64>,
}

/// Every field any platform has, optional, so that a missing or misplaced one is reported by name.
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    key: Option<String>,
    order: Option<u32>,
    platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    game: Option<u64>,
    #[serde(default, rename = "mod", skip_serializing_if = "Option::is_none")]
    mod_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    track: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dll_overrides: Vec<RawOverride>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOverride {
    dll: String,
    mode: String,
}

impl From<&Pack> for RawPack {
    #[inline]
    fn from(pack: &Pack) -> Self {
        Self {
            schema: SCHEMA,
            id: Some(pack.id.as_str().to_owned()),
            name: Some(pack.name.clone()),
            revision: Some(pack.revision),
            description: pack.description.clone(),
            game: RawGame {
                title: pack.game.title.clone(),
                build_hint: pack.game.build_hint.clone(),
                thunderstore: pack.game.thunderstore.clone(),
                nexus: pack.game.nexus.clone(),
                modio: pack.game.modio,
            },
            entries: pack.entries.iter().map(RawEntry::from).collect(),
        }
    }
}

impl From<&Entry> for RawEntry {
    #[inline]
    fn from(entry: &Entry) -> Self {
        let mut raw = Self {
            key: Some(entry.key.as_str().to_owned()),
            order: Some(entry.order),
            platform: Some(entry.source.platform().to_owned()),
            track: Some(entry.track.as_str().to_owned()),
            enabled: Some(entry.enabled),
            auto: entry.auto.then_some(true),
            target: entry.target.as_ref().map(|target| target.as_str().to_owned()),
            name: entry.name.clone(),
            author: entry.author.clone(),
            md5: entry.md5.as_ref().map(|md5| md5.as_str().to_owned()),
            sha256: entry.sha256.as_ref().map(|sha256| sha256.as_str().to_owned()),
            size: entry.size,
            requires: entry.requires.iter().map(|key| key.as_str().to_owned()).collect(),
            after: entry.after.iter().map(|key| key.as_str().to_owned()).collect(),
            dll_overrides: entry
                .dll_overrides
                .iter()
                .map(|wanted| RawOverride {
                    dll: wanted.dll.clone(),
                    mode: wanted.mode.as_str().to_owned(),
                })
                .collect(),
            ..Self::default()
        };
        match &entry.source {
            Source::Thunderstore { package, version } => {
                raw.package = Some(package.clone());
                raw.version = Some(version.clone());
            }
            Source::Nexus {
                domain,
                mod_id,
                file,
                version,
            } => {
                raw.domain = Some(domain.clone());
                raw.mod_id = Some(*mod_id);
                raw.file = Some(*file);
                raw.version.clone_from(version);
            }
            Source::Modio {
                game,
                mod_id,
                file,
                version,
            } => {
                raw.game = Some(*game);
                raw.mod_id = Some(*mod_id);
                raw.file = Some(*file);
                raw.version.clone_from(version);
            }
            Source::Local { file_name } => raw.file_name = Some(file_name.clone()),
        }
        raw
    }
}

pub(crate) fn decode(text: &str) -> Result<Pack> {
    // The schema first, without denying unknown fields, so a newer file is refused with the
    // version it needs rather than with the first field this decanter does not know.
    let probe: Probe = toml::from_str(text).map_err(|err| Error::syntax(text, &err))?;
    match probe.schema {
        Some(SCHEMA) => {}
        Some(found) => return Err(Error::Schema { found }),
        None => return Err(Error::invalid(Field::top("schema"), Reason::Missing)),
    }
    let raw: RawPack = toml::from_str(text).map_err(|err| Error::syntax(text, &err))?;
    validate(raw)
}

pub(crate) fn encode(pack: &Pack) -> Result<String> {
    toml::to_string(&RawPack::from(pack)).map_err(|err| Error::Encode(err.to_string()))
}

fn validate(raw: RawPack) -> Result<Pack> {
    let top = |name, reason| Error::invalid(Field::top(name), reason);
    let id = required(raw.id, Field::top("id"))?
        .parse()
        .map_err(|reason| top("id", reason))?;
    let name = display_text(&required(raw.name, Field::top("name"))?).map_err(|reason| top("name", reason))?;
    let revision = required(raw.revision, Field::top("revision"))?;
    let description = raw
        .description
        .map(|text| display_text(&text))
        .transpose()
        .map_err(|reason| top("description", reason))?;
    let game = validate_game(raw.game)?;
    if raw.entries.len() > MAX_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    let mut entries = Vec::with_capacity(raw.entries.len());
    let mut keys = HashMap::with_capacity(raw.entries.len());
    let mut orders = HashSet::with_capacity(raw.entries.len());
    let mut sources = HashSet::with_capacity(raw.entries.len());
    for (index, item) in raw.entries.into_iter().enumerate() {
        let entry = validate_entry(index, item)?;
        if keys.insert(entry.key.clone(), index).is_some() {
            return Err(Error::invalid(Field::entry(index, "key"), Reason::Duplicate));
        }
        if !orders.insert(entry.order) {
            return Err(Error::invalid(Field::entry(index, "order"), Reason::Duplicate));
        }
        if let Some((blamed, same)) = same_mod(&entry)
            && !sources.insert(same)
        {
            return Err(Error::invalid(Field::entry(index, blamed), Reason::Duplicate));
        }
        entries.push(entry);
    }
    check_references(&entries, &keys)?;
    Ok(Pack {
        id,
        name,
        revision,
        description,
        game,
        entries,
    })
}

fn validate_game(raw: RawGame) -> Result<GameHint> {
    let top = |name| move |reason| Error::invalid(Field::top(name), reason);
    Ok(GameHint {
        title: raw
            .title
            .map(|text| display_text(&text))
            .transpose()
            .map_err(top("title"))?,
        build_hint: raw
            .build_hint
            .map(|text| display_text(&text))
            .transpose()
            .map_err(top("build_hint"))?,
        thunderstore: raw
            .thunderstore
            .map(|text| community(&text))
            .transpose()
            .map_err(top("thunderstore"))?,
        nexus: raw.nexus.map(|text| domain(&text)).transpose().map_err(top("nexus"))?,
        modio: raw.modio.map(nonzero).transpose().map_err(top("modio"))?,
    })
}

fn validate_entry(index: usize, mut raw: RawEntry) -> Result<Entry> {
    let field = |name| Field::entry(index, name);
    let bad = |name| move |reason| Error::invalid(Field::entry(index, name), reason);
    let key = required(raw.key.take(), field("key"))?.parse().map_err(bad("key"))?;
    let order = required(raw.order, field("order"))?;
    let track = match raw.track.as_deref() {
        None | Some("pinned") => Track::Pinned,
        Some("latest") => Track::Latest,
        Some(_) => return Err(Error::invalid(field("track"), Reason::Unknown)),
    };
    let source = validate_source(index, track, &mut raw)?;
    let target = raw
        .target
        .map(|text| ModPath::parse(&text))
        .transpose()
        .map_err(|_err| Error::invalid(field("target"), Reason::Path))?;
    let size = raw
        .size
        .map(|size| {
            if (1..=MAX_MOD_BYTES).contains(&size) {
                Ok(size)
            } else {
                Err(Reason::OutOfRange)
            }
        })
        .transpose()
        .map_err(bad("size"))?;
    Ok(Entry {
        key,
        order,
        source,
        track,
        enabled: raw.enabled.unwrap_or(true),
        auto: raw.auto.unwrap_or(false),
        target,
        name: raw
            .name
            .map(|text| display_text(&text))
            .transpose()
            .map_err(bad("name"))?,
        author: raw
            .author
            .map(|text| display_text(&text))
            .transpose()
            .map_err(bad("author"))?,
        md5: raw.md5.map(|text| text.parse()).transpose().map_err(bad("md5"))?,
        sha256: raw.sha256.map(|text| text.parse()).transpose().map_err(bad("sha256"))?,
        size,
        dll_overrides: overrides(index, raw.dll_overrides)?,
        requires: keys(index, "requires", raw.requires)?,
        after: keys(index, "after", raw.after)?,
    })
}

/// The platform's fields, and none of another platform's.
fn validate_source(index: usize, track: Track, raw: &mut RawEntry) -> Result<Source> {
    let field = |name| Field::entry(index, name);
    let bad = |name| move |reason| Error::invalid(Field::entry(index, name), reason);
    let platform = required(raw.platform.take(), field("platform"))?;
    let present = [
        ("package", raw.package.is_some()),
        ("domain", raw.domain.is_some()),
        ("game", raw.game.is_some()),
        ("mod", raw.mod_id.is_some()),
        ("file", raw.file.is_some()),
        ("file_name", raw.file_name.is_some()),
        ("version", raw.version.is_some()),
        ("md5", raw.md5.is_some()),
    ];
    let allowed: &[&str] = match platform.as_str() {
        "thunderstore" => &["package", "version"],
        "nexus" => &["domain", "mod", "file", "version", "md5"],
        "modio" => &["game", "mod", "file", "version", "md5"],
        "local" => &["file_name"],
        _ => return Err(Error::invalid(field("platform"), Reason::Unknown)),
    };
    if let Some((name, _)) = present
        .iter()
        .find(|(name, is_present)| *is_present && !allowed.contains(name))
    {
        return Err(Error::invalid(field(name), Reason::NotForPlatform));
    }
    let source = match platform.as_str() {
        "thunderstore" => Source::Thunderstore {
            package: package(&required(raw.package.take(), field("package"))?).map_err(bad("package"))?,
            version: version_number(&required(raw.version.take(), field("version"))?).map_err(bad("version"))?,
        },
        "nexus" => Source::Nexus {
            domain: domain(&required(raw.domain.take(), field("domain"))?).map_err(bad("domain"))?,
            mod_id: nexus_id(required(raw.mod_id.take(), field("mod"))?).map_err(bad("mod"))?,
            file: nexus_id(required(raw.file.take(), field("file"))?).map_err(bad("file"))?,
            version: raw
                .version
                .take()
                .map(|text| version_token(&text))
                .transpose()
                .map_err(bad("version"))?,
        },
        "modio" => Source::Modio {
            game: nonzero(required(raw.game.take(), field("game"))?).map_err(bad("game"))?,
            mod_id: nonzero(required(raw.mod_id.take(), field("mod"))?).map_err(bad("mod"))?,
            file: nonzero(required(raw.file.take(), field("file"))?).map_err(bad("file"))?,
            version: raw
                .version
                .take()
                .map(|text| version_token(&text))
                .transpose()
                .map_err(bad("version"))?,
        },
        _ => {
            if track == Track::Latest {
                return Err(Error::invalid(field("track"), Reason::NotForPlatform));
            }
            Source::Local {
                file_name: file_name(&required(raw.file_name.take(), field("file_name"))?).map_err(bad("file_name"))?,
            }
        }
    };
    Ok(source)
}

fn overrides(index: usize, raw: Vec<RawOverride>) -> Result<Vec<DllOverride>> {
    let field = Field::entry(index, "dll_overrides");
    if raw.len() > MAX_LIST {
        return Err(Error::invalid(field, Reason::TooMany));
    }
    let mut found: Vec<DllOverride> = Vec::with_capacity(raw.len());
    for wanted in raw {
        let mode = wanted
            .mode
            .parse::<DllMode>()
            .map_err(|_err| Error::invalid(field, Reason::Unknown))?;
        let wanted = DllOverride::new(&wanted.dll, mode).map_err(|_err| Error::invalid(field, Reason::Characters))?;
        if found.iter().any(|known| known.dll == wanted.dll) {
            return Err(Error::invalid(field, Reason::Duplicate));
        }
        found.push(wanted);
    }
    Ok(found)
}

fn keys(index: usize, name: &'static str, raw: Vec<String>) -> Result<Vec<EntryKey>> {
    let field = Field::entry(index, name);
    if raw.len() > MAX_LIST {
        return Err(Error::invalid(field, Reason::TooMany));
    }
    let mut found: Vec<EntryKey> = Vec::with_capacity(raw.len());
    for text in raw {
        let key = text.parse().map_err(|reason| Error::invalid(field, reason))?;
        if found.contains(&key) {
            return Err(Error::invalid(field, Reason::Duplicate));
        }
        found.push(key);
    }
    Ok(found)
}

/// Every `requires` and `after` names another entry of the pack, and all of them together stay
/// under the edge cap the resolver is sized for.
fn check_references(entries: &[Entry], keys: &HashMap<EntryKey, usize>) -> Result<()> {
    let mut edges = 0_usize;
    for (index, entry) in entries.iter().enumerate() {
        for (name, list) in [("requires", &entry.requires), ("after", &entry.after)] {
            edges = edges.saturating_add(list.len());
            if edges > MAX_EDGES {
                return Err(Error::TooManyEdges);
            }
            for key in list {
                if *key == entry.key {
                    return Err(Error::invalid(Field::entry(index, name), Reason::SelfReference));
                }
                if !keys.contains_key(key) {
                    return Err(Error::invalid(Field::entry(index, name), Reason::UnknownReference));
                }
            }
        }
    }
    Ok(())
}

/// What makes two entries the same mod, and the field to blame: a Nexus mod may rightly appear once
/// per file (a main file and its optional ones), the others once per mod. A local folder has no
/// identity to compare.
fn same_mod(entry: &Entry) -> Option<(&'static str, String)> {
    match &entry.source {
        Source::Thunderstore { package, .. } => Some(("package", format!("thunderstore:{package}"))),
        Source::Nexus {
            domain, mod_id, file, ..
        } => Some(("file", format!("nexus:{domain}:{mod_id}:{file}"))),
        Source::Modio { game, mod_id, .. } => Some(("mod", format!("modio:{game}:{mod_id}"))),
        Source::Local { .. } => entry
            .sha256
            .as_ref()
            .map(|sha256| ("sha256", format!("local:{}", sha256.as_str()))),
    }
}

fn required<T>(value: Option<T>, field: Field) -> Result<T> {
    value.ok_or_else(|| Error::invalid(field, Reason::Missing))
}

/// `Team-Name`, both parts as Thunderstore's package format page allows (as decanter-thunderstore's
/// `PackageId`).
fn package(text: &str) -> core::result::Result<String, Reason> {
    let (namespace, name) = text.split_once('-').ok_or(Reason::Characters)?;
    let is_name_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    token(namespace, MAX_PACKAGE_PART, is_name_byte)?;
    token(name, MAX_PACKAGE_PART, is_name_byte)?;
    Ok(text.to_owned())
}

/// `major.minor.patch`, Thunderstore's `version_number`.
fn version_number(text: &str) -> core::result::Result<String, Reason> {
    let mut parts = 0_usize;
    for part in text.split('.') {
        parts = parts.saturating_add(1);
        token(part, 9, |byte| byte.is_ascii_digit())?;
    }
    if parts == 3 {
        Ok(text.to_owned())
    } else {
        Err(Reason::Characters)
    }
}

/// The token `ModOrigin.version` keeps for Nexus and mod.io.
fn version_token(text: &str) -> core::result::Result<String, Reason> {
    token(text, MAX_VERSION, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
    })
}

fn community(text: &str) -> core::result::Result<String, Reason> {
    token(text, MAX_COMMUNITY, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
    })
}

fn domain(text: &str) -> core::result::Result<String, Reason> {
    token(text, MAX_DOMAIN, |byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
    })
}

const fn nexus_id(value: u64) -> core::result::Result<u64, Reason> {
    if value > 0 && value <= MAX_NEXUS_ID {
        Ok(value)
    } else {
        Err(Reason::OutOfRange)
    }
}

const fn nonzero(value: u64) -> core::result::Result<u64, Reason> {
    if value > 0 { Ok(value) } else { Err(Reason::OutOfRange) }
}

/// Display text that also reads as one file's name, since it is what the user is asked to pick.
fn file_name(text: &str) -> core::result::Result<String, Reason> {
    let name = display_text(text)?;
    if name.contains(['/', '\\']) {
        return Err(Reason::Characters);
    }
    Ok(name)
}
