//! bbport: Bloodborne v1.09, built once into its checkout and launched
//! through `run.sh` without building again.
//!
//! What `run.sh` generates and reads (`bbport.ini`, mods, patches, `out/`)
//! goes to decanter's bbport data directory rather than the checkout, so a
//! port update or reset never touches it; saves go to the profile's own
//! directory.

pub mod direct;
pub mod effects;
mod features;
mod groups;
pub mod icons;
mod ini;
pub mod local_player;
pub mod mods;
mod names;
pub mod online;
pub mod pad;
mod settings;
pub mod tools;
pub mod values;

use core::iter;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use decanter_nix::Toolchain;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

pub use features::{HostFeatures, host_features};
pub use groups::{Control, GROUPS, Group, OPTION_FEATURES, Setting, Store};
pub use ini::{Ini, read_binds, write_binds};
pub use names::{MEMORY_MODEL_SETTINGS, Names, single_memory_model_blocking};
pub use settings::LauncherSettings;

use crate::learned;
use crate::library::{Game, Platform};
use crate::os::linux::toolchain;
use crate::paths::Root;
use crate::port::Port;
use names::single_memory_model;

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

/// The `aspect_ratio` value nearest a `width` by `height` display, either way up: what the
/// setting's automatic choice passes, since psport keeps 16:9 unless told otherwise.
#[must_use]
#[expect(clippy::float_arithmetic, reason = "comparing the display's ratio")]
pub fn nearest_aspect_ratio(width: f32, height: f32) -> Option<&'static str> {
    const RATIOS: [(&str, f32); 5] = [
        ("4:3", 4.0 / 3.0),
        ("16:10", 16.0 / 10.0),
        ("16:9", 16.0 / 9.0),
        ("21:9", 21.0 / 9.0),
        ("32:9", 32.0 / 9.0),
    ];
    let (long, short) = if width >= height {
        (width, height)
    } else {
        (height, width)
    };
    if short <= 0.0 {
        return None;
    }
    let ratio = long / short;
    RATIOS
        .into_iter()
        .min_by(|a, b| (a.1 - ratio).abs().total_cmp(&(b.1 - ratio).abs()))
        .map(|(name, _)| name)
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

/// One game's launch, as both [`Bbport::launch_command`] and [`Bbport::direct_command`] take it.
/// `user_dir` holds the profile's saves; `mods` comes from [`mods::prepare`], or [`mods::Mods::off`]
/// for a title other than Bloodborne. `menu_language` is the in-game menu's until one is picked in
/// the menu itself. `learned` is what earlier runs of this title taught ([`learned::Store`]).
/// `aspect_ratio` is the display's, when the setting is automatic ([`nearest_aspect_ratio`]).
/// `cache_dir` is the game's own cache folder, in the location of the library folder holding its
/// dump; without one the port keeps its caches in `user_dir`. `debug_dir` turns the port's debug
/// mode on, each run's logs gathered there under the port's own folder per host. `online` is online
/// play, with its secrets, when it is on and the build has it.
#[derive(Clone, Copy)]
pub struct GameLaunch<'launch> {
    pub game: &'launch Game,
    pub user_dir: &'launch Path,
    pub cache_dir: Option<&'launch Path>,
    pub debug_dir: Option<&'launch Path>,
    pub settings: &'launch LauncherSettings,
    pub mods: &'launch mods::Mods,
    pub menu_language: &'launch str,
    pub learned: &'launch learned::Title,
    pub aspect_ratio: Option<&'launch str>,
    pub online: Option<&'launch online::Online>,
}

/// A bbport checkout. The toolchain that builds it must be the one that
/// runs it: a nix-built probe only loads nix's Vulkan drivers.
pub struct Bbport {
    root: Root,
    checkout: PathBuf,
}

impl Bbport {
    pub fn new(root: Root) -> Self {
        Self {
            checkout: Port::Bbport.checkout(&root),
            root,
        }
    }

    pub fn checkout(&self) -> &Path {
        &self.checkout
    }

