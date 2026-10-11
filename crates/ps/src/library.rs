//! Game discovery: PS4 and PS5 dumps found under a profile's folders.
//!
//! The dump is the source of truth, the way a manifest is for the PC
//! library: nothing found here is cached, and every call reads the folders
//! again. That is file I/O over every folder, so callers run it off the UI
//! thread.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Ps4,
    Ps5,
}

#[derive(Clone, Debug)]
pub struct Game {
    pub dir: PathBuf,
    pub platform: Platform,
    pub title_id: String,
    pub title: String,
    /// `APP_VER` (PS4, e.g. `01.09`) or `contentVersion` (PS5, e.g. `01.000.000`).
    pub version: String,
    /// PS4 `CATEGORY`: `gd` for a game, `gp` for an update.
    pub category: String,
    /// Whether the dump carries `sce_module/`. An extracted update lacks it, while a base game
    /// with the update merged in keeps it (and takes the update's `gp` category).
    pub has_modules: bool,
}

impl Game {
    /// An update on its own, not playable; a merged copy is a game.
    pub fn is_update(&self) -> bool {
        self.platform == Platform::Ps4 && self.category.starts_with("gp") && !self.has_modules
    }
}

/// Values of a PS4 `param.sfo`, as `(key, value)` with integers rendered in decimal.
fn read_sfo(data: &[u8]) -> Option<Vec<(String, String)>> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize);
    if data.get(..4)? != b"\0PSF" {
        return None;
    }
    let (keys, values, count) = (u32_at(8)?, u32_at(12)?, u32_at(16)?);
    (0..count)
        .map(|i| {
            let entry = 20 + i * 16;
            let key_start = keys + u16_at(entry)? as usize;
            let key_len = data.get(key_start..)?.iter().position(|&b| b == 0)?;
            let key = String::from_utf8_lossy(&data[key_start..key_start + key_len]).into_owned();
            let (format, len, start) = (u16_at(entry + 2)?, u32_at(entry + 4)?, values + u32_at(entry + 12)?);
            let raw = data.get(start..start + len)?;
            let value = if format == 0x0404 {
                u32::from_le_bytes(raw.get(..4)?.try_into().ok()?).to_string()
            } else {
                String::from_utf8_lossy(raw).trim_end_matches('\0').to_owned()
            };
            Some((key, value))
        })
        .collect()
}

fn read_ps4(dir: &Path) -> Option<Game> {
    let values = read_sfo(&fs::read(dir.join("sce_sys/param.sfo")).ok()?)?;
    let get = |key: &str| {
        values
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    Some(Game {
        dir: dir.to_owned(),
        platform: Platform::Ps4,
        title_id: get("TITLE_ID"),
        title: get("TITLE"),
        version: get("APP_VER"),
        category: get("CATEGORY"),
        has_modules: dir.join("sce_module").is_dir(),
    })
}

fn read_ps5(dir: &Path) -> Option<Game> {
    let json: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("sce_sys/param.json")).ok()?).ok()?;
    let text = |value: &serde_json::Value| value.as_str().unwrap_or_default().to_owned();
    let localized = &json["localizedParameters"];
    let language = localized["defaultLanguage"].as_str().unwrap_or("en-US");
    Some(Game {
        dir: dir.to_owned(),
        platform: Platform::Ps5,
        title_id: text(&json["titleId"]),
        title: text(&localized[language]["titleName"]),
        version: text(&json["contentVersion"]),
        category: String::new(),
        has_modules: dir.join("sce_module").is_dir(),
    })
}

/// The title id names directories decanter writes (conversions, a
/// profile's saves), so a dump whose metadata says anything other than
/// letters and digits (`CUSA00900`, `PPSA02929`) is not taken as a game.
fn valid_title_id(title_id: &str) -> bool {
    (1..=16).contains(&title_id.len()) && title_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn read_game(dir: &Path) -> Option<Game> {
    let game = read_ps4(dir).or_else(|| read_ps5(dir))?;
    if valid_title_id(&game.title_id) {
        Some(game)
    } else {
        tracing::warn!(dir = %dir.display(), title_id = game.title_id, "dump skipped: unusable title id");
        None
    }
}

/// Dumps in each root and its direct subfolders, sorted by title then version.
pub fn scan(roots: &[PathBuf]) -> Vec<Game> {
    let mut games: Vec<Game> = roots
        .iter()
        .flat_map(|root| {
            let children = fs::read_dir(root)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path());
            std::iter::once(root.clone()).chain(children)
        })
        .filter(|dir| dir.is_dir())
        .filter_map(|dir| read_game(&dir))
        .collect();
    games.sort_by(|a, b| (&a.title, &a.version, &a.dir).cmp(&(&b.title, &b.version, &b.dir)));
    games.dedup_by(|a, b| a.dir == b.dir);
    games
}
