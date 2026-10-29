//! A PS game's command as Play builds it: the launch's inputs read from the settings, the port's
//! command, and what a run of the port adds around it.

use std::io;
use std::path::{Path, PathBuf};

use decanter_core::{KeyringError, ShadnetSecret};
use decanter_nix::Toolchain;
use decanter_ps::anyps5::{self, AnyPs5};
use decanter_ps::bbport::GameLaunch;
use decanter_ps::bbport::frame_gen::FrameGen;
use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::local_player::{LocalPlayer, LocalPlayerError};
use decanter_ps::bbport::mods::{ModFiles, Mods};
use decanter_ps::bbport::online::{self, Online, OnlineError};
use decanter_ps::bbport::{self, Bbport, Ini, LauncherSettings};
use decanter_ps::learned;
use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::process;
use tokio::process::Command;

use crate::i18n::Text;

/// Why a game could not be started: logged as it is, worded for the UI by
/// [`LaunchError::localized`].
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("mods setup failed: {0}")]
    Mods(io::Error),
    #[error("could not create {}: {err}", dir.display())]
    CreateDir { dir: PathBuf, err: io::Error },
    #[error("launch environment setup failed: {0}")]
    Environment(io::Error),
    #[error("conversion output setup failed: {0}")]
    ConvertPrepare(io::Error),
    #[error("conversion failed")]
    ConvertFailed,
    #[error("run directory setup failed at {}: {err}", dir.display())]
    RunDir { dir: PathBuf, err: io::Error },
    #[error("online settings unusable: {0}")]
    Online(OnlineError),
    /// Asking for the password at each launch instead is left for later: the launch refuses.
    #[error("shadNet secrets unreadable: {0}")]
    Keyring(String),
    #[error("no shadNet password in the keyring for {0}")]
    NoPassword(String),
    #[error("local player unusable: {0}")]
    LocalPlayer(LocalPlayerError),
    /// The title's own port ini or launch settings could not be made from the shared ones, or
    /// its launch settings don't parse.
    #[error("settings of the title unusable: {0}")]
    TitleSettings(io::Error),
    /// The settings or the mods were read on a blocking thread that did not finish.
    #[error("launch setup interrupted: {0}")]
    Interrupted(io::Error),
}

impl LaunchError {
    pub fn localized(&self) -> Text {
        match self {
            Self::Mods(err) => Text::new("ps.play.mods_error").arg("error", err),
            Self::CreateDir { dir, err } => Text::new("ps.errors.create_dir")
                .arg("dir", dir.display())
                .arg("error", err),
            Self::Environment(err) | Self::Interrupted(err) => Text::new("ps.errors.environment").arg("error", err),
            Self::ConvertPrepare(err) => Text::new("ps.play.convert_prepare_error").arg("error", err),
            Self::ConvertFailed => Text::new("ps.play.convert_failed"),
            Self::RunDir { dir, err } => Text::new("ps.play.prepare_error")
                .arg("name", dir.display())
                .arg("error", err),
            Self::Online(err) => Text::new("ps.play.online_error").text("error", online_error(err)),
            Self::Keyring(err) => Text::new("ps.play.online_keyring_error").arg("error", err),
            Self::NoPassword(npid) => Text::new("ps.play.online_no_password").arg("npid", npid),
            Self::LocalPlayer(err) => Text::new("ps.play.local_player_error").arg("value", err.value()),
            Self::TitleSettings(err) => Text::new("ps.play.title_settings_error").arg("error", err),
        }
    }
}

/// Why an online setting can't be used, in the UI's language.
pub fn online_error(err: &OnlineError) -> Text {
    match err {
        OnlineError::Server(value) => Text::new("ps.online.errors.server").arg("value", value),
        OnlineError::Community(value) => Text::new("ps.online.errors.community").arg("value", value),
        OnlineError::Webapi(value) => Text::new("ps.online.errors.webapi").arg("value", value),
        OnlineError::NpidEmail => Text::new("ps.online.errors.npid_email"),
        OnlineError::Npid(value) => Text::new("ps.online.errors.npid").arg("value", value),
        OnlineError::Secret => Text::new("ps.online.errors.secret"),
    }
}

#[derive(Clone, Copy)]
pub enum OnlinePlay {
    /// As the settings have it, when this psport build has online play (`online_module`).
    AsSet { is_offered: bool },
    /// Never, whatever the settings say: a headless run connects nowhere by itself.
    Off,
}

/// What a launch takes from where it starts, decanter's window or a headless command.
pub struct Surroundings<F> {
    pub online: OnlinePlay,
    /// Whether the port's build lists frame generation (`frame_gen_fsr`).
    pub is_frame_gen_offered: bool,
    /// The display's aspect ratio, asked only when the port's setting is automatic.
    pub display_ratio: F,
}

