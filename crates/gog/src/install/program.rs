//! Guessing which installed file launches the game, once every byte has
//! landed.
//!
//! The answer is recorded in the appmanifest as a path relative to the
//! install directory, with forward slashes — the same shape the depot
//! manifest addresses files by, so it stays comparable to the file list
//! and doesn't bake in where the library happens to live.

use std::fs::{File, symlink_metadata};
use std::io::{Cursor, Read as _};
use std::path::Path;

use anyhow::{Context as _, ensure};
use decanter_core::{GameSystem, InstalledGame, platform};
use decanter_galaxy::ClientId;

/// Far above any real Info.plist, which runs to a few kilobytes.
const INFO_PLIST_MAX_BYTES: u64 = 1024 * 1024;

/// Best-effort: an install with no identifiable executable is still a
/// valid install, just not launchable until someone points at one by hand.
pub(super) fn find_launch_executable(
    install_root: &Path,
    relative_paths: &[String],
    install_directory: &str,
    product_id: &str,
    system: GameSystem,
) -> Option<String> {
    let found = match system {
        GameSystem::Windows => find_goggame_primary_executable(install_root, relative_paths)
            .or_else(|| find_primary_executable_heuristic(relative_paths, install_directory)),
        GameSystem::MacOs => find_bundle_executable(install_root, relative_paths, product_id)
            .or_else(|| find_goggame_primary_executable(install_root, relative_paths)),
    };
    match &found {
        Some(executable) => tracing::debug!(product_id, executable, "found launch executable"),
        None => tracing::warn!(product_id, "couldn't determine a launch executable"),
    }
    found
}

/// The program a macOS build's app bundle names, which a repair gives its
/// mode back as [`find_bundle_executable`] did on install.
pub(super) fn bundle_program(game: &InstalledGame, system: GameSystem) -> Option<&str> {
    match system {
        GameSystem::MacOs => game.launch_executable.as_deref(),
        GameSystem::Windows => None,
    }
}

/// A macOS build is an app bundle, whose `Contents/Info.plist` names the
/// program under `Contents/MacOS` ([`CFBundleExecutable`](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleexecutable)).
///
/// GOG asks publishers to make the bundle the installation directory
/// itself ([depots for an app bundle](https://docs.gog.com/bc-macos-depot/)),
/// so its `Contents` sits at the install root; a build that ships a
/// `<Name>.app` folder instead is found one level down, the shallowest
/// bundle winning over any nested helper app. The program gets its mode
/// here too, since nothing documents that a depot flags it as executable.
fn find_bundle_executable(install_root: &Path, relative_paths: &[String], product_id: &str) -> Option<String> {
    let Some((bundle, info_path)) = relative_paths
        .iter()
        .filter_map(|path| path.strip_suffix("Contents/Info.plist").map(|bundle| (bundle, path)))
        .filter(|&(bundle, _)| {
            bundle.is_empty()
                || bundle
                    .strip_suffix('/')
                    .is_some_and(|dir| dir.to_ascii_lowercase().ends_with(".app"))
        })
        .min_by_key(|&(bundle, _)| bundle.matches('/').count())
    else {
        return None;
    };

    let info_file = install_root.join(info_path);
    let info = match read_info_plist(&info_file) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!(path = %info_file.display(), error = %format_args!("{err:#}"), "couldn't read the app bundle's Info.plist");
            return None;
        }
    };
    let Some(name) = info
        .as_dictionary()
        .and_then(|dictionary| dictionary.get("CFBundleExecutable"))
        .and_then(plist::Value::as_string)
    else {
        tracing::warn!(path = %info_file.display(), "the app bundle's Info.plist names no CFBundleExecutable");
        return None;
    };
    // The name comes from the downloaded build and becomes a path to run:
    // a plain file name inside `Contents/MacOS`, never a way out of it.
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        tracing::warn!(
            product_id,
            name,
            "the app bundle's CFBundleExecutable is not a plain file name"
        );
        return None;
    }
    let executable = format!("{bundle}Contents/MacOS/{name}");
    if !relative_paths.contains(&executable) {
        tracing::warn!(
            product_id,
            executable,
            "the app bundle's executable isn't among the installed files"
        );
        return None;
    }
    if let Some(permissions) = platform::executable_permissions() {
        let path = install_root.join(&executable);
        if let Err(err) = std::fs::set_permissions(&path, permissions) {
            tracing::warn!(path = %path.display(), %err, "couldn't make the app bundle's executable runnable");
        }
    }
    Some(executable)
}

