//! CMake build directories inside a port checkout that the checkout moved
//! away from.
//!
//! A `CMakeCache.txt` records the absolute directory it was created in, and
//! CMake refuses to configure from anywhere else. The PS folder follows the
//! library, so a checkout built before a move would never build again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How deep under the checkout build directories are looked for: bbport's
/// `out/gpu`, AnyPS5's `build/` and its `build/*-build` dependencies.
const MAX_DEPTH: usize = 3;
const CACHE_DIR_KEY: &str = "CMAKE_CACHEFILE_DIR:INTERNAL=";

/// Removes each cache (and its `CMakeFiles/`) that names another directory
/// than its own, so the next configure starts fresh; sources stay, compiled
/// objects go with `CMakeFiles/`. Returns the build directories it reset. File
/// I/O over the checkout's top levels: off the UI thread.
pub fn forget_moved(checkout: &Path) -> io::Result<Vec<PathBuf>> {
    let mut reset = Vec::new();
    walk(checkout, 0, &mut reset)?;
    Ok(reset)
}

fn walk(dir: &Path, depth: usize, reset: &mut Vec<PathBuf>) -> io::Result<()> {
    let cache = dir.join("CMakeCache.txt");
    if let Ok(text) = fs::read_to_string(&cache)
        && let Some(recorded) = text.lines().find_map(|line| line.strip_prefix(CACHE_DIR_KEY))
        && Path::new(recorded) != dir
        && !matches!(
            (fs::canonicalize(recorded), fs::canonicalize(dir)),
            (Ok(physical), Ok(here)) if physical == here
        )
    {
        fs::remove_file(&cache)?;
        match fs::remove_dir_all(dir.join("CMakeFiles")) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        reset.push(dir.to_owned());
    }
    if depth == MAX_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if depth > 0 && matches!(err.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound) => {
            tracing::warn!(dir = %dir.display(), %err, "CMake cache scan skipped a folder");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let is_dir = match entry.file_type() {
            Ok(kind) => kind.is_dir(),
            Err(err) if matches!(err.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound) => false,
            Err(err) => return Err(err),
        };
        // Submodules and vendored code carry no build directory of ours, and
        // `.git` is large.
        let name = entry.file_name();
        if is_dir && !matches!(name.to_str(), Some(".git" | "third_party" | "3rdparty")) {
            walk(&entry.path(), depth + 1, reset)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cache_from_another_directory_is_reset() {
        let root = std::env::temp_dir().join(format!("decanter-ps-cmake-{}", std::process::id()));
        let (moved, current) = (root.join("out/gpu"), root.join("build"));
        fs::create_dir_all(moved.join("CMakeFiles")).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(
            moved.join("CMakeCache.txt"),
            format!("{CACHE_DIR_KEY}/old/place/out/gpu\n"),
        )
        .unwrap();
        fs::write(moved.join("bbgpu.o"), "").unwrap();
        fs::write(
            current.join("CMakeCache.txt"),
            format!("{CACHE_DIR_KEY}{}\n", current.display()),
        )
        .unwrap();

        assert_eq!(forget_moved(&root).unwrap(), vec![moved.clone()]);
        assert!(!moved.join("CMakeCache.txt").exists() && !moved.join("CMakeFiles").exists());
        assert!(moved.join("bbgpu.o").exists());
        assert!(current.join("CMakeCache.txt").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