/// Everything a bbport launch reads besides the game and its cache folder, gathered before the
/// launch borrows it ([`LaunchInputs::game_launch`]).
pub struct LaunchInputs {
    pub(crate) settings: LauncherSettings,
    /// The title's own port ini, `None` where the port reads the shared one.
    config: Option<PathBuf>,
    learned: learned::Title,
    aspect_ratio: Option<&'static str>,
    mods: Mods,
    debug_dir: Option<PathBuf>,
    user_dir: PathBuf,
    online: Option<Online>,
    local_player: Option<LocalPlayer>,
    frame_gen: Option<FrameGen>,
    menu_language: String,
}

impl LaunchInputs {
    pub fn game_launch<'inputs>(
        &'inputs self,
        game: &'inputs Game,
        cache_dir: Option<&'inputs Path>,
    ) -> GameLaunch<'inputs> {
        GameLaunch {
            game,
            user_dir: &self.user_dir,
            cache_dir,
            config: self.config.as_deref(),
            debug_dir: self.debug_dir.as_deref(),
            settings: &self.settings,
            mods: &self.mods,
            menu_language: &self.menu_language,
            learned: &self.learned,
            aspect_ratio: self.aspect_ratio,
            online: self.online.as_ref(),
            local_player: self.local_player.as_ref(),
            frame_gen: self.frame_gen,
        }
    }
}

/// File reads and the mods' icon encoding, off the async threads: a few hundred milliseconds at most.
async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, LaunchError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| LaunchError::Interrupted(io::Error::other(err)))
}

/// `toolchain` only tells the connected pads apart for the layout's button icons; without one the
/// icons take their automatic style's default.
pub async fn launch_inputs(
    root: &Root,
    toolchain: Option<&Toolchain>,
    game: &Game,
    profile_id: &str,
    surroundings: Surroundings<impl FnOnce() -> Option<&'static str>>,
) -> Result<LaunchInputs, LaunchError> {
    let read = {
        let root = root.clone();
        let title_id = game.title_id.clone();
        off_thread(move || {
            let port_ini = Bbport::title_ini(&root, &title_id)?;
            let settings = LauncherSettings::of_title(&root, &title_id)?;
            let mod_files = ModFiles::of_title(&root, &title_id, &settings)?;
            io::Result::Ok((
                settings,
                learned::Store::load(&root).title(&title_id),
                Ini::load(&port_ini.path).get("aspect_ratio").map(str::to_owned),
                port_ini,
                mod_files,
            ))
        })
        .await
    };
    let (settings, learned, chosen_ratio, port_ini, mod_files) = match read {
        Ok(Ok(read)) => read,
        Ok(Err(err)) => return Err(LaunchError::TitleSettings(err)),
        Err(err) => return Err(err),
    };
    let aspect_ratio = match chosen_ratio.as_deref() {
        None | Some("auto") => (surroundings.display_ratio)(),
        Some(_) => None,
    };
    let mods = if bbport::is_bloodborne(game) {
        // The automatic icon style follows the pad bbport will read: the one
        // picked in Mando, else the first connected.
        let detected = match toolchain {
            Some(toolchain) => {
                let pads = Bbport::new(root.clone()).connected_gamepads(toolchain).await;
                let chosen = settings.text("gamepad");
                pads.iter()
                    .find(|(guid, _)| Some(guid.as_str()) == chosen)
                    .or_else(|| pads.first())
                    .map(|(guid, name)| IconStyle::for_gamepad(guid, name))
            }
            None => None,
        };
        let game_dir = game.dir.clone();
        let for_mods = settings.clone();
        let root = root.clone();
        let ini_path = port_ini.path.clone();
        // The layout's button icons are built here when they need to be: a few
        // hundred milliseconds of decoding and encoding, off the UI thread.
        match off_thread(move || bbport::mods::prepare(&root, &game_dir, &for_mods, &mod_files, &ini_path, detected))
            .await
        {
            Ok(Ok(mods)) => mods,
            Ok(Err(err)) => return Err(LaunchError::Mods(err)),
            Err(err) => return Err(err),
        }
    } else {
        Mods::off(&mod_files)
    };
    let debug_dir = crate::debug_logs::ps_folder(settings.title_flag(&game.title_id, crate::debug_logs::TITLE_KEY));
    let user_dir = root.profile(profile_id).join("bbport/user");
    if let Err(err) = tokio::fs::create_dir_all(&user_dir).await {
        return Err(LaunchError::CreateDir { dir: user_dir, err });
    }
    let online = match online_play(&settings, surroundings.online).await {
        Ok(online) => online,
        Err(err) => return Err(err),
    };
    let local_player = if bbport::is_bloodborne(game) {
        match LocalPlayer::of_title(&settings, &game.title_id) {
            Ok(player) => player,
            Err(err) => return Err(LaunchError::LocalPlayer(err)),
        }
    } else {
        None
    };
    let frame_gen = surroundings
        .is_frame_gen_offered
        .then(|| FrameGen::of_title(&settings, &game.title_id));
    Ok(LaunchInputs {
        settings,
        config: port_ini.config().map(Path::to_path_buf),
        learned,
        aspect_ratio,
        mods,
        debug_dir,
        user_dir,
        online,
        local_player,
        frame_gen,
        menu_language: rust_i18n::locale().to_string(),
    })
}

