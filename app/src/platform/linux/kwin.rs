//! The display decanter's windows open on, in a KDE Wayland session. A
//! Wayland client can't choose the output of its toplevels and gpui hands
//! `display_id` to layer-shell surfaces only, so KWin is asked instead,
//! through a script that sends every `decanter` window to the chosen output.
//! The outputs come from `kscreen-doctor -j`, Plasma's own read-only view of
//! them, since gpui names Wayland outputs only by a hash of the connector.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use zbus::Connection;

use crate::platform::{ConnectorName, KwinOutput};

const KWIN: &str = "org.kde.KWin";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING: &str = "org.kde.kwin.Scripting";
/// Also how a script left behind by a crash is found and unloaded.
const PLUGIN: &str = "decanter-window-output";
const SCRIPT: &str = include_str!("window_output.js");
const OUTPUT_PLACEHOLDER: &str = "@OUTPUT@";
/// The window's first frame waits on this at start, so a stuck KWin or
/// kscreen-doctor costs at most this long.
const ANSWER_WAIT: Duration = Duration::from_secs(3);

static IS_LOADED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
struct Kscreen {
    outputs: Vec<KscreenOutput>,
}

#[derive(Deserialize)]
struct KscreenOutput {
    name: String,
    enabled: bool,
    connected: bool,
    /// 1 for the primary output.
    #[serde(default)]
    priority: u32,
    size: Option<KscreenSize>,
}

#[derive(Deserialize)]
struct KscreenSize {
    width: u32,
    height: u32,
}

/// The session KWin can place decanter's windows in: gpui on Wayland, and
/// the desktop is Plasma. Whether KWin answers on the bus is asked later.
pub fn is_kde_wayland(compositor: &str) -> bool {
    compositor == "Wayland"
        && std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland")
        && std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktops| desktops.split(':').any(|desktop| desktop == "KDE"))
}

/// The enabled outputs, in KDE's order, or `None` outside a KDE Wayland
/// session or when they can't be listed.
pub async fn kwin_outputs(compositor: &str) -> Option<Vec<KwinOutput>> {
    drop(kwin_bus(compositor).await?);
    list_outputs().await
}

/// Replaces decanter's KWin script (or one a crash left behind) with one
/// for `output`, a connector name; `None` only removes it. Failures are
/// logged and leave windows where KWin puts them.
pub async fn place_windows_on(compositor: &str, output: Option<&str>) {
    let Some(bus) = kwin_bus(compositor).await else {
        return;
    };
    unload(&bus).await;
    let Some(saved) = output else {
        tracing::info!("no window output chosen, KWin places decanter's windows");
        return;
    };
    let wanted = match ConnectorName::try_from(saved.to_owned()) {
        Ok(name) => name,
        Err(name) => {
            tracing::warn!(output = name, "window_output setting is not a connector name, ignored");
            return;
        }
    };
    let Some(outputs) = list_outputs().await else {
        return;
    };
    if !outputs.iter().any(|output| output.name == wanted) {
        tracing::warn!(
            output = wanted.as_str(),
            "chosen window output is not connected, KWin places decanter's windows"
        );
        return;
    }
    load(&bus, &wanted).await;
}

/// At exit, so the script doesn't keep acting on a decanter that is gone.
pub async fn stop_placing_windows() {
    if !IS_LOADED.load(Ordering::Acquire) {
        return;
    }
    match Connection::session().await {
        Ok(bus) => unload(&bus).await,
        Err(err) => tracing::warn!(plugin = PLUGIN, error = %err, "no session bus to unload the KWin script"),
    }
}

async fn kwin_bus(compositor: &str) -> Option<Connection> {
    if !is_kde_wayland(compositor) {
        return None;
    }
    let bus = Connection::session()
        .await
        .inspect_err(|err| tracing::warn!(error = %err, "no session bus, KWin cannot place decanter's windows"))
        .ok()?;
    let reply = tokio::time::timeout(
        ANSWER_WAIT,
        bus.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(KWIN,),
        ),
    )
    .await;
    let has_kwin = match reply {
        Ok(Ok(reply)) => reply.body().deserialize::<bool>().map_err(|err| err.to_string()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(elapsed) => Err(elapsed.to_string()),
    };
    match has_kwin {
        Ok(true) => Some(bus),
        Ok(false) => {
            tracing::warn!(
                service = KWIN,
                "KWin is not on the session bus, it cannot place decanter's windows"
            );
            None
        }
        Err(err) => {
            tracing::warn!(service = KWIN, error = err, "could not ask the session bus for KWin");
            None
        }
    }
}

