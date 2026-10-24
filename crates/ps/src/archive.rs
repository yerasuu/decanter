//! Zip archives from the network, read with the system's `tar` ([`platform::zip_tar`]) and checked
//! before and after it extracts anything: psport's Windows package ([`crate::package`]) and the
//! pieces of the Windows toolchain ([`crate::winbuild`]).
//!
//! Every name is checked from the listing first: one top folder (or none, as [`Top`] asks), names
//! Windows takes as they are, no links or special files, nothing listed twice regardless of case,
//! and the entry count and sizes under the archive's own [`Limits`]. Only then does `tar` extract it,
//! into a staging folder, and what lands there must be exactly what was listed, file by file and
//! size by size. Nothing extracted is ever run here.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::platform;

const MAX_NAME_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("tar could not read the archive: {0}")]
    Tar(String),
    #[error("the archive holds {0}")]
    Layout(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// What an archive may hold at most, sized for each archive from what it is known to hold.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Files and folders together, as `tar -t` lists them.
    pub entries: usize,
    /// Every file, unpacked.
    pub unpacked_bytes: u64,
    pub file_bytes: u64,
}

/// Where an archive's files sit.
#[derive(Debug, Clone, Copy)]
pub enum Top<'name> {
    /// In one folder, whatever its name.
    AnyFolder,
    /// In one folder of this name.
    Folder(&'name str),
    /// At the archive's top, with no folder of their own (Ninja's `ninja-win.zip`).
    Flat,
}

/// Which names an archive may use beyond [`valid_component`]'s.
#[derive(Debug, Clone, Copy)]
pub enum Names {
    Plain,
    /// Also spaces inside a name, never first or last, as CMake's documentation has
    /// (`Unix Makefiles.html`), and `@`, as gettext's locale folders have (`sr@latin`).
    Toolchain,
}

/// The top folder of a listing, if any, and its files with their sizes.
type Listing = (Option<String>, BTreeMap<String, u64>);

/// What [`unpack`] extracted: the folder holding the files, and each file as a `/`-separated path
/// below it with its size.
pub struct Unpacked {
    pub dir: PathBuf,
    pub files: BTreeMap<String, u64>,
}

/// Lists `archive` twice, its names and their kinds and sizes, checks both, extracts it into
/// `staging` and checks what landed there against the listing.
///
/// # Errors
/// When `tar` can't read it, the listing or what lands breaks any of the rules above, or `staging`
/// can't be made.
pub async fn unpack(
    archive: &Path,
    staging: &Path,
    top: Top<'_>,
    names: Names,
    limits: Limits,
) -> Result<Unpacked, Error> {
    let listed = match tar(&[OsStr::new("-t"), OsStr::new("-f"), archive.as_os_str()]).await {
        Ok(listed) => listed,
        Err(err) => return Err(err),
    };
    let kinds = match tar(&[
        OsStr::new("-t"),
        OsStr::new("-v"),
        OsStr::new("-f"),
        archive.as_os_str(),
    ])
    .await
    {
        Ok(kinds) => kinds,
        Err(err) => return Err(err),
    };
    let (folder, files) = match check_listing(&listed, &kinds, top, names, limits) {
        Ok(checked) => checked,
        Err(err) => return Err(err),
    };
    let _ = tokio::fs::remove_dir_all(staging).await;
    if let Err(err) = tokio::fs::create_dir_all(staging).await {
        return Err(err.into());
    }
    if let Err(err) = tar(&[
        OsStr::new("-x"),
        OsStr::new("-f"),
        archive.as_os_str(),
        OsStr::new("-C"),
        staging.as_os_str(),
    ])
    .await
    {
        return Err(err);
    }
    let dir = folder.map_or_else(|| staging.to_owned(), |folder| staging.join(folder));
    let checked = {
        let (staging, dir) = (staging.to_owned(), dir.clone());
        tokio::task::spawn_blocking(move || check_extracted(&staging, &dir, &files).map(|()| files)).await
    };
    match checked {
        Ok(Ok(files)) => Ok(Unpacked { dir, files }),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(io::Error::other(err).into()),
    }
}

/// `tar` with `args`, its listing as text. Nothing it reads is ever run.
async fn tar(args: &[&OsStr]) -> Result<String, Error> {
    let mut command = Command::new(platform::zip_tar());
    command
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    decanter_core::strip_launcher_env(command.as_std_mut());
    platform::without_console(&mut command);
    let output = match command.output().await {
        Ok(output) => output,
        Err(err) => return Err(Error::Tar(err.to_string())),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tar(stderr.lines().last().unwrap_or("failed").to_owned()));
    }
    String::from_utf8(output.stdout).map_err(|err| Error::Layout(format!("names that aren't UTF-8: {err}")))
}

