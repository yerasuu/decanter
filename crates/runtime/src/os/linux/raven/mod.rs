//! Raven ([mirkobrombin/raven](https://github.com/mirkobrombin/raven)), built in the library from
//! [`RAVEN_COMMIT`] once the owner confirmed that commit (#1067), inside a nix environment of its
//! own ([`RECIPE`]), and run from the toolchain that built it.
//!
//! The build happens where it stays, `<raven>/<commit>/`, because CMake writes absolute paths into
//! the build tree and Raven runs from it; it counts as installed only once one rename puts its
//! marker in place, after every step passed. A build cut short leaves no marker, and its folder
//! goes with it.

mod build;

use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use decanter_core::runner::{Commit, Installed, Origin, Pin, Progress, Stage, Status};
use decanter_nix::{Recipe, Toolchain};

use crate::Launch;
use crate::runner::{RAVEN_COMMIT, raven_dir};

const RECIPE: Recipe = Recipe {
    name: "raven",
    env_file: "raven.nix",
    env: include_str!("raven.nix"),
    host_check: HOST_CHECK,
};

/// What [`RECIPE`] brings, asked of the host before nix is used.
const HOST_CHECK: &str = "command -v gcc && command -v g++ && command -v cmake && command -v ninja \
     && command -v python3 && command -v pkg-config && command -v x86_64-w64-mingw32-gcc \
     && pkg-config --exists libffi wayland-client wayland-protocols xkbcommon pixman-1 freetype2 libpng openssl";

/// The runtime, relative to the checkout: what the build makes and what runs programs.
const BINARY: &str = "build/native/raven";

/// In the checkout once its build finished, holding the id of the toolchain that built it.
const BUILT: &str = ".decanter-built";

const CONFIRMED: &str = "confirmed";
const LOG: &str = "build.log";
const LOCK: &str = ".lock";
const TOOLCHAIN: &str = "toolchain";
/// Ends the name of a cut-short build set aside to be removed.
const TRASH: &str = ".trash";

/// The one commit decanter builds, from `pin`.
fn pinned(pin: &Pin) -> anyhow::Result<Commit> {
    match pin {
        Pin::Commit(commit) if commit.as_str() == RAVEN_COMMIT => Ok(commit.clone()),
        Pin::Commit(_) | Pin::Release(_) => bail!("Raven {pin} is not the pinned commit {RAVEN_COMMIT}"),
    }
}

fn folder() -> io::Result<PathBuf> {
    raven_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the active library isn't available"))
}

fn is_built(checkout: &Path) -> bool {
    checkout.join(BUILT).is_file() && checkout.join(BINARY).is_file()
}

fn installed(commit: Commit, checkout: &Path) -> Installed {
    Installed {
        pin: Pin::Commit(commit),
        origin: Origin::Local,
        path: checkout.join(BINARY),
    }
}

/// Every finished build in the active library's Raven folder.
pub(crate) fn raven_status() -> Status {
    let Some(entries) = raven_dir().and_then(|folder| std::fs::read_dir(folder).ok()) else {
        return Status::Absent;
    };
    let mut builds: Vec<Installed> = entries
        .flatten()
        .filter_map(|entry| {
            let commit = entry.file_name().to_str().and_then(Commit::try_new)?;
            let checkout = entry.path();
            is_built(&checkout).then(|| installed(commit, &checkout))
        })
        .collect();
    if builds.is_empty() {
        return Status::Absent;
    }
    builds.sort_by(|left, right| left.path.cmp(&right.path));
    Status::Installed(builds)
}

pub(crate) async fn raven_confirm(pin: &Pin) -> io::Result<()> {
    let commit = pinned(pin).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, format!("{err:#}")))?;
    let folder = folder()?;
    tokio::fs::create_dir_all(&folder).await?;
    tokio::fs::write(folder.join(CONFIRMED), commit.as_str()).await?;
    tracing::info!(%commit, "Raven build confirmed by the owner");
    Ok(())
}

async fn is_confirmed(folder: &Path, commit: &Commit) -> bool {
    tokio::fs::read_to_string(folder.join(CONFIRMED))
        .await
        .is_ok_and(|confirmed| confirmed.trim() == commit.as_str())
}