async fn list_outputs() -> Option<Vec<KwinOutput>> {
    let mut command = tokio::process::Command::new("kscreen-doctor");
    command
        .arg("-j")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let stdout = match tokio::time::timeout(ANSWER_WAIT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => output.stdout,
        Ok(Ok(output)) => {
            tracing::warn!(status = %output.status, "kscreen-doctor failed, KDE's outputs are unknown");
            return None;
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "cannot run kscreen-doctor, KDE's outputs are unknown");
            return None;
        }
        Err(elapsed) => {
            tracing::warn!(error = %elapsed, "kscreen-doctor did not answer, KDE's outputs are unknown");
            return None;
        }
    };
    let kscreen: Kscreen = serde_json::from_slice(&stdout)
        .inspect_err(|err| tracing::warn!(error = %err, "kscreen-doctor's output is not the JSON expected"))
        .ok()?;
    let outputs: Vec<_> = kscreen
        .outputs
        .into_iter()
        .filter(|output| output.enabled && output.connected)
        .filter_map(|output| {
            let size = output.size?;
            let name = ConnectorName::try_from(output.name)
                .inspect_err(|name| tracing::warn!(output = name, "kscreen-doctor listed an output name left out"))
                .ok()?;
            Some(KwinOutput {
                name,
                width: size.width,
                height: size.height,
                is_primary: output.priority == 1,
            })
        })
        .collect();
    tracing::debug!(count = outputs.len(), "listed KDE's outputs");
    Some(outputs)
}

async fn load(bus: &Connection, output: &ConnectorName) {
    let Some(path) = write_script(output).await else {
        return;
    };
    let Some(path_str) = path.to_str() else {
        tracing::warn!(path = %path.display(), "the KWin script's path is not UTF-8, it cannot be loaded");
        return;
    };
    let id = match call(bus, "loadScript", &(path_str, PLUGIN)).await {
        Ok(reply) => reply.body().deserialize::<i32>().map_err(|err| err.to_string()),
        Err(err) => Err(err),
    };
    match id {
        Ok(id) if id >= 0 => {}
        Ok(id) => {
            tracing::warn!(plugin = PLUGIN, id, "KWin rejected the window placement script");
            return;
        }
        Err(err) => {
            tracing::warn!(
                plugin = PLUGIN,
                error = err,
                "could not load the window placement script"
            );
            return;
        }
    }
    if let Err(err) = call(bus, "start", &()).await {
        tracing::warn!(
            plugin = PLUGIN,
            error = err,
            "could not start the window placement script"
        );
        return;
    }
    IS_LOADED.store(true, Ordering::Release);
    tracing::info!(
        plugin = PLUGIN,
        output = output.as_str(),
        "KWin opens decanter's windows on the chosen output"
    );
}

async fn unload(bus: &Connection) {
    let is_loaded = match call(bus, "isScriptLoaded", &(PLUGIN,)).await {
        Ok(reply) => reply.body().deserialize::<bool>().map_err(|err| err.to_string()),
        Err(err) => Err(err),
    };
    match is_loaded {
        Ok(false) => {}
        Ok(true) => match call(bus, "unloadScript", &(PLUGIN,)).await {
            Ok(_) => tracing::info!(plugin = PLUGIN, "unloaded the window placement script"),
            Err(err) => tracing::warn!(
                plugin = PLUGIN,
                error = err,
                "could not unload the window placement script"
            ),
        },
        Err(err) => tracing::warn!(
            plugin = PLUGIN,
            error = err,
            "could not ask KWin for the window placement script"
        ),
    }
    IS_LOADED.store(false, Ordering::Release);
}

async fn call<B>(bus: &Connection, method: &str, body: &B) -> Result<zbus::Message, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    match tokio::time::timeout(
        ANSWER_WAIT,
        bus.call_method(Some(KWIN), SCRIPTING_PATH, Some(SCRIPTING), method, body),
    )
    .await
    {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(err)) => Err(err.to_string()),
        Err(elapsed) => Err(elapsed.to_string()),
    }
}

/// In `$XDG_RUNTIME_DIR/decanter`, which only this user can read, so
/// nobody else can swap the script KWin is told to run.
async fn write_script(output: &ConnectorName) -> Option<PathBuf> {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    else {
        tracing::warn!("XDG_RUNTIME_DIR is not set to an absolute path, no place for the KWin script");
        return None;
    };
    let dir = runtime_dir.join("decanter");
    if let Err(err) = super::create_private_dir(&dir).await {
        tracing::warn!(dir = %dir.display(), error = %err, "cannot create the KWin script's folder");
        return None;
    }
    if !super::is_owner_only(&dir) {
        tracing::warn!(dir = %dir.display(), "the KWin script's folder is not this user's alone, not writing it");
        return None;
    }
    let path = dir.join("window-output.js");
    let script = SCRIPT.replacen(OUTPUT_PLACEHOLDER, output.as_str(), 1);
    match write_private(&path, &script).await {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot write the KWin script");
            None
        }
    }
}

async fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    super::restrict_to_owner(&mut options);
    let mut file = options.open(path).await?;
    file.write_all(contents.as_bytes()).await?;
    file.flush().await
}