    /// Which toolchain and commit the current build came from, so a launch
    /// with another toolchain, or after the checkout moved, builds again
    /// instead of failing to load drivers or running the old code. A second
    /// line names the store path of a build fetched from the port's binary
    /// cache, or the tag of a release bundle ([`crate::os::linux::bundle`]), instead.
    fn build_marker(&self) -> PathBuf {
        self.checkout.join("out/decanter-toolchain")
    }

    async fn fetched(&self) -> Option<String> {
        let Ok(marker) = tokio::fs::read_to_string(self.build_marker()).await else {
            return None;
        };
        marker.lines().nth(1).map(str::to_owned)
    }

    /// The fetched build the marker names, when the current one came from the
    /// binary cache.
    pub async fn prebuilt(&self) -> Option<PathBuf> {
        self.fetched()
            .await
            .filter(|path| crate::os::linux::prebuilt::valid_store_path(path))
            .map(PathBuf::from)
    }

    /// The release whose bundle the marker names, when the current build is one.
    pub async fn bundle_tag(&self) -> Option<String> {
        self.fetched()
            .await
            .filter(|tag| crate::os::linux::prebuilt::valid_tag(tag))
    }

    /// The current build's marker, which changes whenever the port is built or fetched again:
    /// what a cache of the port's answers is keyed by.
    pub async fn build_id(&self) -> Option<String> {
        tokio::fs::read_to_string(self.build_marker()).await.ok()
    }

    /// What `run.sh` and the port's tools run from: the release bundle or the
    /// fetched build, else the checkout built in place.
    async fn port_dir(&self) -> PathBuf {
        if let Some(tag) = self.bundle_tag().await {
            return crate::os::linux::bundle::dir(&self.root, Port::Bbport, &tag);
        }
        self.prebuilt().await.unwrap_or_else(|| self.checkout.clone())
    }

    /// [`Self::port_dir`] as the host sees it, where `toolchain` may keep its store elsewhere.
    async fn host_port_dir(&self, toolchain: &Toolchain) -> PathBuf {
        if let Some(tag) = self.bundle_tag().await {
            return crate::os::linux::bundle::dir(&self.root, Port::Bbport, &tag);
        }
        self.prebuilt().await.map_or_else(
            || self.checkout.clone(),
            |store_path| toolchain.store_path_on_host(&self.root.toolchain(), &store_path),
        )
    }

    /// Spelled as the checkout's port reads it. A port that reads `psport.ini` gets a copy of the
    /// `bbport.ini` an older one left, so the first edit here doesn't start from its defaults; the
    /// port makes the same copy at launch, and an older tag keeps reading `bbport.ini`.
    pub fn ini_path(root: &Root) -> PathBuf {
        let names = Names::of_blocking(&Port::Bbport.checkout(root));
        let path = root.bbport_data().join(names.ini());
        let older = root.bbport_data().join(Names::EarlyPsport.ini());
        if matches!(names, Names::Psport) && !path.exists() && older.exists() {
            match fs::copy(&older, &path) {
                Ok(_) => tracing::info!(from = %older.display(), to = %path.display(), "port settings copied"),
                Err(err) => tracing::warn!(from = %older.display(), %err, "port settings not copied"),
            }
        }
        path
    }

    /// Drops the in-game menu's language, so the next run follows
    /// `PS_MENU_LANGUAGE` again: the menu writes `menu_language` whenever it
    /// saves any setting, which would otherwise keep it in the language that
    /// was current then. File I/O: off the UI thread.
    pub fn forget_menu_language(root: &Root) -> io::Result<()> {
        let path = Self::ini_path(root);
        let mut ini = Ini::load(&path);
        if ini.get("menu_language").is_none() {
            return Ok(());
        }
        ini.remove("menu_language");
        ini.save(&path)
    }

    pub async fn build_command(&self, toolchain: &Toolchain) -> io::Result<Command> {
        toolchain::command(toolchain, &self.root, Port::Bbport, "bash build.sh").await
    }

    /// After a build that succeeded.
    pub async fn record_build(&self, toolchain: &Toolchain) -> io::Result<()> {
        tokio::fs::write(
            self.build_marker(),
            toolchain::build_stamp(toolchain, &self.checkout).await,
        )
        .await
    }

