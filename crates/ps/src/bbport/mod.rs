//! bbport: Bloodborne v1.09, built once into its checkout and launched
//! through `run.sh` without building again.
//!
//! What `run.sh` generates and reads (`bbport.ini`, mods, patches, `out/`)
//! goes to decanter's bbport data directory rather than the checkout, so a
//! port update or reset never touches it; saves go to the profile's own
//! directory.

mod groups;
mod ini;
mod settings;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tokio::process::Command;

pub use groups::{Control, GROUPS, Group, Setting, Store};
pub use ini::{Ini, read_binds, write_binds};
pub use settings::LauncherSettings;

use crate::library::{Game, Platform};
use crate::port::Port;
use crate::toolchain::Toolchain;

const BLOODBORNE_IDS: [&str; 6] = [
    "CUSA00207",
    "CUSA00208",
    "CUSA00900",
    "CUSA01363",
    "CUSA03173",
    "CUSA03023",
];
pub const SUPPORTED_VERSION: &str = "01.09";
/// The one edition bbport runs: upstream `scripts/game_check.py` accepts only this title's 1.09
/// executable image, so other regions are listed but not playable.
pub const SUPPORTED_TITLE: &str = "CUSA03173";

pub fn is_bloodborne(game: &Game) -> bool {
    game.platform == Platform::Ps4 && BLOODBORNE_IDS.contains(&game.title_id.as_str())
}

/// A 1.09 update folder for this base game among the scanned dumps, if one exists.
pub fn find_update<'a>(game: &Game, games: &'a [Game]) -> Option<&'a Game> {
    games
        .iter()
        .find(|other| other.is_update() && other.title_id == game.title_id && other.version == SUPPORTED_VERSION)
}

/// Builds `<base>-<version>`: the base game as hard links (no extra space) with the update
/// copied over it. Update files replace links instead of writing through them, so the base
/// dump is never modified.
/// Blocking file I/O over the whole dump: off the UI thread.
pub fn merge_update(base: &Path, update: &Path, version: &str) -> io::Result<PathBuf> {
    let name = format!("{}-{version}", base.file_name().unwrap_or_default().to_string_lossy());
    let target = base.with_file_name(name);
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    fn walk(from: &Path, to: &Path, link: bool) -> io::Result<()> {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let (source, dest) = (entry.path(), to.join(entry.file_name()));
            if entry.file_type()?.is_dir() {
                walk(&source, &dest, link)?;
            } else if link {
                fs::hard_link(&source, &dest)?;
            } else {
                if dest.exists() {
                    fs::remove_file(&dest)?;
                }
                fs::copy(&source, &dest)?;
            }
        }
        Ok(())
    }
    walk(base, &target, true)?;
    walk(update, &target, false)?;
    Ok(target)
}

/// A bbport checkout. The toolchain that builds it must be the one that
/// runs it: a nix-built `bb-probe` only loads nix's Vulkan drivers.
pub struct Bbport {
    checkout: PathBuf,
}

impl Bbport {
    pub const fn new(checkout: PathBuf) -> Self {
        Self { checkout }
    }

    pub fn checkout(&self) -> &Path {
        &self.checkout
    }

    fn probe(&self) -> PathBuf {
        self.checkout.join("out/bb-probe")
    }

    /// Which toolchain the current build came from, so a launch with
    /// another one asks for a rebuild instead of failing to load drivers.
    fn build_marker(&self) -> PathBuf {
        self.checkout.join("out/decanter-toolchain")
    }

    pub fn ini_path() -> PathBuf {
        crate::paths::bbport_data().join("bbport.ini")
    }

    pub async fn build_command(&self, toolchain: &Toolchain) -> io::Result<Command> {
        toolchain.command(Port::Bbport, &self.checkout, "bash build.sh").await
    }

    /// After a build that succeeded.
    pub async fn record_build(&self, toolchain: &Toolchain) -> io::Result<()> {
        tokio::fs::write(self.build_marker(), toolchain.id()).await
    }

    /// Whether `toolchain` can launch what is built, without building.
    pub async fn is_built_with(&self, toolchain: &Toolchain) -> bool {
        let probe = tokio::fs::metadata(self.probe()).await.is_ok_and(|meta| meta.is_file());
        let marker = tokio::fs::read_to_string(self.build_marker()).await;
        probe && marker.is_ok_and(|id| id == toolchain.id())
    }

