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

/// GOG ships a `goggame-<id>.info` beside the game naming its primary
/// play task. Present often enough to try first, absent often enough not
/// to rely on.
fn find_goggame_primary_executable(install_root: &Path, relative_paths: &[String]) -> Option<String> {
    let Some(info_path) = relative_paths.iter().find(|path| {
        Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name.starts_with("goggame-") && name.ends_with(".info")
            })
    }) else {
        return None;
    };

    let info_file = install_root.join(info_path);
    let contents = match std::fs::read(&info_file) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(path = %info_file.display(), error = %err, "couldn't read goggame info file");
            return None;
        }
    };
    let info: serde_json::Value = match serde_json::from_slice(&contents) {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!(path = %info_file.display(), error = %err, "couldn't parse goggame info file");
            return None;
        }
    };
    let Some(play_tasks) = info.get("playTasks").and_then(serde_json::Value::as_array) else {
        return None;
    };
    let Some(primary_task) = play_tasks
        .iter()
        .find(|task| task.get("isPrimary").and_then(serde_json::Value::as_bool) == Some(true))
    else {
        return None;
    };
    let Some(executable) = primary_task.get("path").and_then(serde_json::Value::as_str) else {
        return None;
    };

    let root = Path::new(info_path).parent().unwrap_or_else(|| Path::new(""));
    Some(root.join(executable.replace('\\', "/")).to_string_lossy().into_owned())
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