/// The build is downloaded, so its Info.plist is read only as a regular
/// file inside the bundle (never through a symlink to somewhere else) and
/// only up to [`INFO_PLIST_MAX_BYTES`].
fn read_info_plist(path: &Path) -> anyhow::Result<plist::Value> {
    let metadata = symlink_metadata(path).context("reading its metadata")?;
    ensure!(metadata.is_file(), "not a regular file");
    ensure!(
        metadata.len() <= INFO_PLIST_MAX_BYTES,
        "{} bytes, over the {INFO_PLIST_MAX_BYTES}-byte limit",
        metadata.len()
    );
    let mut contents = Vec::new();
    File::open(path)
        .context("opening it")?
        .take(INFO_PLIST_MAX_BYTES)
        .read_to_end(&mut contents)
        .context("reading it")?;
    plist::Value::from_reader(Cursor::new(contents)).context("parsing it")
}

/// One program a game's `goggame-<id>.info` lists: the game itself, and often a
/// configuration tool or a launcher beside it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlayTask {
    /// GOG's name for it (`Configuration tool`), as the info file has it.
    pub name: String,
    /// Relative to the install directory, with forward slashes, the shape
    /// `LaunchExecutable` records.
    pub path: String,
    /// The one GOG starts by default.
    pub is_primary: bool,
}

/// Far above any real info file, which runs to a few kilobytes.
const INFO_MAX_BYTES: u64 = 1024 * 1024;

/// The programs `goggame-<product_id>.info` at the root of `install_root`
/// lists, primary first; empty when there is none or it can't be read,
/// which is logged.
#[must_use]
#[inline]
pub fn play_tasks(install_root: &Path, product_id: &str) -> Vec<PlayTask> {
    let Some(file) = root_info_file(install_root, product_id) else {
        tracing::debug!(product_id, path = %install_root.display(), "no goggame info file, no play tasks");
        return Vec::new();
    };
    let mut tasks = read_play_tasks(install_root, &file);
    tasks.sort_by_key(|task| !task.is_primary);
    tasks
}

/// The name of `goggame-<product_id>.info` at the root of `install_root`, in whatever case the
/// depot wrote it.
fn root_info_file(install_root: &Path, product_id: &str) -> Option<String> {
    let name = format!("goggame-{product_id}.info");
    match std::fs::read_dir(install_root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .find(|file| file.to_str().is_some_and(|file| file.eq_ignore_ascii_case(&name)))
            .map(|file| file.to_string_lossy().into_owned()),
        Err(err) => {
            tracing::warn!(path = %install_root.display(), error = %err, "couldn't list the install directory for its info file");
            None
        }
    }
}

/// The Galaxy client id the game's `goggame-<product_id>.info` names, which is what its SDK signs
/// in with; `None` for a game built without Galaxy, whose info file has none. Read from the
/// install rather than from GOG's build manifest so that it is known offline.
#[must_use]
#[inline]
pub fn galaxy_client_id(install_root: &Path, product_id: &str) -> Option<ClientId> {
    let file = root_info_file(install_root, product_id)?;
    let info_file = install_root.join(file);
    let info: serde_json::Value = match read_info(&info_file)
        .and_then(|contents| serde_json::from_slice(&contents).context("parsing it"))
    {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!(path = %info_file.display(), error = %format_args!("{err:#}"), "couldn't read goggame info file");
            return None;
        }
    };
    let raw = info.get("clientId").and_then(serde_json::Value::as_str)?;
    let client_id = ClientId::parse(raw);
    if client_id.is_none() {
        tracing::warn!(path = %info_file.display(), product_id, "goggame info file names a client id that is not a GOG one, ignored");
    }
    client_id
}

/// The `FileTask`s of the info file at `info_path` (relative to
/// `install_root`), their paths made relative to the install directory.
/// `URLTask`s (a support page, a forum) launch nothing and are left out; a
/// task with no type but a path is taken as a file, since the primary task
/// was always read that way.
fn read_play_tasks(install_root: &Path, info_path: &str) -> Vec<PlayTask> {
    let info_file = install_root.join(info_path);
    let contents = match read_info(&info_file) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(path = %info_file.display(), error = %format_args!("{err:#}"), "couldn't read goggame info file");
            return Vec::new();
        }
    };
    let info: serde_json::Value = match serde_json::from_slice(&contents) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!(path = %info_file.display(), error = %err, "couldn't parse goggame info file");
            return Vec::new();
        }
    };
    let Some(play_tasks) = info.get("playTasks").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let root = Path::new(info_path).parent().unwrap_or_else(|| Path::new(""));
    play_tasks
        .iter()
        .filter(|task| task.get("type").and_then(serde_json::Value::as_str) != Some("URLTask"))
        .filter_map(|task| {
            let executable = task.get("path").and_then(serde_json::Value::as_str)?;
            // The path comes from a downloaded file and becomes a program to run: it stays
            // inside the install directory or the task is dropped.
            let Some(relative) = super::plan::manifest_relative_path(&root.join(executable).to_string_lossy()) else {
                tracing::warn!(path = %info_file.display(), executable, "goggame info file names a program outside the install, skipped");
                return None;
            };
            let path = relative.to_string_lossy().replace('\\', "/");
            let name = task
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map_or_else(|| path.clone(), |name| name.chars().take(MAX_TASK_NAME).collect());
            Some(PlayTask {
                name,
                path,
                is_primary: task.get("isPrimary").and_then(serde_json::Value::as_bool) == Some(true),
            })
        })
        .collect()
}

