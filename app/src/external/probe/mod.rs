//! What a file dropped on the window is, and what it says about itself:
//! read, never run. A Windows program by its PE headers and resources (its
//! version strings and icon), a Linux one by its ELF header or `#!`, plus
//! the `.desktop` file and images a Linux game ships beside itself.
//!
//! The file is untrusted and this is where it is checked, once: reads are
//! bounded by [`source::Source`], every offset taken from the file is
//! checked against what was read before it is followed, and nothing here
//! panics on a truncated or hostile file.

mod elf;
mod icon;
mod pe;
mod siblings;
mod source;

use std::path::{Path, PathBuf};

/// The system a dropped program is built for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Windows,
    Linux,
}

pub struct Found {
    /// Canonical: symlinks resolved, so the same file dropped through two
    /// links is one game.
    pub executable: PathBuf,
    pub kind: Kind,
    /// Never empty.
    pub title: String,
    pub art: Art,
}

#[derive(Default)]
pub struct Art {
    pub icon: Option<Image>,
    pub cover: Option<Image>,
    pub hero: Option<Image>,
}

pub struct Image {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

/// Why a dropped path isn't a game decanter can add.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("no such file")]
    Missing,
    #[error("a folder")]
    Directory,
    #[error("not a regular file")]
    Special,
    #[error("unreadable: {0}")]
    Unreadable(std::io::Error),
    #[error("a library, not a program")]
    Library,
    #[error("not a program decanter knows")]
    Unsupported,
}

const MAX_TITLE_CHARS: usize = 80;
/// Folders that say where a program sits in its game, not which game it
/// is, skipped when the title comes from a folder.
const GENERIC_FOLDERS: [&str; 28] = [
    "bin", "bin32", "bin64", "binaries", "win", "win32", "win64", "windows", "wingdk", "x64", "x86", "x86_64", "amd64",
    "linux", "linux32", "linux64", "game", "games", "release", "shipping", "retail", "app", "build", "common", "usr",
    "opt", "lib", "lib64",
];
/// What an AppImage's file name carries after the application's name.
const APPIMAGE_SUFFIXES: [&str; 8] = ["x86", "x64", "amd64", "aarch64", "arm64", "i386", "i686", "linux"];
/// What engines put in a program's version strings when the game didn't
/// set its own: not a title.
const ENGINE_PRODUCT_NAMES: [&str; 4] = ["unity", "unity player", "unreal engine", "bootstrappackagedgame"];
/// Folders above the executable looked at for a title.
const MAX_TITLE_FOLDERS: usize = 4;

/// Reads `path` and says what it is. Blocking: opens the file and, for a
/// Linux program, the files beside it.
///
/// # Errors
///
/// Returns a [`Refusal`] for anything that isn't a Windows or Linux
/// program, or that can't be read.
pub fn probe(path: &Path) -> Result<Found, Refusal> {
    let executable = std::fs::canonicalize(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Refusal::Missing
        } else {
            Refusal::Unreadable(err)
        }
    })?;
    let metadata = std::fs::metadata(&executable).map_err(Refusal::Unreadable)?;
    if metadata.is_dir() {
        return Err(Refusal::Directory);
    }
    if !metadata.is_file() {
        return Err(Refusal::Special);
    }
    let mut source = source::Source::open(&executable).map_err(Refusal::Unreadable)?;
    let magic = source.read_up_to(0, 4).unwrap_or_default();
    if magic.starts_with(b"MZ") {
        return match pe::header(&mut source) {
            pe::Header::Program(program) => {
                let title = program
                    .version_name(&mut source)
                    .and_then(|name| sanitize(&name))
                    .filter(|name| !ENGINE_PRODUCT_NAMES.contains(&name.to_lowercase().as_str()));
                let icon = program.icon(&mut source).map(|bytes| Image {
                    bytes,
                    extension: "png",
                });
                Ok(Found {
                    title: title_or_path(title, &executable),
                    executable,
                    kind: Kind::Windows,
                    art: Art { icon, ..Art::default() },
                })
            }
            pe::Header::Library => Err(Refusal::Library),
            pe::Header::Other => Err(Refusal::Unsupported),
        };
    }
    // The launch gives a registered program the permission to run (see
    // `launch::linux_native::allow_to_run`), so a missing exec bit is no reason to refuse.
    if magic.starts_with(b"#!") {
        return Ok(linux(executable));
    }
    if magic != b"\x7fELF" {
        return Err(Refusal::Unsupported);
    }
    if is_shared_object_name(&executable) {
        return Err(Refusal::Library);
    }
    match elf::read(&mut source) {
        Some(elf::Elf::Program) => {}
        Some(elf::Elf::Other) | None => return Err(Refusal::Unsupported),
    }
    Ok(linux(executable))
}