/// `run.sh` in `toolchain` with the game's launch.
pub async fn bbport_command(
    root: &Root,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
    surroundings: Surroundings<impl FnOnce() -> Option<&'static str>>,
) -> Result<Command, LaunchError> {
    let inputs = match launch_inputs(root, Some(toolchain), game, profile_id, surroundings).await {
        Ok(inputs) => inputs,
        Err(err) => return Err(err),
    };
    Bbport::new(root.clone())
        .launch_command(toolchain, &inputs.game_launch(game, cache_dir))
        .await
        .map_err(LaunchError::Environment)
}

/// Online play when it is on and the build has it, with the account's secrets from the keyring.
async fn online_play(settings: &LauncherSettings, online: OnlinePlay) -> Result<Option<Online>, LaunchError> {
    let is_offered = match online {
        OnlinePlay::AsSet { is_offered } => is_offered,
        OnlinePlay::Off => return Ok(None),
    };
    if !settings.flag(online::ONLINE, false) {
        return Ok(None);
    }
    if !is_offered {
        tracing::warn!("online play is on, but this psport build has no online module: playing offline");
        return Ok(None);
    }
    let mut online = match Online::from_settings(settings) {
        Ok(Some(online)) => online,
        Ok(None) => return Ok(None),
        Err(err) => return Err(LaunchError::Online(err)),
    };
    let Some(npid) = online.npid().map(str::to_owned) else {
        tracing::info!("online play without a shadNet account");
        return Ok(Some(online));
    };
    let secrets = tokio::task::spawn_blocking(move || {
        let password = decanter_core::load_shadnet(&npid, ShadnetSecret::Password)?;
        let token = decanter_core::load_shadnet(&npid, ShadnetSecret::Token)?;
        Ok::<_, KeyringError>((password, token))
    })
    .await;
    let (password, token) = match secrets {
        Ok(Ok(secrets)) => secrets,
        Ok(Err(err)) => return Err(LaunchError::Keyring(err.to_string())),
        Err(err) => return Err(LaunchError::Keyring(err.to_string())),
    };
    if password.is_none() {
        tracing::warn!("online play with a shadNet NPID but no password in the keyring, not starting");
        return Err(LaunchError::NoPassword(online.npid().unwrap_or_default().to_owned()));
    }
    match online.set_secrets(password, token) {
        Ok(()) => Ok(Some(online)),
        Err(err) => Err(LaunchError::Online(err)),
    }
}

/// A converted game's run in the profile's own run directory; the conversion is the caller's.
pub async fn anyps5_run_command(
    root: &Root,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
) -> Result<Command, LaunchError> {
    let run_dir = root.profile(profile_id).join("anyps5").join(&game.title_id);
    let laid_out = {
        let title_id = game.title_id.clone();
        let run_dir = run_dir.clone();
        let root = root.clone();
        off_thread(move || anyps5::prepare_run_dir(&AnyPs5::new(root), &title_id, &run_dir)).await
    };
    match laid_out {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(LaunchError::RunDir { dir: run_dir, err }),
        Err(err) => return Err(err),
    }
    let mut command = match AnyPs5::new(root.clone()).run_command(toolchain, game, &run_dir).await {
        Ok(command) => command,
        Err(err) => return Err(LaunchError::Environment(err)),
    };
    // AnyPS5 has no cache of its own to root; only the drivers' follow the game.
    if let Some(dir) = cache_dir {
        command.envs(decanter_core::shader_cache_env(dir));
    }
    Ok(command)
}

/// The game's command with what a run of the port adds around it: MangoHud, GameMode and a scope of
/// its own, as the game's own settings ask.
pub async fn around_port(root: &Root, game: &Game, mut command: Command) -> Command {
    let key = game.key();
    let (mangohud, gamemoded) = crate::core::db()
        .map(|db| {
            (
                crate::game_tools::mangohud(&db, &key),
                crate::game_tools::gamemoded(&db, &key),
            )
        })
        .unwrap_or_default();
    if let Some(autostart_log) = mangohud {
        command.envs(crate::game_tools::mangohud_env(&key, root.library(), autostart_log).await);
    }
    if let Some(gamemoded) = gamemoded {
        command = process::through_gamemode(command, &gamemoded);
    }
    process::in_own_scope(command, &key, &game.title)
}

/// The game's cache folder in the cache location of `folder`, the library
/// folder holding its dump.
pub async fn game_cache_dir(folder: PathBuf, title_id: &str) -> Option<PathBuf> {
    let title_id = title_id.to_owned();
    tokio::task::spawn_blocking(move || crate::cache::game_dir(&folder, "ps", &title_id))
        .await
        .ok()
        .flatten()
}