pub(crate) async fn raven_prepare(pin: &Pin, on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<Installed> {
    let commit = pinned(pin)?;
    let folder = folder()?;
    let checkout = folder.join(commit.as_str());
    if is_built(&checkout) {
        return Ok(installed(commit, &checkout));
    }
    if !is_confirmed(&folder, &commit).await {
        bail!("building Raven {commit} was not confirmed");
    }
    let lock = lock(&folder).await?;
    if is_built(&checkout) {
        return Ok(installed(commit, &checkout));
    }
    let leftover = Leftover(Some(checkout.clone()));
    let mut log = build::Log::create(&folder.join(LOG)).await?;
    log.line(&format!("building Raven {commit} in {}", checkout.display()))
        .await;
    on_progress(Progress {
        stage: Stage::Sources,
        received: 0,
        total: None,
        item: None,
    });
    build::clone(&checkout, &commit, &mut log).await?;
    let toolchain_dir = folder.join(TOOLCHAIN);
    let toolchain = Toolchain::resolve(&toolchain_dir, &RECIPE, |received, total| {
        on_progress(Progress {
            stage: Stage::Downloading,
            received,
            total,
            item: None,
        });
    })
    .await
    .context("getting a toolchain to build Raven")?;
    log.line(&format!("toolchain: {}", toolchain.id())).await;
    let compiled = build::compile(&toolchain, &toolchain_dir, &RECIPE, &checkout, &mut log, on_progress).await;
    if let Err(err) = compiled {
        if toolchain == Toolchain::Host
            && let Err(refused) = Toolchain::refuse_host(&toolchain_dir, &RECIPE).await
        {
            tracing::warn!(%refused, "cannot set the host's tools aside for Raven's next build");
        }
        log.line(&format!("build failed: {err:#}")).await;
        return Err(err);
    }
    mark_built(&checkout, &toolchain).await?;
    log.line("build finished").await;
    leftover.keep();
    drop(lock);
    tracing::info!(%commit, toolchain = toolchain.id(), "Raven built");
    Ok(installed(commit, &checkout))
}

/// One rename, so a build is installed whole or not at all.
async fn mark_built(checkout: &Path, toolchain: &Toolchain) -> anyhow::Result<()> {
    let part = checkout.join(format!("{BUILT}.part"));
    tokio::fs::write(&part, toolchain.id())
        .await
        .context("writing Raven's build marker")?;
    tokio::fs::rename(&part, checkout.join(BUILT))
        .await
        .context("putting Raven's build marker in place")
}

/// Builds [`RAVEN_COMMIT`] and, once it is newly in, removes the builds of other commits.
pub(crate) async fn raven_update(on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<Installed> {
    let Some(pin) = crate::runner::raven_pin() else {
        bail!("Raven's pinned commit {RAVEN_COMMIT:?} is no commit id");
    };
    let installed = raven_prepare(&pin, on_progress).await?;
    remove_other_builds(&installed.path).await;
    Ok(installed)
}

async fn remove_other_builds(binary: &Path) {
    let Some(folder) = raven_dir() else {
        return;
    };
    let Ok(mut entries) = tokio::fs::read_dir(&folder).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let is_other_build =
            entry.file_name().to_str().and_then(Commit::try_new).is_some() && !binary.starts_with(&path);
        if is_other_build && let Err(err) = tokio::fs::remove_dir_all(&path).await {
            tracing::warn!(path = %path.display(), %err, "cannot remove an old Raven build");
        }
    }
}

/// Raven as a whole: the build, the toolchain, the build log and the confirmation, so a next build
/// asks again. The programs added to the section and their prefixes are the player's and stay.
pub(crate) async fn raven_remove(pin: &Pin) -> io::Result<()> {
    let commit = pinned(pin).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, format!("{err:#}")))?;
    let folder = folder()?;
    for path in [folder.join(commit.as_str()), folder.join(TOOLCHAIN)] {
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    for path in [folder.join(LOG), folder.join(CONFIRMED)] {
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    tracing::info!(%commit, folder = %folder.display(), "Raven removed");
    Ok(())
}

pub(crate) fn raven_log_path(pin: &Pin) -> Option<PathBuf> {
    pinned(pin).ok()?;
    raven_dir().map(|folder| folder.join(LOG))
}

pub(crate) async fn raven_launch(mut launch: Launch) -> anyhow::Result<Launch> {
    let folder = folder()?;
    let checkout = folder.join(RAVEN_COMMIT);
    if !is_built(&checkout) {
        bail!("Raven is not built; build it in the Raven section first");
    }
    let Some(prefix) = launch.prefix.as_ref().map(|prefix| prefix.path().to_owned()) else {
        bail!("no Raven prefix to run {} in", launch.executable.display());
    };
    tokio::fs::create_dir_all(&prefix)
        .await
        .with_context(|| format!("creating the Raven prefix {}", prefix.display()))?;
    let toolchain_dir = folder.join(TOOLCHAIN);
    let toolchain = built_with(&toolchain_dir, &checkout).await?;
    let (script, variables) = invocation(&checkout.join(BINARY), &launch.executable, &prefix);
    let command = toolchain
        .command(&toolchain_dir, &RECIPE, &checkout, script)
        .await
        .context("entering Raven's toolchain")?;
    let command = command.as_std();
    let mut args = Vec::new();
    for arg in command.get_args() {
        args.push(utf8(arg)?);
    }
    for (name, value) in command.get_envs() {
        // A variable the toolchain removes is removed again by the launch itself.
        if let Some(value) = value {
            launch.env.push((utf8(name)?, utf8(value)?));
        }
    }
    for (name, value) in variables {
        launch.env.push((name.to_owned(), utf8(value.as_os_str())?));
    }
    if launch.working_directory.is_none() {
        launch.working_directory = launch.executable.parent().map(Path::to_path_buf);
    }
    tracing::debug!(
        executable = %launch.executable.display(),
        prefix = %prefix.display(),
        toolchain = toolchain.id(),
        "Raven command built"
    );
    launch.executable = PathBuf::from(command.get_program());
    launch.args = args;
    launch.native = true;
    Ok(launch)
}

/// How Raven is asked to run `executable` in `prefix`: a script for the toolchain's shell, and the
/// variables it and Raven read, since nix-shell's `--run` passes no arguments through.
// TODO(#1067): confirm after the first real build, against Raven's README and docs at
// RAVEN_COMMIT: (1) that `--run-process <exe>` starts a GUI program as it started a console
// hello.exe in the spike (which otherwise only used `--inspect`); (2) how Raven is told its
// prefix: `RAVEN_PREFIX` is assumed, it may be a flag or a default under `$HOME`; (3) whether the
// guest components (`provision-*.py`) must be provisioned into the prefix before a first run, as
// the spike's `missing PE module: msvcrt.dll` suggests.
fn invocation(binary: &Path, executable: &Path, prefix: &Path) -> (&'static str, [(&'static str, PathBuf); 3]) {
    (
        "exec \"$DECANTER_RAVEN\" --run-process \"$DECANTER_RAVEN_EXE\"",
        [
            ("DECANTER_RAVEN", binary.to_owned()),
            ("DECANTER_RAVEN_EXE", executable.to_owned()),
            ("RAVEN_PREFIX", prefix.to_owned()),
        ],
    )
}

