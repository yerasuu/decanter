//! Games a launcher installed inside an external game's prefix — HoYoPlay's
//! Genshin Impact, GRYPHLINK's Arknights: Endfield — found so each can be
//! added as its own entry, running in that same prefix.
//!
//! Every launcher records its games its own way, so each gets a reader of
//! its own; there is no general "what's installed" in a Wine prefix to
//! fall back on (start-menu shortcuts are binary `.lnk` files, and HoYoPlay
//! doesn't make any for its games).

// TODO: Battle.net, once there's a real install to test against. Its games
// are listed in `C:\ProgramData\Battle.net\Agent\product.db`, a protobuf
// `Database` whose `product_install` (field 1) entries carry `uid` (1),
// `product_code` (2) and `settings.install_path` (3.1) — field numbers as
// Lutris reads them, unverified here. Its games mostly refuse to start from
// their own executable, so each would launch as `Battle.net.exe
// --exec="launch <product_code>"` (`Pro` is Overwatch, `Fen` Diablo IV,
// `WoW`, `S2`, `WTCG`...), which needs launch arguments in the external
// game's manifest first, and `Detected`/the "already added" check keyed by
// executable *and* arguments, since every one shares `Battle.net.exe`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One game found in a prefix.
#[derive(Clone)]
pub struct Detected {
    pub name: String,
    pub executable: PathBuf,
}

/// Everything every known launcher installed under the prefix at `root`
/// (the directory holding `drive_c`). Blocking: reads `user.reg` and a few
/// directories.
pub fn detect(root: &Path) -> Vec<Detected> {
    let registry = std::fs::read_to_string(root.join("user.reg"))
        .map(|text| parse_registry(&text))
        .unwrap_or_default();
    let prefix = decanter_runtime::Prefix::new(root);
    let mut found: Vec<Detected> = hoyoplay(&registry, &prefix);
    found.extend(gryphlink(&registry, &prefix));
    let mut seen = std::collections::HashSet::new();
    found.retain(|game| seen.insert(game.executable.clone()));
    tracing::debug!(prefix = %root.display(), count = found.len(), "detected launcher games");
    found
}

/// HoYoPlay's game id prefix ("biz"), the name to show, and the executables
/// it might start — the global and Chinese builds name theirs differently.
const HOYOPLAY_GAMES: [(&str, &str, &[&str]); 4] = [
    ("hk4e", "Genshin Impact", &["GenshinImpact.exe", "YuanShen.exe"]),
    ("hkrpg", "Honkai: Star Rail", &["StarRail.exe"]),
    ("nap", "Zenless Zone Zero", &["ZenlessZoneZero.exe"]),
    ("bh3", "Honkai Impact 3rd", &["BH3.exe"]),
];

/// HoYoPlay keeps one registry key per installed game,
/// `HKCU\Software\Cognosphere\HYP\1_0\<biz>` (or `miHoYo` for the Chinese
/// build), with `GameInstallPath` naming its folder.
fn hoyoplay(registry: &Registry, prefix: &decanter_runtime::Prefix) -> Vec<Detected> {
    registry
        .iter()
        .filter_map(|(key, values)| {
            let biz = ["software\\cognosphere\\hyp\\1_0\\", "software\\mihoyo\\hyp\\1_0\\"]
                .iter()
                .find_map(|base| key.strip_prefix(base))?;
            let (_, name, executables) = HOYOPLAY_GAMES.iter().find(|(id, _, _)| biz.starts_with(id))?;
            let folder = prefix.resolve_windows_path(values.get("gameinstallpath")?)?;
            let executable = executables
                .iter()
                .map(|exe| folder.join(exe))
                .find(|exe| exe.is_file())?;
            Some(Detected {
                name: (*name).to_owned(),
                executable,
            })
        })
        .collect()
}

/// Executables that ship beside a game but aren't it — crash reporters,
/// embedded browsers (`QtWebEngineProcess`), platform agents
/// (`PlatformProcess`, beside Endfield).
const HELPERS: [&str; 9] = [
    "crash",
    "uninstall",
    "unins",
    "setup",
    "launcher",
    "process",
    "helper",
    "report",
    "service",
];

/// Games whose executable is known, checked before guessing — a game folder
/// can carry more than one executable that isn't a helper.
const KNOWN_EXECUTABLES: [&str; 2] = ["Endfield.exe", "Arknights.exe"];

/// GRYPHLINK puts each game in its own folder under `<install>\games`; the
/// install directory is in its registry key, `C:\Program Files\GRYPHLINK`
/// by default. A game's folder is named after it, with one executable of
/// its own at the top.
fn gryphlink(registry: &Registry, prefix: &decanter_runtime::Prefix) -> Vec<Detected> {
    let installs = registry
        .iter()
        .filter(|(key, _)| key.starts_with("software\\gryphlink\\launcher"))
        .filter_map(|(_, values)| prefix.resolve_windows_path(values.get("install_path")?))
        .chain(prefix.resolve_windows_path("C:\\Program Files\\GRYPHLINK"));
    let mut found = Vec::new();
    for games in installs.map(|install| install.join("games")) {
        let Ok(entries) = std::fs::read_dir(&games) else {
            continue;
        };
        for folder in entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
            let Some(executable) = main_executable(&folder) else {
                continue;
            };
            let name = folder.file_name().map(|name| name.to_string_lossy().into_owned());
            found.extend(name.map(|name| Detected { name, executable }));
        }
    }
    found
}

/// A known game executable at the top of `folder`, else the one there that
/// isn't a helper, if there is exactly one — two leave it unclear which is
/// the game.
fn main_executable(folder: &Path) -> Option<PathBuf> {
    if let Some(known) = KNOWN_EXECUTABLES
        .iter()
        .map(|exe| folder.join(exe))
        .find(|exe| exe.is_file())
    {
        return Some(known);
    }
    let mut candidates = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            name.ends_with(".exe") && !HELPERS.iter().any(|helper| name.contains(helper)) && path.is_file()
        });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

/// `user.reg`'s keys, lowercased, each to its string values (names
/// lowercased too — the registry is case-insensitive).
type Registry = HashMap<String, HashMap<String, String>>;

/// Reads the plain-text format Wine keeps its registry hives in:
/// `[Key\\Path] stamp` headers, then `"name"="value"` lines with `\\`
/// escaping backslashes. Only string values matter here.
fn parse_registry(text: &str) -> Registry {
    let mut registry = Registry::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix('[') {
            current = header.split_once(']').map(|(key, _)| unescape(key).to_lowercase());
            continue;
        }
        let (Some(key), Some((name, value))) = (&current, line.split_once("\"=\"")) else {
            continue;
        };
        let (Some(name), Some(value)) = (name.strip_prefix('"'), value.strip_suffix('"')) else {
            continue;
        };
        registry
            .entry(key.clone())
            .or_default()
            .insert(name.to_lowercase(), unescape(value));
    }
    registry
}

fn unescape(text: &str) -> String {
    text.replace("\\\\", "\\")
}