/// `libfoo.so` or `libfoo.so.6`: a library, even with an interpreter and
/// the permission to run, as glibc's own has.
fn is_shared_object_name(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .is_some_and(|name| name.ends_with(".so") || name.contains(".so."))
}

/// A Linux program's title and art, from its `.desktop` file and the
/// images beside it.
fn linux(executable: PathBuf) -> Found {
    let folder = executable.parent().map(Path::to_path_buf).unwrap_or_default();
    let file_name = executable
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let desktop = siblings::desktop(&folder, &file_name);
    let mut art = siblings::images(&folder);
    if let Some(icon) = desktop
        .as_ref()
        .and_then(|desktop| desktop.icon.as_deref())
        .and_then(|icon| siblings::desktop_icon(&folder, icon))
    {
        art.icon = Some(icon);
    }
    // An AppImage is a whole application in one file, kept wherever the
    // user keeps downloads, so its folder says nothing about it.
    let title = desktop
        .and_then(|desktop| desktop.name)
        .and_then(|name| sanitize(&name))
        .or_else(|| appimage_name(&executable));
    Found {
        title: title_or_path(title, &executable),
        executable,
        kind: Kind::Linux,
        art,
    }
}

/// Whether a game added earlier with `executable` runs as a Linux program
/// rather than under Proton: the same header check [`probe`] makes, by
/// the first bytes only.
pub fn is_native_linux(executable: &Path) -> bool {
    source::Source::open(executable)
        .ok()
        .and_then(|mut source| source.read_up_to(0, 4))
        .is_some_and(|magic| magic == b"\x7fELF" || magic.starts_with(b"#!"))
}

/// An AppImage's file name up to its version or architecture:
/// `Game-1.2.0-x86_64.AppImage` is `Game`.
fn appimage_name(executable: &Path) -> Option<String> {
    if !executable
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("appimage"))
    {
        return None;
    }
    let stem = executable.file_stem()?.to_string_lossy();
    let cut = stem
        .match_indices(['-', '_', ' '])
        .map(|(at, _)| at)
        .find(|&at| {
            let rest = stem
                .get(at.saturating_add(1)..)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let is_version = rest.trim_start_matches('v').starts_with(|c: char| c.is_ascii_digit());
            is_version || APPIMAGE_SUFFIXES.iter().any(|suffix| rest.starts_with(suffix))
        })
        .unwrap_or(stem.len());
    sanitize(stem.get(..cut).unwrap_or(&stem))
}

/// `title`, else the nearest folder that names the game, else the file's
/// own name.
fn title_or_path(title: Option<String>, executable: &Path) -> String {
    title
        .or_else(|| {
            executable
                .ancestors()
                .skip(1)
                .take(MAX_TITLE_FOLDERS)
                .filter_map(|folder| folder.file_name()?.to_str())
                .find(|name| !GENERIC_FOLDERS.contains(&name.to_lowercase().as_str()))
                .and_then(sanitize)
        })
        .or_else(|| {
            executable
                .file_stem()
                .and_then(|stem| sanitize(&stem.to_string_lossy()))
        })
        .unwrap_or_else(|| executable.to_string_lossy().chars().take(MAX_TITLE_CHARS).collect())
}

/// Text from a file made fit to show: no control or bidirectional
/// override characters, whitespace collapsed, at most
/// [`MAX_TITLE_CHARS`]; `None` when nothing is left.
fn sanitize(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|&c| !c.is_control() && !is_bidi_control(c))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}