    /// After `store_path`, the checkout's commit as the binary cache holds it,
    /// was fetched into `toolchain`'s store.
    pub async fn record_prebuilt(&self, toolchain: &Toolchain, store_path: &str) -> io::Result<()> {
        if let Err(err) = tokio::fs::create_dir_all(self.checkout.join("out")).await {
            return Err(err);
        }
        let stamp = toolchain::build_stamp(toolchain, &self.checkout).await;
        tokio::fs::write(self.build_marker(), format!("{stamp}\n{store_path}")).await
    }

    /// After `tag`'s bundle, the release the checkout is on, was installed.
    pub async fn record_bundle(&self, tag: &str) -> io::Result<()> {
        if let Err(err) = tokio::fs::create_dir_all(self.checkout.join("out")).await {
            return Err(err);
        }
        let stamp = toolchain::build_stamp(&Toolchain::Bundle, &self.checkout).await;
        tokio::fs::write(self.build_marker(), format!("{stamp}\n{tag}")).await
    }

    /// Whether `toolchain` can launch what is built, without building.
    pub async fn is_built_with(&self, toolchain: &Toolchain) -> bool {
        let port_dir = self.host_port_dir(toolchain).await;
        let names = Names::of(&port_dir).await;
        let probe = tokio::fs::metadata(port_dir.join(names.probe()))
            .await
            .is_ok_and(|meta| meta.is_file());
        let marker = tokio::fs::read_to_string(self.build_marker()).await;
        let stamp = toolchain::build_stamp(toolchain, &self.checkout).await;
        probe && marker.is_ok_and(|built| built.lines().next() == Some(stamp.as_str()))
    }

    /// `run.sh` in `toolchain`, with [`Self::game_env`] plus what keeps it from building and from
    /// writing into the checkout.
    pub async fn launch_command(&self, toolchain: &Toolchain, launch: &GameLaunch<'_>) -> io::Result<Command> {
        let mut command = match toolchain::command(
            toolchain,
            &self.root,
            Port::Bbport,
            r#"bash "$DECANTER_PORT_DIR/run.sh""#,
        )
        .await
        {
            Ok(command) => command,
            Err(err) => return Err(err),
        };
        let port_dir = self.port_dir().await;
        let host_port_dir = self.host_port_dir(toolchain).await;
        let names = Names::of(&host_port_dir).await;
        let old_memory_model = !single_memory_model(&host_port_dir).await;
        command
            .env(names.var("PREBUILT"), "1")
            .env(names.var("PROBE"), port_dir.join(names.probe()))
            .env("DECANTER_PORT_DIR", port_dir);
        self.game_env(&mut command, names, old_memory_model, launch).await;
        Ok(command)
    }

    /// `folder`'s `psport.exe` started as `command` with no `run.sh` in between: the environment
    /// of [`Self::game_env`], the defaults `run.sh` would add to it, and the arguments `run.sh`
    /// builds from all of it ([`direct::arguments`]).
    ///
    /// # Errors
    /// Creating the folder the port links the game's mods in, or the game's cache folder.
    pub async fn direct_command(
        &self,
        command: &mut Command,
        folder: &direct::Folder,
        launch: &GameLaunch<'_>,
    ) -> io::Result<()> {
        let names = Names::Psport;
        let data = self.root.bbport_data();
        let out = data.join("out");
        for dir in iter::once(out.as_path()).chain(launch.cache_dir) {
            if let Err(err) = tokio::fs::create_dir_all(dir).await {
                return Err(err);
            }
        }
        // `run.sh` names the program it runs here; `psport.exe` ignores it, while the run's log
        // tells the port's variable spelling by it.
        command
            .env(names.var("PREBUILT"), "1")
            .env(names.var("PROBE"), folder.executable());
        self.game_env(command, names, false, launch).await;
        direct::run_sh_defaults(command, &data.join(names.ini()), &data);
        command.args(direct::arguments(command, &data, &out));
        Ok(())
    }

