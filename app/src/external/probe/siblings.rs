//! What a Linux game ships beside its executable: a `.desktop` file naming
//! it and its icon, and images named for what they are. Only the
//! executable's own folder is looked at, never the system's icon themes.

use std::path::{Component, Path};

use super::source::Source;
use super::{Art, Image};

/// Directory entries looked at in the executable's folder.
const MAX_DIR_ENTRIES: usize = 512;
const MAX_DESKTOP_SIZE: usize = 64 * 1024;
const MAX_IMAGE_SIZE: usize = 8 * 1024 * 1024;
const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "ico"];
/// File stems that say which slot an image is for, the preferred first.
const ICON_STEMS: [&str; 2] = ["icon", "logo"];
const COVER_STEMS: [&str; 2] = ["cover", "capsule"];
const HERO_STEMS: [&str; 2] = ["hero", "header"];

pub(super) struct Desktop {
    pub(super) name: Option<String>,
    pub(super) icon: Option<String>,
}

/// The file names in `folder`, at most [`MAX_DIR_ENTRIES`], sorted so the
/// pick doesn't depend on the order the file system lists them in.
fn names(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .take(MAX_DIR_ENTRIES)
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort_unstable();
    names
}

/// The whole of a small regular file, or `None` past `cap`.
fn read_small(path: &Path, cap: usize) -> Option<Vec<u8>> {
    let mut source = Source::open(path).ok()?;
    if usize::try_from(source.len()).ok()? > cap {
        return None;
    }
    source.read_up_to(0, cap)
}

/// The `.desktop` file beside `executable_name` whose `Exec=` names it,
/// else the only one there.
pub(super) fn desktop(folder: &Path, executable_name: &str) -> Option<Desktop> {
    let parsed: Vec<(Desktop, bool)> = names(folder)
        .iter()
        .filter(|name| name.to_ascii_lowercase().ends_with(".desktop"))
        .filter_map(|name| {
            let bytes = read_small(&folder.join(name), MAX_DESKTOP_SIZE)?;
            Some(parse_desktop(&String::from_utf8_lossy(&bytes), executable_name))
        })
        .collect();
    if let Some(index) = parsed.iter().position(|(_, names_it)| *names_it) {
        return parsed.into_iter().nth(index).map(|(desktop, _)| desktop);
    }
    let mut parsed = parsed.into_iter();
    match (parsed.next(), parsed.next()) {
        (Some((only, _)), None) => Some(only),
        _ => None,
    }
}

/// The unlocalized `Name=` and `Icon=` of the `[Desktop Entry]` group, and
/// whether its `Exec=` mentions `executable_name`.
fn parse_desktop(text: &str, executable_name: &str) -> (Desktop, bool) {
    let mut desktop = Desktop { name: None, icon: None };
    let mut names_it = false;
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" if desktop.name.is_none() => desktop.name = Some(value.to_owned()),
            "Icon" if desktop.icon.is_none() => desktop.icon = Some(value.to_owned()),
            "Exec" => names_it = value.contains(executable_name),
            _ => {}
        }
    }
    (desktop, names_it)
}

/// A `.desktop` file's `Icon=`: an absolute path to an image, or a name
/// matching a file next to the executable.
pub(super) fn desktop_icon(folder: &Path, icon: &str) -> Option<Image> {
    let path = Path::new(icon);
    if path.is_absolute() {
        return read_small(path, MAX_IMAGE_SIZE).and_then(super::icon::checked);
    }
    // Only a plain file name: anything with a folder in it could reach
    // outside the game's own.
    if path.components().count() != 1 || !matches!(path.components().next(), Some(Component::Normal(_))) {
        return None;
    }
    let names = names(folder);
    let lowered = icon.to_ascii_lowercase();
    names
        .iter()
        .filter(|name| {
            let name = name.to_ascii_lowercase();
            name == lowered
                || name
                    .rsplit_once('.')
                    .is_some_and(|(stem, extension)| stem == lowered && IMAGE_EXTENSIONS.contains(&extension))
        })
        .find_map(|name| read_small(&folder.join(name), MAX_IMAGE_SIZE).and_then(super::icon::checked))
}

/// Images beside the executable named for a slot (`icon.png`, `cover.jpg`,
/// `hero.png` and the like).
pub(super) fn images(folder: &Path) -> Art {
    let names = names(folder);
    let pick = |stems: &[&str]| {
        stems.iter().find_map(|wanted| {
            names
                .iter()
                .filter(|name| {
                    let name = name.to_ascii_lowercase();
                    name.rsplit_once('.')
                        .is_some_and(|(stem, extension)| stem == *wanted && IMAGE_EXTENSIONS.contains(&extension))
                })
                .find_map(|name| read_small(&folder.join(name), MAX_IMAGE_SIZE).and_then(super::icon::checked))
        })
    };
    Art {
        icon: pick(&ICON_STEMS),
        cover: pick(&COVER_STEMS),
        hero: pick(&HERO_STEMS),
    }
}
