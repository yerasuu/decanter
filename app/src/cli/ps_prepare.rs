//! `decanter ps prepare`: a port made ready to run without a window, through the steps the PS section
//! takes ([`crate::state::update_and_prepare_port`]), so `decanter ps run` finds it built.
//!
//! What differs from the section, because there is no window around it: the jobs' output goes only to
//! their logs in the library, and what is printed is a line per step, a line a minute while a job runs
//! and a summary at the end; a timeout kills the job running. Where `psport.exe` runs directly, it is
//! made ready as Play makes it ready there.

use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use decanter_core::Db;
use decanter_core::capabilities::{Capabilities, Feature};
use decanter_nix::Toolchain;
use decanter_ps::bbport;
use decanter_ps::failures;
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use decanter_ps::process::{Event, Job, JobKind};
use decanter_ps::run_log::{self, RunLog, describe_exit};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

use super::ps_run::NOT_STARTED;
use crate::i18n::Text;
use crate::ps_play::run::{self, Finished, Launcher};
use crate::state::{JobKey, PortHost, PortState, Stage, job_target};

/// Lines taken per batch, as the window's jobs take them.
const BATCH: usize = 256;
/// How often a running job says it still runs, so a log tells a long build from a stuck one.
const HEARTBEAT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub enum Outcome {
    Ready,
    Failed,
    Timeout,
    NotStarted,
}

impl Outcome {
    pub const fn code(self) -> i32 {
        match self {
            Self::Ready => 0,
            Self::Failed | Self::Timeout => 1,
            Self::NotStarted => NOT_STARTED,
        }
    }

    const fn status(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::NotStarted => "could not start",
        }
    }
}

/// What runs the port once it is ready.
enum Ready {
    Toolchain(Toolchain),
    /// The folder `psport.exe` runs from.
    #[cfg(windows)]
    Folder(PathBuf),
}

/// Never fails: whatever stops the port from getting ready is printed, and the outcome says it.
pub async fn run(db: &Db, port: Port, timeout: Option<u64>) -> Outcome {
    let root = match library(db) {
        Ok(root) => root,
        Err(why) => return summary(Outcome::NotStarted, port, &paths::active(), None, None, Some(&why)).await,
    };
    // As the section: where psport.exe runs directly, or without the checkouts' builds, nothing is built.
    let is_built_here = !bbport::direct::IN_USE && Capabilities::detect().is_offered(Feature::PsBuilds);
    #[cfg(not(windows))]
    if !is_built_here {
        let why = Text::new("ps.errors.builds_unavailable");
        return summary(Outcome::NotStarted, port, &root, None, None, Some(&why)).await;
    }
    #[cfg(windows)]
    if !is_built_here && port != Port::Bbport {
        let why = Text::new("ps.play.direct.port_unavailable").arg("name", port.name());
        return summary(Outcome::NotStarted, port, &root, None, None, Some(&why)).await;
    }
    let mut host = Printer::new();
    let prepared = {
        let preparing = async {
            #[cfg(windows)]
            if !is_built_here {
                return directly(port, &root).await;
            }
            crate::state::update_and_prepare_port(&mut host, port, &root)
                .await
                .map(Ready::Toolchain)
        };
        match timeout {
            Some(secs) => tokio::time::timeout(Duration::from_secs(secs), preparing).await.ok(),
            None => Some(preparing.await),
        }
    };
    let log = host.last_log.as_deref();
    match prepared {
        Some(Ok(ready)) => summary(Outcome::Ready, port, &root, Some(&ready), log, None).await,
        Some(Err(why)) => summary(Outcome::Failed, port, &root, None, log, Some(&why)).await,
        None => {
            tracing::warn!(port = port.name(), timeout, "port preparation timed out");
            let why = Text::new("ps.prepare.errors.timeout").arg("secs", timeout.unwrap_or_default());
            summary(Outcome::Timeout, port, &root, None, log, Some(&why)).await
        }
    }
}

/// The active profile's library, made the one the PS paths point into.
fn library(db: &Db) -> Result<Root, Text> {
    let profile = match db.active_profile() {
        Ok(Some(profile)) => profile,
        Ok(None) => return Err(Text::new("ps.report.errors.no_profile")),
        Err(err) => return Err(Text::new("ps.run.errors.profile").arg("error", err)),
    };
    if let Some(path) = crate::core::missing_library(&profile.id) {
        return Err(Text::new("ps.errors.library_missing").arg("path", path.display()));
    }
    let library = match crate::core::library_for_profile(&profile.id) {
        Ok(library) => library,
        Err(err) => return Err(Text::new("ps.run.errors.library").arg("error", err)),
    };
    paths::use_library(library.root());
    Ok(paths::active())
}