/// The toolchain the build at `checkout` came from, as its marker names it.
async fn built_with(toolchain_dir: &Path, checkout: &Path) -> anyhow::Result<Toolchain> {
    let id = tokio::fs::read_to_string(checkout.join(BUILT))
        .await
        .context("reading Raven's build marker")?;
    if id.trim() == Toolchain::Host.id() {
        return Ok(Toolchain::Host);
    }
    Toolchain::nix(toolchain_dir, &RECIPE, |_, _| {})
        .await
        .context("finding the nix that built Raven")
}

fn utf8(text: &std::ffi::OsStr) -> anyhow::Result<String> {
    match text.to_str() {
        Some(text) => Ok(text.to_owned()),
        None => bail!("{} is not UTF-8", text.display()),
    }
}

/// Takes the folder's build lock, then removes what builds cut short by a crash left behind.
async fn lock(folder: &Path) -> anyhow::Result<File> {
    tokio::fs::create_dir_all(folder)
        .await
        .context("creating the library's Raven folder")?;
    let folder = folder.to_owned();
    let locked = tokio::task::spawn_blocking(move || {
        let lock = File::create(folder.join(LOCK))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another decanter process is building Raven",
                ));
            }
            Err(TryLockError::Error(err)) => return Err(err),
        }
        sweep_stale(&folder);
        Ok(lock)
    })
    .await
    .context("locking the Raven folder")?;
    locked.context("locking the Raven folder")
}

/// A commit's folder without its marker is a build a crash cut short, and a folder set aside one
/// whose removal a crash cut short. Best effort, said in the log. Blocking file I/O.
fn sweep_stale(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_cut_short = Commit::try_new(&name).is_some() && !is_built(&path);
        let is_set_aside = name.starts_with('.') && name.ends_with(TRASH);
        if !is_cut_short && !is_set_aside {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "interrupted Raven build removed"),
            Err(err) => tracing::warn!(path = %path.display(), %err, "cannot remove an interrupted Raven build"),
        }
    }
}

/// The checkout of a build that ends early, a cancel dropping its future included: renamed out of
/// the way at once, so a build started right after never meets it, then removed on a thread of its
/// own since a build tree holds gigabytes.
struct Leftover(Option<PathBuf>);

impl Leftover {
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Leftover {
    fn drop(&mut self) {
        let Some(checkout) = self.0.take() else {
            return;
        };
        let name = checkout.file_name().unwrap_or_default().to_string_lossy();
        let path = checkout.with_file_name(format!(".{name}.{}{TRASH}", std::process::id()));
        if let Err(err) = std::fs::rename(&checkout, &path) {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %checkout.display(), %err, "cannot set aside what a Raven build left");
            }
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("raven-cleanup".to_owned())
            .spawn(move || {
                if let Err(err) = std::fs::remove_dir_all(&path)
                    && err.kind() != io::ErrorKind::NotFound
                {
                    tracing::warn!(path = %path.display(), %err, "cannot remove what a Raven build left");
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "cannot start removing what a Raven build left");
        }
    }
}