/// A task's name is shown in a selector, never more than a line of it.
const MAX_TASK_NAME: usize = 120;

/// Read as a regular file only, never through a symlink, and only up to
/// [`INFO_MAX_BYTES`], since it came with the download.
fn read_info(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = symlink_metadata(path).context("reading its metadata")?;
    ensure!(metadata.is_file(), "not a regular file");
    ensure!(
        metadata.len() <= INFO_MAX_BYTES,
        "{} bytes, over the {INFO_MAX_BYTES}-byte limit",
        metadata.len()
    );
    let mut contents = Vec::new();
    File::open(path)
        .context("opening it")?
        .take(INFO_MAX_BYTES)
        .read_to_end(&mut contents)
        .context("reading it")?;
    Ok(contents)
}

/// The info file GOG ships beside the game, wherever the depot put it.
fn info_file(relative_paths: &[String]) -> Option<&String> {
    relative_paths.iter().find(|path| {
        Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name.starts_with("goggame-") && name.ends_with(".info")
            })
    })
}

/// GOG ships a `goggame-<id>.info` beside the game naming its primary
/// play task. Present often enough to try first, absent often enough not
/// to rely on.
fn find_goggame_primary_executable(install_root: &Path, relative_paths: &[String]) -> Option<String> {
    let info_path = info_file(relative_paths)?;
    read_play_tasks(install_root, info_path)
        .into_iter()
        .find(|task| task.is_primary)
        .map(|task| task.path)
}

/// The program an update or a reinstall keeps launching: the one the user
/// picked among the play tasks, while the new build still lists it; else
/// the one [`find_launch_executable`] finds.
pub(super) fn kept_or_found(
    previous: Option<&str>,
    install_root: &Path,
    relative_paths: &[String],
    found: Option<String>,
) -> Option<String> {
    let Some(previous) = previous.filter(|previous| found.as_deref() != Some(*previous)) else {
        return found;
    };
    let is_listed = info_file(relative_paths).is_some_and(|info| {
        read_play_tasks(install_root, info)
            .iter()
            .any(|task| task.path == previous)
    });
    if is_listed {
        tracing::debug!(executable = previous, "keeping the program picked among the play tasks");
        Some(previous.to_owned())
    } else {
        found
    }
}

/// Falls back to guessing from the installed file list.
fn find_primary_executable_heuristic(relative_paths: &[String], install_directory: &str) -> Option<String> {
    let exes: Vec<&str> = relative_paths
        .iter()
        .filter(|path| path.to_ascii_lowercase().ends_with(".exe"))
        .map(String::as_str)
        .collect();

    // An exe named after the install directory is the game, near enough
    // always — checked across the whole tree, not just the install root:
    // UDK/UE3-era games (and plenty of others) put it several folders
    // down (`Binaries/Win32/<Name>.exe`), never at the top level at all.
    if let Some(exact) = exes.iter().find(|path| {
        Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(install_directory))
    }) {
        return Some((*exact).to_string());
    }

    fn looks_like_helper(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        [
            "crashhandler",
            "crashpad",
            "unins",
            "redist",
            "vcredist",
            "setup",
            "helper",
        ]
        .iter()
        .any(|pattern| name.contains(pattern))
    }

    // No name match: only trust a single unambiguous candidate right at
    // the install root from here on. Widening this to the whole tree too
    // would just as often surface a tool or redistributable that happens
    // to be the only exe in its own folder — nothing filters those out.
    let non_helpers: Vec<&&str> = exes
        .iter()
        .filter(|path| !path.contains('/'))
        .filter(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !looks_like_helper(name))
        })
        .collect();

    match non_helpers.as_slice() {
        [only] => Some((**only).to_string()),
        _ => None,
    }
}