/// As Play where `psport.exe` runs directly: the folder picked in Ajustes, else the build or version
/// chosen, else psport's newest Windows package, downloaded and chosen.
#[cfg(windows)]
async fn directly(port: Port, root: &Root) -> Result<Ready, Text> {
    use decanter_ps::bbport::LauncherSettings;
    use decanter_ps::os::windows::package::{self, PINNED_KEY, SELECTED_KEY};

    let settings = tokio::task::block_in_place(|| LauncherSettings::load(root));
    if let Some(dir) = crate::ps_play::direct::chosen_dir(root, &settings).await {
        return Ok(Ready::Folder(dir));
    }
    let offers = match package::offers(port).await {
        Ok(offers) => offers,
        Err(err) => {
            tracing::warn!(%err, "no psport Windows package to prepare");
            return Err(Text::new("ps.play.package.unavailable").arg("error", err));
        }
    };
    let Some(newest) = offers.into_iter().next() else {
        tracing::info!("no psport release carries a Windows package yet");
        return Err(Text::new("ps.play.package.none"));
    };
    let tag = newest.version.tag().to_owned();
    println!("step: downloading psport {tag} for Windows");
    let installed = match package::install(root, port, &newest, |_, _| {}).await {
        Ok(installed) => installed,
        Err(err) => {
            tracing::warn!(tag, %err, "psport Windows package not installed");
            return Err(Text::new("ps.play.package.unavailable").arg("error", err));
        }
    };
    let saved = tokio::task::block_in_place(|| {
        LauncherSettings::edit(root, |edited| {
            edited.set_text(SELECTED_KEY, &tag);
            edited.remove(PINNED_KEY);
        })
    });
    if let Err(err) = saved {
        tracing::warn!(tag, %err, "could not save which psport Windows package is in use");
        return Err(Text::new("ps.settings.save_error").arg("error", err));
    }
    tracing::info!(tag, "psport Windows package in use");
    Ok(Ready::Folder(package::dir(root, &installed.version)))
}

/// The port's state kept for the steps that read it back, its stages printed as they change.
struct Printer {
    state: PortState,
    is_configure_failed: bool,
    last_log: Option<PathBuf>,
}

impl Printer {
    const fn new() -> Self {
        Self {
            state: PortState::new(),
            is_configure_failed: false,
            last_log: None,
        }
    }
}

/// A download's progress changes the stage many times a second; only the step it is in is printed.
const fn step(stage: &Stage) -> Option<&'static str> {
    match stage {
        Stage::Idle => None,
        Stage::Resolving => Some("resolving"),
        Stage::Downloading(..) => Some("downloading nix-portable"),
        Stage::DownloadingBundle(..) => Some("downloading the release bundle"),
        Stage::Cloning => Some("cloning"),
        Stage::Updating => Some("updating the checkout"),
        Stage::Fetching => Some("fetching the CI build"),
        Stage::Building => Some("building"),
        Stage::RollingBack => Some("rolling back to the last commit that built"),
        Stage::Ready => Some("ready"),
        Stage::Failed(_) => Some("failed"),
    }
}

/// Kills the job's process group when the preparation is dropped while it runs, as at the timeout.
struct Running(Job);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.kill();
    }
}

impl PortHost for Printer {
    fn port<T>(&mut self, _port: Port, change: impl FnOnce(&mut PortState) -> T) -> Option<T> {
        let before = mem::discriminant(self.state.stage());
        let changed = change(&mut self.state);
        if mem::discriminant(self.state.stage()) != before
            && let Some(step) = step(self.state.stage())
        {
            println!("step: {step}");
        }
        Some(changed)
    }