    /// The same environment bbport's own launcher built (`game_environment` in
    /// `launcher/psport_launcher.py`), for the settings decanter knows; `names` and
    /// `old_memory_model` are those of the port that reads it.
    async fn game_env(&self, command: &mut Command, names: Names, old_memory_model: bool, launch: &GameLaunch<'_>) {
        let GameLaunch {
            game,
            user_dir,
            cache_dir,
            debug_dir,
            settings,
            mods,
            menu_language,
            learned,
            aspect_ratio,
            online,
        } = *launch;
        let var = |suffix: &str| names.var(suffix);
        let port_reads = |key: &str| old_memory_model || !MEMORY_MODEL_SETTINGS.contains(&key);
        let data = self.root.bbport_data();
        let path_or = |key: &str, fallback: PathBuf| settings.text(key).map_or(fallback, PathBuf::from);
        command
            .env(var("DATA_DIR"), &data)
            .env(var("USER_DIR"), user_dir)
            .env(var("GAME_DIR"), &game.dir)
            .env(var("MODS_DIR"), &mods.dir)
            .env(var("MODS_CONFIG"), &mods.config)
            .env(var("MODS_ENABLED"), if mods.enabled { "1" } else { "0" })
            .env(var("PATCHES_DIR"), path_or("patches_dir", data.join("patches")))
            .env(var("PATCHES_CONFIG"), data.join("patches.json"))
            .env(var("LANGUAGE"), settings.text("language").unwrap_or("1"))
            .env(
                var("FULLSCREEN"),
                if settings.flag("fullscreen", false) { "1" } else { "0" },
            )
            .env(var("PRESENT_MODE"), settings.text("present_mode").unwrap_or("Mailbox"))
            .env(var("FPS"), settings.text("fps_mode").unwrap_or("uncap"))
            .env(var("MENU_LANGUAGE"), menu_language)
            // SDL reads its hints from the environment too: the window's Wayland app id and X11
            // class, otherwise the probe's name for every title, which the task bar matches the
            // game's desktop entry against.
            .env("SDL_APP_ID", decanter_core::application_id(&game.key()));
        // The port picks the memory model itself (the PC model on AMD GPUs); the setting only forces
        // the PC model on other GPUs, so off must not pass PC_MODEL=0 and hold AMD on the old one.
        command.envs(cache_dir.map(|dir| (var("CACHE_DIR"), dir)));
        command.envs(debug_dir.into_iter().flat_map(|dir| debug_env(names, dir)));
        if port_reads("pc_model") && settings.flag("pc_model", false) {
            command.env(var("PC_MODEL"), "1");
        }
        // The fork's way past `game_check.py`, which accepts only Bloodborne 1.09, and past the
        // first import the runtime lacks, so the log lists every one the title reaches.
        if !is_bloodborne(game) {
            command.env(var("SKIP_GAME_CHECK"), "1").env(var("PERMISSIVE"), "1");
            // The user's own dump of the console's modules (Ajustes), which the fork links ahead of
            // its own versions; Bloodborne keeps the path it is known to work on.
            if let Some(dir) = settings.text("sys_modules_dir") {
                command.env(var("SYS_MODULES"), dir);
            }
        }
        // Ports from before `psport.ini` have no aspect ratio setting to override.
        command.envs(
            aspect_ratio
                .filter(|_| matches!(names, Names::Psport))
                .map(|ratio| (var("ASPECT_RATIO"), ratio)),
        );
        if settings.flag("hdr", false) {
            command.env(var("HDR"), "1");
        }
        if let Some(limit) = settings.number("fps_limit").filter(|&limit| limit > 0.0) {
            command.env(var("FPS_LIMIT"), format!("{limit:.0}"));
        }
        command.envs(optional_env(names, settings, &game.title_id, &port_reads));
        let fsr411 = self.root.fsr411();
        if tokio::fs::metadata(&fsr411).await.is_ok_and(|meta| meta.is_dir()) {
            command.env(var("FSR411_DIR"), fsr411);
        }
        if settings.flag("crash_diag", false) {
            for suffix in ["FREE_CHECK", "WRITE_LOG", "PRODUCER_CHECK"] {
                command.env(var(suffix), "1");
            }
        }
        // The port's guard would turn it off again a few corrupted frames in; without the
        // record the port's own default stays.
        if learned.frame_overlap_off.is_some() {
            command.env(var("SUBMIT_AHEAD"), "0");
        }
        command.envs(online.into_iter().flat_map(|online| online.env(names)));
        for item in settings.text("extra_env").unwrap_or_default().split_whitespace() {
            if let Some((key, value)) = item.split_once('=') {
                command.env(key, value);
            }
        }
    }