/// The top folder every name sits in (`None` for a [`Top::Flat`] archive), and the files below it as
/// `/`-separated paths with their sizes, from the names `tar -t` lists and the `ls`-style lines
/// `tar -tv` lists for them in the same order (kind first, size fifth): only plain files and
/// folders, each name made of [`valid_component`]s and told apart from the others regardless of
/// case, as Windows tells them, within `limits`.
fn check_listing(names: &str, kinds: &str, top: Top<'_>, allowed: Names, limits: Limits) -> Result<Listing, Error> {
    let names: Vec<&str> = names.lines().collect();
    let kinds: Vec<&str> = kinds.lines().collect();
    if names.is_empty() || names.len() > limits.entries || names.len() != kinds.len() {
        return Err(Error::Layout(format!(
            "{} names for {} entries, at most {} expected",
            names.len(),
            kinds.len(),
            limits.entries
        )));
    }
    let mut folder: Option<&str> = None;
    let mut seen = HashSet::with_capacity(names.len());
    let mut files = BTreeMap::new();
    let mut total: u64 = 0;
    for (name, kind) in names.into_iter().zip(kinds) {
        let shown: String = name.chars().take(200).collect();
        let is_dir = match kind.bytes().next() {
            Some(b'd') => true,
            Some(b'-') => false,
            _ => return Err(Error::Layout(format!("a link or special file: {shown}"))),
        };
        let path = name.strip_suffix('/').unwrap_or(name);
        if !is_dir && path.len() != name.len() {
            return Err(Error::Layout(format!("a file named as a folder: {shown}")));
        }
        let mut components = path.split('/');
        if !matches!(top, Top::Flat) {
            let first = components.next().unwrap_or_default();
            let expected = match top {
                Top::Folder(expected) => first == expected,
                Top::AnyFolder | Top::Flat => folder.is_none_or(|folder| folder == first),
            };
            if !valid_component(first, allowed) || !expected {
                return Err(Error::Layout(format!("{shown} outside its one folder")));
            }
            folder = Some(first);
        }
        let rest: Vec<&str> = components.collect();
        if !rest.iter().all(|component| valid_component(component, allowed)) {
            return Err(Error::Layout(format!("a name Windows can't take as it is: {shown}")));
        }
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(Error::Layout(format!("{shown} twice")));
        }
        if !is_dir {
            if rest.is_empty() {
                return Err(Error::Layout(format!("{shown} beside its folder")));
            }
            let Some(size) = kind.split_whitespace().nth(4).and_then(|size| size.parse::<u64>().ok()) else {
                return Err(Error::Layout(format!("{shown} without a size tar can tell")));
            };
            total = total.saturating_add(size);
            if size > limits.file_bytes || total > limits.unpacked_bytes {
                return Err(Error::Layout(format!("{shown}, too large unpacked ({size} bytes)")));
            }
            files.insert(rest.join("/"), size);
        }
    }
    match top {
        Top::Flat if files.is_empty() => Err(Error::Layout("no file".to_owned())),
        Top::Flat => Ok((None, files)),
        Top::AnyFolder | Top::Folder(_) => folder
            .map(|folder| (Some(folder.to_owned()), files))
            .ok_or_else(|| Error::Layout("no folder".to_owned())),
    }
}

/// A file or folder name that means the same to `tar` and Windows: plain ASCII letters, digits and
/// `._+-` (and inner spaces and `@` where `allowed`), not `.` or `..`, no trailing dot (Windows drops it) and
/// no device name such as `NUL` or `COM1`, with or without an extension ([Naming files, paths, and
/// namespaces]). Drive letters, separators, streams and absolute paths can't be spelled with it.
///
/// [Naming files, paths, and namespaces]: https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
pub(crate) fn valid_component(name: &str, allowed: Names) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered = ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix)
            .is_some_and(|number| number.len() == 1 && number.bytes().all(|byte| byte.is_ascii_digit()))
    });
    let wider = matches!(allowed, Names::Toolchain) && !name.starts_with(' ') && !name.ends_with(' ');
    (1..=MAX_NAME_BYTES).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'+' | b'-')
                || (wider && matches!(byte, b' ' | b'@'))
        })
        && name != "."
        && name != ".."
        && !name.ends_with('.')
        && !DEVICES.contains(&stem.as_str())
        && !numbered
}

/// `staging` holds `dir` alone (or is it), and `dir` exactly the `files` listed, as plain files of
/// their listed sizes and folders. Blocking file I/O.
fn check_extracted(staging: &Path, dir: &Path, files: &BTreeMap<String, u64>) -> Result<(), Error> {
    if staging != dir {
        let entries = match std::fs::read_dir(staging) {
            Ok(entries) => entries,
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            match entry {
                Ok(entry) if entry.path() == dir => {}
                Ok(entry) => {
                    return Err(Error::Layout(format!(
                        "{} beside its folder",
                        entry.file_name().to_string_lossy()
                    )));
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
    let mut found: usize = 0;
    let mut pending = vec![(dir.to_owned(), String::new())];
    while let Some((folder, prefix)) = pending.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => return Err(err.into()),
            };
            // Not followed: a link or junction is the entry itself.
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(err) => return Err(err.into()),
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_symlink() {
                return Err(Error::Layout(format!("a link: {relative}")));
            } else if kind.is_dir() {
                pending.push((entry.path(), relative));
            } else if kind.is_file()
                && let Some(&listed) = files.get(&relative)
            {
                let size = match entry.metadata() {
                    Ok(meta) => meta.len(),
                    Err(err) => return Err(err.into()),
                };
                if size != listed {
                    return Err(Error::Layout(format!(
                        "{relative} of {size} bytes, not the {listed} listed"
                    )));
                }
                found += 1;
            } else {
                return Err(Error::Layout(format!("{relative}, which it doesn't list")));
            }
        }
    }
    if found == files.len() {
        Ok(())
    } else {
        Err(Error::Layout(format!("{found} of the {} files it lists", files.len())))
    }
}