    async fn run(
        &mut self,
        root: &Root,
        port: Port,
        kind: JobKind,
        facts: Vec<(&'static str, String)>,
        command: Command,
    ) -> Finished {
        let target = job_target(&JobKey::Port(port));
        let mut launcher = Launcher::Command(command);
        let started_log = RunLog::start(
            &run_log::dir(root),
            &target,
            kind,
            &facts,
            launcher.command_mut(),
            false,
        )
        .await;
        let mut log = match started_log {
            Ok(log) => Some(log),
            Err(err) => {
                tracing::warn!(job = kind.name(), target, %err, "could not create the job's log file");
                None
            }
        };
        self.last_log = log.as_ref().map(|log| log.path().to_owned());
        self.is_configure_failed = false;
        let program = launcher.program();
        let (job, mut events) = match launcher.spawn(kind) {
            Ok(spawned) => spawned,
            Err(err) => {
                not_started(log, &program, &err, kind, &target).await;
                return Finished::none();
            }
        };
        let log_path = self.last_log.clone();
        tracing::info!(
            job = kind.name(),
            target,
            program,
            log = log_path.as_deref().map(|path| tracing::field::display(path.display())),
            "job started"
        );
        println!(
            "job: {} started, log: {}",
            kind.name(),
            inside_or_none(root, log_path.as_deref())
        );
        let started = Instant::now();
        let mut running = Running(job);
        let (exit, is_configure_failed) = follow(&mut running, &mut events, &mut log, &target, started).await;
        self.is_configure_failed = is_configure_failed;
        if let Some(file) = log {
            run::finish_log(file, exit, None, None, kind, &target).await;
        }
        run::trace_end(kind, &target, exit, false, false, started, log_path.as_deref());
        println!(
            "job: {} ended after {}s, exit {}",
            kind.name(),
            started.elapsed().as_secs(),
            describe_exit(exit)
        );
        run::finished(None, exit, false, log_path.as_deref(), None)
    }

    fn configure_failed(&mut self, _port: Port) -> bool {
        self.is_configure_failed
    }

    fn last_log(&mut self, _port: Port) -> Option<String> {
        self.last_log
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
    }

    /// `decanter ps prepare`'s runtime is multi-threaded ([`super::run`]), which this needs.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the window's host awaits a background task; nothing else runs here meanwhile"
    )]
    async fn blocking<T>(&self, work: impl FnOnce() -> T + Send + 'static) -> T
    where
        T: Send + 'static,
    {
        tokio::task::block_in_place(work)
    }

    async fn ready(&mut self, _port: Port, _root: &Root, _toolchain: &Toolchain) {}

    async fn move_checkout(&mut self, port: Port, root: &Root, command: Command) -> Option<bool> {
        Some(crate::state::move_port_checkout(self, port, root, command).await)
    }
}

async fn not_started(log: Option<RunLog>, program: &str, err: &io::Error, kind: JobKind, target: &str) {
    tracing::warn!(job = kind.name(), target, program, %err, "could not start job");
    println!("job: {} could not start {program}: {err}", kind.name());
    if let Some(mut file) = log {
        if let Err(err) = file.line(&format!("# error: could not start {program}: {err}")).await {
            tracing::warn!(job = kind.name(), target, %err, "the job's log file stopped taking lines");
        }
        run::finish_log(file, None, None, None, kind, target).await;
    }
}

/// The job's output into its log until it ends, with a line printed every [`HEARTBEAT`]. How it exited,
/// and whether it printed `CMake Error`.
async fn follow(
    running: &mut Running,
    events: &mut UnboundedReceiver<Event>,
    log: &mut Option<RunLog>,
    target: &str,
    started: Instant,
) -> (Option<ExitStatus>, bool) {
    let kind = running.0.kind;
    let mut batch = Vec::with_capacity(BATCH);
    let mut exit = None;
    let mut is_configure_failed = false;
    let mut lines = 0_usize;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.reset();
    loop {
        tokio::select! {
            received = events.recv_many(&mut batch, BATCH) => {
                if received == 0 {
                    break;
                }
                run::log_lines(log, &batch, kind, target).await;
                for event in batch.drain(..) {
                    match event {
                        Event::Line(line) => {
                            lines = lines.saturating_add(1);
                            is_configure_failed |= line.contains("CMake Error");
                        }
                        Event::Exited(status) => {
                            exit = status;
                            running.0.push(Event::Exited(status));
                        }
                    }
                }
            }
            _ = heartbeat.tick() => {
                println!("job: {} running for {}s, {lines} lines", kind.name(), started.elapsed().as_secs());
            }
        }
    }
    (exit, is_configure_failed)
}

/// A path as it is inside the library: only those are printed.
fn inside(root: &Root, path: &Path) -> Option<String> {
    path.strip_prefix(root.library())
        .ok()
        .map(|inside| inside.display().to_string())
}

fn inside_or_none(root: &Root, path: Option<&Path>) -> String {
    path.and_then(|path| inside(root, path))
        .unwrap_or_else(|| "none".to_owned())
}

/// The block a script reads at the end, in English like the job logs: `key: value` lines.
async fn summary(
    outcome: Outcome,
    port: Port,
    root: &Root,
    ready: Option<&Ready>,
    log: Option<&Path>,
    why: Option<&Text>,
) -> Outcome {
    println!("status: {}", outcome.status());
    println!("port: {}", port.name());
    match ready {
        Some(Ready::Toolchain(toolchain)) => {
            println!("toolchain: {}", toolchain.id());
            let commit = failures::full_commit_of(&port.checkout(root)).await;
            if !commit.is_empty() {
                println!("commit: {commit}");
            }
        }
        #[cfg(windows)]
        Some(Ready::Folder(dir)) => println!(
            "psport.exe: {}",
            inside(root, dir).unwrap_or_else(|| "outside the library".to_owned())
        ),
        None => {}
    }
    if let Some(why) = why {
        println!("reason: {why}");
    }
    println!("log: {}", inside_or_none(root, log));
    outcome
}
