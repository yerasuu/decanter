//! A psport build unpacked in a folder the user picked, its `psport.exe` started directly: no
//! checkout, toolchain, bash or `run.sh`, so decanter does here what `run.sh` does around the
//! program (its defaults and its arguments).

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf, Prefix};
use std::{fs, io};

use tokio::process::Command;

use super::local_player::LocalPlayer;

/// Whether this system runs psport this way rather than through `run.sh`.
pub const IN_USE: bool = crate::os::DIRECT_LAUNCH;

/// The [`super::LauncherSettings`] key the folder is kept under, as the user picked it.
pub const FOLDER_KEY: &str = "psport_exe_dir";
pub const EXECUTABLE: &str = "psport.exe";
/// What `psport.exe` loads from its own folder before it can draw anything.
pub const GPU_LIBRARY: &str = "libpsport-gpu.dll";

/// A folder on a local drive that held [`EXECUTABLE`] and [`GPU_LIBRARY`], as files of its own,
/// when it was checked.
#[derive(Debug, Clone)]
pub struct Folder(PathBuf);

#[derive(Debug, thiserror::Error)]
pub enum FolderError {
    #[error("{file} in {} is a link, not the file itself", dir.display())]
    Linked { dir: PathBuf, file: &'static str },
    #[error("{} holds no {file}", dir.display())]
    Missing { dir: PathBuf, file: &'static str },
    #[error("{} is not a folder: {err}", dir.display())]
    NotAFolder { dir: PathBuf, err: io::Error },
    #[error("{} is not on a local drive", .0.display())]
    NotLocal(PathBuf),
    #[error("{} is not an absolute path without `..`", .0.display())]
    Relative(PathBuf),
}

impl Folder {
    /// Blocking file I/O: off the UI thread.
    ///
    /// # Errors
    /// When `dir` is relative or goes through `..`, isn't a folder, is (or leads through a link
    /// or junction to) anything but a local drive, or lacks either file psport needs to start.
    pub fn try_new(dir: PathBuf) -> Result<Self, FolderError> {
        if !dir.is_absolute() || dir.components().any(|part| part == Component::ParentDir) {
            return Err(FolderError::Relative(dir));
        }
        if !is_on_local_drive(&dir) {
            return Err(FolderError::NotLocal(dir));
        }
        match fs::metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(FolderError::NotAFolder {
                    dir,
                    err: io::Error::from(io::ErrorKind::NotADirectory),
                });
            }
            Err(err) => return Err(FolderError::NotAFolder { dir, err }),
        }
        // Kept as picked, because on Windows the canonical form is a `\\?\` path, which takes no `/`
        // separators; canonicalized only to tell where links, junctions and mapped drives lead.
        match fs::canonicalize(&dir) {
            Ok(real) if is_on_local_drive(&real) => {}
            Ok(_) => return Err(FolderError::NotLocal(dir)),
            Err(err) => return Err(FolderError::NotAFolder { dir, err }),
        }
        for file in [EXECUTABLE, GPU_LIBRARY] {
            match fs::symlink_metadata(dir.join(file)) {
                Ok(meta) if meta.is_file() => {}
                Ok(meta) if meta.is_symlink() => return Err(FolderError::Linked { dir, file }),
                Ok(_) | Err(_) => return Err(FolderError::Missing { dir, file }),
            }
        }
        Ok(Self(dir))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn executable(&self) -> PathBuf {
        self.0.join(EXECUTABLE)
    }
}

/// Whether every prefix of `path` names a drive letter: no network share and no device.
fn is_on_local_drive(path: &Path) -> bool {
    path.components().all(|part| match part {
        Component::Prefix(prefix) => matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
        Component::RootDir | Component::CurDir | Component::ParentDir | Component::Normal(_) => true,
    })
}

/// What `run.sh` sets before the program unless the environment `command` will run with already
/// has it (an empty variable counts as unset): the settings file in `config` (the program alone
/// reads `psport.ini` in its working folder), the GPU library's user folder, which it reads as it
/// loads before `--user` arrives. Called once the launch's
/// own environment is set, since the user folder follows its final `PS_USER_DIR`.
pub fn run_sh_defaults(command: &mut Command, config: &Path, data: &Path) {
    let user_dir = env_of(command, "PS_USER_DIR").unwrap_or_else(|| data.join("user").into_os_string());
    let defaults = [
        ("PS_CONFIG", config.as_os_str()),
        ("PS_GPU_USER_DIR", user_dir.as_os_str()),
    ];
    for (name, value) in defaults {
        if env_of(command, name).is_none() {
            command.env(name, value);
        }
    }
}

/// The arguments `run.sh` passes the program, read from the environment `command` will run with
/// as `run.sh` reads its own: an empty variable counts as unset. `data` is `PS_DATA_DIR` and `out`
/// the folder the port links a game's mods in. `local_player` adds `--user-id` and its digits as two
/// arguments, which `run.sh` takes through its own.
#[must_use]
pub fn arguments(command: &Command, data: &Path, out: &Path, local_player: Option<&LocalPlayer>) -> Vec<OsString> {
    let var = |name: &str| env_of(command, name);
    let path = |name: &str| var(name).map(not_an_option);
    let var_or = |name: &str, fallback: PathBuf| path(name).unwrap_or_else(|| fallback.into_os_string());
    let game = path("PS_GAME_DIR").unwrap_or_default();
    let mut args = Vec::with_capacity(24);
    args.extend([OsString::from("--game"), game.clone()]);
    if let Some(modules) = path("PS_SYS_MODULES") {
        args.extend([
            OsString::from("--system-modules"),
            modules,
            OsString::from("--hle"),
            var("PS_HLE").unwrap_or_default(),
            OsString::from("--lle"),
            var("PS_LLE").unwrap_or_default(),
        ]);
    }
    if var("PS_MODS_ENABLED").is_none_or(|enabled| enabled != "0") {
        args.extend([
            OsString::from("--mods"),
            var_or("PS_MODS_DIR", data.join("mods")),
            OsString::from("--mods-config"),
            var_or("PS_MODS_CONFIG", data.join("mods.json")),
            OsString::from("--out"),
            out.as_os_str().to_owned(),
        ]);
    }
    args.extend([
        OsString::from("--patches-dir"),
        var_or("PS_PATCHES_DIR", data.join("patches")),
        OsString::from("--patches-config"),
        var_or("PS_PATCHES_CONFIG", data.join("patches.json")),
        OsString::from("--app0"),
        game,
        OsString::from("--user"),
        var_or("PS_USER_DIR", data.join("user")),
        OsString::from("--timeout"),
        var("PS_TIMEOUT").unwrap_or_else(|| OsString::from("0")),
    ]);
    if let Some(player) = local_player {
        args.extend([OsString::from("--user-id"), OsString::from(player.as_str())]);
    }
    args
}

/// `path` as given, unless it starts with `-`, which psport could read as one of its options:
/// then the same relative path from `.`.
fn not_an_option(path: OsString) -> OsString {
    if path.as_encoded_bytes().starts_with(b"-") {
        Path::new(".").join(path).into_os_string()
    } else {
        path
    }
}

/// `name` as `command` will see it: set or removed on it, else inherited; empty reads as unset.
fn env_of(command: &Command, name: &str) -> Option<OsString> {
    let set = command
        .as_std()
        .get_envs()
        .find(|&(key, _)| key == name)
        .map(|(_, value)| value.map(ToOwned::to_owned));
    set.unwrap_or_else(|| std::env::var_os(name))
        .filter(|value| !value.is_empty())
}
