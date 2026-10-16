//! Host tools a game can run with — MangoHud's overlay, Feral's GameMode —
//! turned on per game or for every game of a library.
//!
//! Neither is bundled: what the host installed is what a game gets, so each
//! is looked for where the host's own loader or bus finds it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One tool a game can be started with. Its [`GameTool::key`] names both
/// the library-wide default in `settings` and the per-game rows in
/// `game_options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GameTool {
    MangoHud,
    GameMode,
}

impl GameTool {
    #[must_use]
    #[inline]
    pub const fn key(self) -> &'static str {
        match self {
            Self::MangoHud => "mangohud",
            Self::GameMode => "gamemode",
        }
    }
}

/// Whether MangoHud's Vulkan layer is installed where the Vulkan loader
/// looks for implicit layers. The layer, not the `mangohud` wrapper, is
/// what a launch relies on: `MANGOHUD=1` is the layer's
/// `enable_environment`, which reaches a Proton game because
/// pressure-vessel imports the host's implicit layers into its container,
/// while the wrapper's `LD_PRELOAD` is cleared by umu inside a gamescope
/// session and can't be resolved by a nix or bundled psport's own loader.
#[must_use]
#[inline]
pub fn mangohud_installed() -> bool {
    cfg!(target_os = "linux")
        && implicit_layer_dirs().any(|dir| {
            std::fs::read_dir(dir).is_ok_and(|entries| {
                entries.flatten().any(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.get(..8)
                        .is_some_and(|start| start.eq_ignore_ascii_case("mangohud"))
                        && name.ends_with(".json")
                })
            })
        })
}

/// The Vulkan loader's implicit layer search on Linux
/// (`docs/LoaderLayerInterface.md`, "Linux Layer Discovery"): the override
/// variables first, then the XDG config and data directories with their
/// defaults, and `/etc`.
fn implicit_layer_dirs() -> impl Iterator<Item = PathBuf> {
    let overrides = ["VK_IMPLICIT_LAYER_PATH", "VK_ADD_IMPLICIT_LAYER_PATH"]
        .into_iter()
        .flat_map(|var| path_list(var, ""))
        .filter(|dir| !dir.as_os_str().is_empty());
    let config = dirs::config_dir()
        .into_iter()
        .chain(path_list("XDG_CONFIG_DIRS", "/etc/xdg"))
        .chain(std::iter::once(PathBuf::from("/etc")));
    let data = dirs::data_dir()
        .into_iter()
        .chain(path_list("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    overrides.chain(
        config
            .chain(data)
            .map(|dir| dir.join("vulkan").join("implicit_layer.d")),
    )
}

/// A `:`-separated list of directories from `var`, or `default` when it is
/// unset or empty, as XDG's lists are read.
fn path_list(var: &str, default: &str) -> Vec<PathBuf> {
    let value = std::env::var_os(var)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from(default));
    std::env::split_paths(&value).collect()
}

/// What turns MangoHud on for a launch: the layer's enable variable, and a
/// `MANGOHUD_CONFIG` that sends its logs to `log_dir` (`autostart_log` starts
/// one a second in, otherwise they wait for MangoHud's own toggle key).
///
/// `read_cfg` comes first because MangoHud ignores its config file
/// whenever `MANGOHUD_CONFIG` is set without it; a `MANGOHUD_CONFIG` decanter
/// was started with goes last, so what the user set by hand still wins.
/// `log_dir` is left out when its path holds a character MangoHud's option
/// parser splits on, rather than handing it a path cut short.
#[must_use]
#[inline]
pub fn mangohud_env(log_dir: &Path, autostart_log: bool) -> [(&'static str, String); 2] {
    let mut config = String::from("read_cfg");
    let folder = log_dir.to_string_lossy();
    if folder.contains([',', ':', ';', '=', '\n']) {
        tracing::warn!(
            log_dir = %log_dir.display(),
            "MangoHud logs stay in its default folder: the library path holds a separator it splits on"
        );
    } else {
        config.push_str(",output_folder=");
        config.push_str(&folder);
        if autostart_log {
            config.push_str(",autostart_log=1");
        }
    }
    if let Some(inherited) = std::env::var("MANGOHUD_CONFIG").ok().filter(|value| !value.is_empty()) {
        config.push(',');
        config.push_str(&inherited);
    }
    [("MANGOHUD", "1".to_owned()), ("MANGOHUD_CONFIG", config)]
}

/// Where MangoHud's logs for a library's games go.
#[must_use]
#[inline]
pub fn mangohud_log_dir(library_root: &Path) -> PathBuf {
    crate::bootstrap::library_data_dir(library_root)
        .join("logs")
        .join("mangohud")
}

/// The D-Bus service `gamemoded` is started through on the session bus.
const GAMEMODE_SERVICE: &str = "com.feralinteractive.GameMode.service";

/// `gamemoded` on `PATH`, when the session bus can also start it (its
/// D-Bus service file is installed): without the service the request a
/// launch makes would find nobody to answer it.
#[must_use]
#[inline]
pub fn gamemoded() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let has_service = dirs::data_dir()
        .into_iter()
        .chain(path_list("XDG_DATA_DIRS", "/usr/local/share:/usr/share"))
        .any(|dir| dir.join("dbus-1").join("services").join(GAMEMODE_SERVICE).is_file());
    if !has_service {
        return None;
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("gamemoded"))
            .find(|candidate| candidate.is_file())
    })
}

/// What goes before a game's command so it runs under GameMode: a shell
/// that registers its own pid with `gamemoded -r<pid>` and then `exec`s the
/// command, which keeps that pid. Everything the game starts inherits the
/// niceness and I/O priority GameMode gives it, and `gamemoded` drops the
/// registration once the pid is gone.
///
/// Not `gamemoderun`: its `LD_PRELOAD` of `libgamemodeauto.so.0` is cleared
/// by umu inside a gamescope session, and a nix or bundled psport's own
/// loader can't find the host's library by that name. A failed request is
/// printed into the run's log and the game starts anyway.
#[must_use]
#[inline]
pub fn gamemode_prefix(gamemoded: &Path) -> [OsString; 5] {
    [
        "sh".into(),
        "-c".into(),
        r#""$1" -r"$$"; shift; exec "$@""#.into(),
        "decanter-gamemode".into(),
        gamemoded.into(),
    ]
}
