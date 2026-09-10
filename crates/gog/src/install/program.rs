//! Guessing which installed file launches the game, once every byte has
//! landed.
//!
//! The answer is recorded in the appmanifest as a path relative to the
//! install directory, with forward slashes — the same shape the depot
//! manifest addresses files by, so it stays comparable to the file list
//! and doesn't bake in where the library happens to live.

use std::path::Path;

/// Best-effort: an install with no identifiable executable is still a
/// valid install, just not launchable until someone points at one by hand.
pub(super) fn find_launch_executable(
    install_root: &Path,
    relative_paths: &[String],
    install_directory: &str,
    product_id: &str,
) -> Option<String> {
    let found = find_goggame_primary_executable(install_root, relative_paths)
        .or_else(|| find_primary_executable_heuristic(relative_paths, install_directory));
    match &found {
        Some(executable) => tracing::debug!(product_id, executable, "found launch executable"),
        None => tracing::warn!(product_id, "couldn't determine a launch executable"),
    }
    found
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

#[cfg(test)]
mod tests;