    /// The same environment bbport's own launcher built (`game_environment`
    /// in `launcher/bbport_launcher.py`), for the settings decanter knows,
    /// plus what keeps `run.sh` from building and from writing into the
    /// checkout. `user_dir` holds the profile's saves.
    pub async fn launch_command(
        &self,
        toolchain: &Toolchain,
        game_dir: &Path,
        user_dir: &Path,
        settings: &LauncherSettings,
    ) -> io::Result<Command> {
        let mut command = match toolchain.command(Port::Bbport, &self.checkout, "bash run.sh").await {
            Ok(command) => command,
            Err(err) => return Err(err),
        };
        let data = crate::paths::bbport_data();
        let path_or = |key: &str, fallback: PathBuf| settings.text(key).map_or(fallback, PathBuf::from);
        command
            .env("BB_PREBUILT", "1")
            .env("BB_PROBE", self.probe())
            .env("BB_DATA_DIR", &data)
            .env("BB_USER_DIR", user_dir)
            .env("BB_GAME_DIR", game_dir)
            .env("BB_MODS_DIR", path_or("mods_dir", data.join("mods")))
            .env("BB_MODS_CONFIG", data.join("mods.json"))
            .env(
                "BB_MODS_ENABLED",
                if settings.flag("mods_enabled", true) { "1" } else { "0" },
            )
            .env("BB_PATCHES_DIR", path_or("patches_dir", data.join("patches")))
            .env("BB_PATCHES_CONFIG", data.join("patches.json"))
            .env("BB_LANGUAGE", settings.text("language").unwrap_or("1"))
            .env(
                "BB_FULLSCREEN",
                if settings.flag("fullscreen", false) { "1" } else { "0" },
            )
            .env("BB_PRESENT_MODE", settings.text("present_mode").unwrap_or("Mailbox"))
            .env("BB_FPS", settings.text("fps_mode").unwrap_or("uncap"))
            // The in-game menu has no Spanish; English until one is picked in the menu itself.
            .env("BB_MENU_LANGUAGE", "en")
            // Experimental memory model, AMD GPUs only upstream; off unless chosen.
            .env("BB_PC_MODEL", if settings.flag("pc_model", false) { "1" } else { "0" });
        if settings.flag("hdr", false) {
            command.env("BB_HDR", "1");
        }
        if let Some(limit) = settings.number("fps_limit").filter(|&limit| limit > 0.0) {
            command.env("BB_FPS_LIMIT", format!("{limit:.0}"));
        }
        for (key, env) in [
            ("draw_pipe", "BB_DRAW_PIPE"),
            ("readbacks", "BB_READBACKS"),
            ("preupload", "BB_PREUPLOAD"),
            ("gamepad", "BB_GAMEPAD"),
        ] {
            if let Some(value) = settings.text(key) {
                command.env(env, value);
            }
        }
        for (key, env) in [
            ("mangohud", "MANGOHUD"),
            ("frame_stats", "BB_FRAME_STATS"),
            ("save_log", "BB_SAVE_LOG"),
            ("as_0_3", "BB_AS_0_3"),
            ("gpu_profile", "BB_GPU_PROFILE"),
            ("vk_validation", "BB_VK_VALIDATION"),
        ] {
            if settings.flag(key, false) {
                command.env(env, "1");
            }
        }
        let fsr411 = crate::paths::fsr411();
        if tokio::fs::metadata(&fsr411).await.is_ok_and(|meta| meta.is_dir()) {
            command.env("BB_FSR411_DIR", fsr411);
        }
        if settings.flag("crash_diag", false) {
            for env in ["BB_FREE_CHECK", "BB_WRITE_LOG", "BB_PRODUCER_CHECK"] {
                command.env(env, "1");
            }
        }
        for item in settings.text("extra_env").unwrap_or_default().split_whitespace() {
            if let Some((key, value)) = item.split_once('=') {
                command.env(key, value);
            }
        }
        Ok(command)
    }

    /// Connected controllers as `(GUID, name)`, from the tool bbport builds
    /// for this. Empty before the first build.
    pub async fn connected_gamepads(&self, toolchain: &Toolchain) -> Vec<(String, String)> {
        let command = toolchain
            .command(Port::Bbport, &self.checkout, "out/bb-gpu-capabilities --gamepads")
            .await;
        let Ok(mut command) = command else {
            return Vec::new();
        };
        command
            .output()
            .await
            .ok()
            .filter(|out| out.status.success())
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .map(|(guid, name)| (guid.to_owned(), name.to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_links_base_and_copies_update() {
        let root = std::env::temp_dir().join(format!("decanter-ps-merge-{}", std::process::id()));
        let (base, update) = (root.join("CUSA00900"), root.join("CUSA00900-patch"));
        fs::create_dir_all(base.join("sce_sys")).unwrap();
        fs::create_dir_all(update.join("sce_sys")).unwrap();
        fs::write(base.join("eboot.bin"), "base").unwrap();
        fs::write(base.join("sce_sys/keep"), "base").unwrap();
        fs::write(update.join("eboot.bin"), "update").unwrap();
        let merged = merge_update(&base, &update, "1.09").unwrap();
        let read = |p: &Path| fs::read_to_string(p).unwrap();
        assert_eq!(read(&merged.join("eboot.bin")), "update");
        assert_eq!(read(&merged.join("sce_sys/keep")), "base");
        assert_eq!(read(&base.join("eboot.bin")), "base");
        assert!(merge_update(&base, &update, "1.09").is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
