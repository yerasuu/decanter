//! Whether a PS4 game can play online, told by its main executable naming an NP online library.
//!
//! The answer is kept per game, keyed by the eboot's size and mtime, so an unchanged file is read
//! once. It lives in its own folder, not in `titles/`, whose entries count as games with settings.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::files::write_atomic;
use crate::paths::Root;
use crate::title_id::TitleId;

/// What a game that plays online with other people links: matchmaking, peer signalling, parties,
/// and the session APIs of newer titles (game intents to join a friend, the C++ and v2 web APIs).
/// `libSceNpWebApi` (v1) is left out because nearly every title links it for PSN friends, presence
/// and trophy sync, which says nothing about playing online.
const NAMES: [&[u8]; 6] = [
    b"libSceNpMatching2",
    b"libSceNpSessionSignaling",
    b"libSceNpParty",
    b"libSceNpGameIntent",
    b"libSceNpCppWebApi",
    b"libSceNpWebApi2",
];
const CHUNK: usize = 1 << 20;
/// Bumped when `NAMES` changes, so answers cached with the old list are scanned again.
const SCAN_VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct Cached {
    is_online: bool,
    mtime_nanos: u32,
    mtime_secs: i64,
    size: u64,
    version: u32,
}

/// Whether `game_dir`'s `eboot.bin` names an NP online library. Reads the whole file on a cache
/// miss. File I/O: off the UI thread.
///
/// # Errors
/// The eboot or the title id could not be read; a cache that can't be read or written only costs
/// a rescan and is logged.
pub fn is_online(root: &Root, game_dir: &Path, title_id: &str) -> io::Result<bool> {
    let id: TitleId = title_id
        .parse()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let eboot = game_dir.join("eboot.bin");
    let meta = fs::metadata(&eboot)?;
    let (mtime_secs, mtime_nanos) = meta
        .modified()
        .ok()
        .map(|time| match time.duration_since(UNIX_EPOCH) {
            Ok(after) => (i64::try_from(after.as_secs()).unwrap_or(0), after.subsec_nanos()),
            Err(_) => (0, 0),
        })
        .unwrap_or_default();
    let cache = root.join("online-scan").join(format!("{}.json", id.as_str()));
    if let Ok(text) = fs::read(&cache)
        && let Ok(cached) = serde_json::from_slice::<Cached>(&text)
        && cached.version == SCAN_VERSION
        && cached.size == meta.len()
        && cached.mtime_secs == mtime_secs
        && cached.mtime_nanos == mtime_nanos
    {
        return Ok(cached.is_online);
    }
    let is_online = names_online_library(File::open(&eboot)?)?;
    let cached = Cached {
        version: SCAN_VERSION,
        size: meta.len(),
        mtime_secs,
        mtime_nanos,
        is_online,
    };
    match serde_json::to_vec(&cached) {
        Ok(bytes) => {
            if let Err(err) = write_atomic(&cache, &bytes) {
                tracing::warn!(path = %cache.display(), %err, "online scan not cached");
            }
        }
        Err(err) => tracing::warn!(path = %cache.display(), %err, "online scan not cached"),
    }
    Ok(is_online)
}

fn names_online_library(mut input: impl Read) -> io::Result<bool> {
    let overlap = NAMES.iter().map(|name| name.len()).max().unwrap_or(1) - 1;
    let mut buf = vec![0; overlap + CHUNK];
    let mut kept = 0;
    loop {
        let read = match input.read(&mut buf[kept..]) {
            Ok(0) => return Ok(false),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let filled = kept + read;
        let window = &buf[..filled];
        let found = window.iter().enumerate().any(|(at, &byte)| {
            byte == b'l'
                && window
                    .get(at..)
                    .is_some_and(|rest| NAMES.iter().any(|name| rest.starts_with(name)))
        });
        if found {
            return Ok(true);
        }
        kept = overlap.min(filled);
        buf.copy_within(filled - kept..filled, 0);
    }
}