    /// Connected controllers as `(GUID, name)`, from the tool bbport builds
    /// for this. Empty before the first build.
    pub async fn connected_gamepads(&self, toolchain: &Toolchain) -> Vec<(String, String)> {
        let port_dir = self.port_dir().await;
        let tool = Names::of(&port_dir).await.gpu_capabilities();
        let command = toolchain::command(
            toolchain,
            &self.root,
            Port::Bbport,
            &format!(r#""$DECANTER_PORT_DIR/{tool}" --gamepads"#),
        )
        .await;
        let Ok(mut command) = command else {
            return Vec::new();
        };
        command
            .env("DECANTER_PORT_DIR", port_dir)
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

/// How long a query of the port's binary may take, a first start through nix-portable included.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// A query of the port's binary (`--list-features`, `--list-effects`, `--online-overrides`): its
/// status and what it printed, refused past `max` bytes, read as they come, or past
/// [`QUERY_TIMEOUT`]. The process is killed then.
async fn query_port(mut command: Command, max: usize) -> io::Result<(ExitStatus, Vec<u8>)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        return Err(io::Error::other("port query without stdout"));
    };
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let answer = async {
        let mut out = Vec::new();
        stdout.take(limit).read_to_end(&mut out).await?;
        if out.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("answer larger than {max} bytes"),
            ));
        }
        child.wait().await.map(|status| (status, out))
    };
    tokio::time::timeout(QUERY_TIMEOUT, answer)
        .await
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "port query timed out")))
}

/// The settings passed only when they differ from what the port does without them: values the
/// user picked (empty means the port's own choice), switches off in the port unless set (for all
/// titles or this one), and those on in the port unless told otherwise (camera cuts, image
/// traps), for which only off is passed.
fn optional_env<'settings>(
    names: Names,
    settings: &'settings LauncherSettings,
    title_id: &'settings str,
    port_reads: &'settings impl Fn(&str) -> bool,
) -> impl Iterator<Item = (String, String)> + 'settings {
    let values = [
        ("draw_pipe", "DRAW_PIPE"),
        ("readbacks", "READBACKS"),
        ("preupload", "PREUPLOAD"),
        ("gamepad", "GAMEPAD"),
        ("aniso", "ANISO"),
        ("intel_tonemap", "INTEL_TONEMAP_FIX"),
        ("sys_fonts_dir", "SYS_FONTS"),
    ]
    .into_iter()
    .filter(|&(key, _)| port_reads(key))
    .filter_map(move |(key, suffix)| settings.text(key).map(|value| (names.var(suffix), value.to_owned())));
    let on = [
        ("frame_stats", "FRAME_STATS"),
        ("save_log", "SAVE_LOG"),
        ("as_0_3", "AS_0_3"),
        ("gpu_profile", "GPU_PROFILE"),
        ("vk_validation", "VK_VALIDATION"),
    ]
    .into_iter()
    .filter(move |&(key, _)| port_reads(key) && (settings.flag(key, false) || settings.title_flag(title_id, key)))
    .map(move |(_, suffix)| (names.var(suffix), String::from("1")));
    let off = [("camera_cuts", "CAMERA_CUTS"), ("image_traps", "IMAGE_TRAPS")]
        .into_iter()
        .filter(|&(key, _)| !settings.flag(key, true))
        .map(move |(_, suffix)| (names.var(suffix), String::from("0")));
    let keep = settings
        .number("keep_logs")
        .map(|keep| (names.var("KEEP_LOGS"), format!("{keep:.0}")));
    values.chain(on).chain(off).chain(keep)
}

/// The port's debug mode, each run's logs gathered in `dir` under the port's own folder per host.
fn debug_env(names: Names, dir: &Path) -> [(String, &OsStr); 2] {
    [
        (names.var("DEBUG"), OsStr::new("1")),
        (names.var("DEBUG_DIR"), dir.as_os_str()),
    ]
}
