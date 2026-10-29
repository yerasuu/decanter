//! `decanter ps run`: one PS game played without a window, through the same launch, scan and failure
//! record as the window's Play ([`crate::ps_play`]), so `decanter ps report` reads what it left.
//!
//! What differs from Play, because there is no window or app around it: the port must already be
//! built (nothing is cloned, fetched, downloaded or built), a PS5 game must already be converted,
//! online play stays off whatever the settings say, an automatic aspect ratio is left to the port,
//! and a Vulkan validation layer is used only when a launch from the window already realised it.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant, SystemTime};

use decanter_core::Db;
use decanter_core::capabilities::{Capabilities, Feature};
use decanter_nix::Toolchain;
use decanter_nix::validation;
use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::{self, Bbport, Names};
use decanter_ps::failures::{self, Kind, Scanner};
use decanter_ps::library::{self, Game};
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use decanter_ps::process::{Event, Job, JobKind};
use decanter_ps::run_log::{self, RunLog};
use decanter_ps::status::{self, Entry, Status};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::i18n::Text;
#[cfg(windows)]
use crate::ps_play::direct;
use crate::ps_play::launch::{self, OnlinePlay, Surroundings};
use crate::ps_play::run::{self, Launcher};

/// A week: a soak test longer than that is not what this command is for.
pub const MAX_TIMEOUT: u64 = 7 * 24 * 60 * 60;
/// The exit code of a game that could not start, apart from 1 for a run that failed.
pub const NOT_STARTED: i32 = 2;
/// How long the game has to close after it was asked to, before its process group is killed.
const STOP_GRACE: Duration = Duration::from_secs(15);
const MAX_ENV: usize = 32;
const MAX_ENV_NAME: usize = 64;
const MAX_ENV_VALUE: usize = 256;
/// Lines taken per batch, as the window's jobs take them.
const BATCH: usize = 256;
/// What would turn online play on or point it at a server (`Online::env`), refused in `--env` and
/// removed from the game's environment, since a headless run connects nowhere by itself.
const ONLINE_VARS: [&str; 10] = [
    "ONLINE",
    "SHADNET_SERVER",
    "SHADNET_WEBAPI",
    "SHADNET_NPID",
    "SHADNET_PASSWORD",
    "SHADNET_TOKEN",
    "COMMUNITY_SERVER",
    "UPNP",
    "BBSC",
    "BBSC_LOCAL_SERVER",
];

#[derive(Clone, Copy)]
pub enum Outcome {
    Clean,
    /// A crash, a `STOP:` or a bad exit was recorded.
    Failure,
    Timeout,
    NotStarted,
}

impl Outcome {
    pub const fn code(self) -> i32 {
        match self {
            Self::Clean => 0,
            Self::Failure | Self::Timeout => 1,
            Self::NotStarted => NOT_STARTED,
        }
    }

    const fn status(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Failure => "failure",
            Self::Timeout => "timeout",
            Self::NotStarted => "could not start",
        }
    }
}

/// `--env`'s `PS_NAME=value`: the name `PS_` and up to 64 of `A-Z`, `0-9` and `_`, not one of
/// [`ONLINE_VARS`]; the value printable ASCII, at most 256 characters. The message names the
/// variable only once its name passed.
pub fn env_pair(value: &str) -> Result<(String, String), String> {
    let Some((name, text)) = value.split_once('=') else {
        return Err("expected NAME=VALUE".to_owned());
    };
    let is_name = name.strip_prefix("PS_").is_some_and(|rest| {
        (1..=MAX_ENV_NAME).contains(&rest.len())
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    });
    if !is_name {
        return Err(format!(
            "the name must be PS_ followed by 1 to {MAX_ENV_NAME} of A-Z, 0-9 and _"
        ));
    }
    if ONLINE_VARS
        .iter()
        .any(|online| name.strip_prefix("PS_") == Some(*online))
    {
        return Err(format!(
            "{name} is an online variable; decanter ps run never plays online"
        ));
    }
    if text.len() > MAX_ENV_VALUE || !text.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
        return Err(format!(
            "the value of {name} must be printable ASCII, at most {MAX_ENV_VALUE} characters"
        ));
    }
    Ok((name.to_owned(), text.to_owned()))
}

/// Never fails: whatever stops the game from starting is printed, and the outcome says it.
pub async fn run(db: &Db, title: &str, timeout: Option<u64>, env: Vec<(String, String)>) -> Outcome {
    if env.len() > MAX_ENV {
        return not_started(title, &Text::new("ps.run.errors.too_many_env").arg("max", MAX_ENV));
    }
    match ready(db, title, timeout, env).await {
        Ok(ready) => play(ready, timeout).await,
        Err(why) => not_started(title, &why),
    }
}

/// A game resolved and its command built, as Play builds it, ready to spawn.
struct Ready {
    root: Root,
    game: Game,
    port: Port,
    profile_id: String,
    launcher: Launcher,
    facts: Vec<(&'static str, String)>,
}

/// How the game starts, and the facts for its log's header.
type Started = Result<(Launcher, Vec<(&'static str, String)>), Text>;

/// The title is looked up first, so an unknown one says so whatever else would stop it.
async fn ready(db: &Db, title: &str, timeout: Option<u64>, env: Vec<(String, String)>) -> Result<Ready, Text> {
    let profile = match db.active_profile() {
        Ok(Some(profile)) => profile,
        Ok(None) => return Err(Text::new("ps.report.errors.no_profile")),
        Err(err) => return Err(Text::new("ps.run.errors.profile").arg("error", err)),
    };
    let library = match crate::core::library_for_profile(&profile.id) {
        Ok(library) => library,
        Err(err) => return Err(Text::new("ps.run.errors.library").arg("error", err)),
    };
    paths::use_library(library.root());
    let root = paths::active();
    let library_roots = library.roots();
    let entry = match pick(scan(&root, &library_roots).await, title) {
        Ok(entry) => entry,
        Err(err) => return Err(err),
    };
    let (game, port) = (entry.game, entry.port.unwrap_or(Port::Bbport));
    let folder = library_roots
        .iter()
        .find(|library_root| game.dir.starts_with(paths::games_in(library_root)))
        .cloned()
        .unwrap_or_else(|| root.library().to_owned());
    let cache_dir = launch::game_cache_dir(folder, &game.title_id).await;
    // As Play: where psport.exe runs directly, or without the checkouts' builds, no toolchain runs it.
    let is_built_here = !bbport::direct::IN_USE && Capabilities::detect().is_offered(Feature::PsBuilds);
    let started = match is_built_here.then_some(port) {
        Some(port) => through_toolchain(&root, port, &game, &profile.id, cache_dir.as_deref(), env).await,
        #[cfg(windows)]
        None => directly(&root, port, &game, &profile.id, cache_dir.as_deref(), &env).await,
        #[cfg(not(windows))]
        None => Err(Text::new("ps.errors.builds_unavailable")),
    };
    let (launcher, mut facts) = match started {
        Ok(started) => started,
        Err(err) => return Err(err),
    };
    facts.push(("run", "decanter ps run".to_owned()));
    facts.extend(timeout.map(|secs| ("timeout", format!("{secs}s"))));
    Ok(Ready {
        root,
        game,
        port,
        profile_id: profile.id,
        launcher,
        facts,
    })
}

/// The port's built checkout in its toolchain, with what a run of the port adds around it.
async fn through_toolchain(
    root: &Root,
    port: Port,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
    env: Vec<(String, String)>,
) -> Started {
    let Some(toolchain) = decanter_ps::os::linux::toolchain::ready(root, port).await else {
        tracing::warn!(port = port.name(), "port not built; decanter ps run never prepares it");
        return Err(Text::new("ps.run.errors.port_not_ready").arg("port", port.name()));
    };
    let command = match port {
        Port::Bbport => {
            let surroundings = Surroundings {
                online: OnlinePlay::Off,
                is_frame_gen_offered: is_frame_gen_offered(root, &toolchain).await,
                display_ratio: no_display_ratio,
            };
            launch::bbport_command(root, &toolchain, game, profile_id, cache_dir, surroundings).await
        }
        Port::AnyPs5 => {
            if !AnyPs5::new(root.clone()).is_converted(&game.title_id).await {
                return Err(Text::new("ps.run.errors.needs_convert").arg("title", &game.title_id));
            }
            launch::anyps5_run_command(root, &toolchain, game, profile_id, cache_dir).await
        }
    };
    let mut command = match command {
        Ok(command) => command,
        Err(err) => {
            tracing::warn!(port = port.name(), title_id = %game.title_id, error = %err, "could not start game");
            return Err(err.localized());
        }
    };
    command.envs(env);
    without_online(&mut command);
    let mut facts = run::port_facts(root, port, Some(&toolchain)).await;
    if let Some(fact) = validation_layer(root, &toolchain, &mut command).await {
        facts.push(("vk_validation_layer", fact));
    }
    Ok((Launcher::Command(launch::around_port(root, game, command).await), facts))
}

/// `psport.exe` as Play starts it here, from the folder picked in Ajustes or the build or package in
/// use; one that is not there yet is reported, never downloaded.
#[cfg(windows)]
async fn directly(
    root: &Root,
    port: Port,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
    env: &[(String, String)],
) -> Started {
    if let Err(err) = direct::check_port(port, game) {
        return Err(err);
    }
    let surroundings = Surroundings {
        online: OnlinePlay::Off,
        is_frame_gen_offered: Capabilities::detect().is_offered(Feature::PsFrameGen),
        display_ratio: no_display_ratio,
    };
    let inputs = match launch::launch_inputs(root, None, game, profile_id, surroundings).await {
        Ok(inputs) => inputs,
        Err(err) => {
            tracing::warn!(title_id = %game.title_id, error = %err, "could not start game");
            return Err(err.localized());
        }
    };
    let Some(dir) = direct::chosen_dir(root, &inputs.settings).await else {
        tracing::warn!(
            "no psport.exe folder picked and no Windows build or package in use; decanter ps run never downloads one"
        );
        return Err(Text::new("ps.run.errors.no_psport"));
    };
    let (mut prepared, facts) = match direct::session(root, game, &inputs, cache_dir, dir, env).await {
        Ok(prepared) => prepared,
        Err(err) => return Err(err),
    };
    without_online(prepared.command_mut());
    Ok((Launcher::Session(prepared), facts))
}

fn no_display_ratio() -> Option<&'static str> {
    tracing::info!("no window to take the display's aspect ratio from, the port keeps its own");
    None
}

/// Online play's variables gone from the game's environment, those the shell running this command
/// exported included.
fn without_online(command: &mut Command) {
    for names in [Names::Psport, Names::of_command(command)] {
        for suffix in ONLINE_VARS {
            command.env_remove(names.var(suffix));
        }
    }
}

/// The PS section's games as its scan lists them, from the folders that are there; none is made.
async fn scan(root: &Root, library_roots: &[PathBuf]) -> Vec<Entry> {
    let root = root.clone();
    let folders = std::iter::once(root.games())
        .chain(
            library_roots
                .iter()
                .skip(1)
                .map(|library_root| paths::games_in(library_root)),
        )
        .collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        let folders = folders.into_iter().filter(|folder| folder.is_dir()).collect::<Vec<_>>();
        status::classify(&root, &library::scan(&folders))
    })
    .await
    .unwrap_or_else(|err| {
        tracing::warn!(%err, "the scan of the PS games folders did not finish");
        Vec::new()
    })
}

/// The playable copy of `title`, the one ready as it is first when there are several.
fn pick(entries: Vec<Entry>, title: &str) -> Result<Entry, Text> {
    let matching = entries
        .into_iter()
        .filter(|entry| entry.game.title_id.eq_ignore_ascii_case(title))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err(Text::new("ps.run.errors.no_game").arg("title", title));
    }
    let best = matching
        .into_iter()
        .filter(|entry| entry.port.is_some())
        .filter_map(|entry| rank(&entry.status).map(|rank| (rank, entry)))
        .min_by_key(|(rank, _)| *rank);
    match best {
        Some((_, entry)) if entry.status == Status::NeedsConvert => {
            Err(Text::new("ps.run.errors.needs_convert").arg("title", title))
        }
        Some((_, entry)) => Ok(entry),
        None => Err(Text::new("ps.run.errors.not_playable").arg("title", title)),
    }
}

const fn rank(status: &Status) -> Option<u8> {
    match status {
        Status::Ready => Some(0),
        Status::Experimental => Some(1),
        Status::NeedsConvert => Some(2),
        Status::NeedsMerge(_)
        | Status::HasPlayable(_)
        | Status::OtherEdition
        | Status::NeedsUpdate
        | Status::NeedsVr
        | Status::Encrypted
        | Status::NoEboot => None,
    }
}

/// As the window's capabilities have it once the port reported its features: frame generation
/// only from a build that lists it.
async fn is_frame_gen_offered(root: &Root, toolchain: &Toolchain) -> bool {
    let Some(report) = bbport::host_features(&Bbport::new(root.clone()), toolchain).await else {
        return false;
    };
    let mut capabilities = Capabilities::detect();
    report.merge_into(&mut capabilities);
    capabilities.is_offered(Feature::PsFrameGen)
}

/// The validation layer when the game's settings ask for it and a launch from the window realised
/// it already; realising it here could fetch from nix's caches, so without it the game starts
/// without validating. The fact for the log's header, when it was asked for.
async fn validation_layer(root: &Root, toolchain: &Toolchain, command: &mut Command) -> Option<String> {
    let var = Names::of_command(command).var("VK_VALIDATION");
    let asked = command
        .as_std()
        .get_envs()
        .any(|(key, value)| key == var.as_str() && value.is_some());
    if !asked {
        return None;
    }
    let cached = match validation::realiser(toolchain).await {
        Ok(realiser) => validation::cached(&root.toolchain(), &realiser).await,
        Err(why) => {
            command.env_remove(&var);
            tracing::warn!(%why, "Vulkan validation layer unavailable; the game starts without validating");
            return Some(format!("off, layer unavailable: {why}"));
        }
    };
    let Some(dir) = cached else {
        command.env_remove(&var);
        tracing::warn!("Vulkan validation layer not realised yet; the game starts without validating");
        return Some("off, layer not realised yet (a launch from decanter's window realises it)".to_owned());
    };
    let fact = dir.display().to_string();
    command.env("VK_ADD_LAYER_PATH", dir);
    Some(fact)
}

async fn play(ready: Ready, timeout: Option<u64>) -> Outcome {
    let Ready {
        root,
        game,
        port,
        profile_id,
        mut launcher,
        mut facts,
    } = ready;
    crate::taskbar::sync_ps(root.library(), &game).await;
    let commit = failures::commit_of(&port.checkout(&root)).await;
    let mut scanner = Scanner::new(&game.dir, &game.title_id, &game.version, commit, true);
    let kind = JobKind::Game;
    let target = run::game_target(&game.dir);
    facts.extend(scanner.facts());
    let logs = run_log::dir(&root);
    let started_log = RunLog::start(&logs, &target, kind, &facts, launcher.command_mut(), true).await;
    let mut log = match started_log {
        Ok(log) => Some(log),
        Err(err) => {
            tracing::warn!(job = kind.name(), target, %err, "could not create the job's log file");
            None
        }
    };
    let log_path = log.as_ref().map(|log| log.path().to_owned());
    let log_file = log_path.as_deref();
    let calls_path = log.as_ref().and_then(|log| log.calls_path().map(Path::to_owned));
    let program = launcher.program();
    let ending = ending_of(&launcher);
    let (job, mut events) = match launcher.spawn(kind) {
        Ok(spawned) => spawned,
        Err(err) => {
            tracing::warn!(job = kind.name(), target, program, %err, "could not start job");
            note(&mut log, &format!("# error: could not start {program}: {err}"), &target).await;
            if let Some(file) = log {
                run::finish_log(file, None, None, None, kind, &target).await;
            }
            let why = Text::new("ps.run.errors.start").arg("error", err);
            return summary(Outcome::NotStarted, &game.title_id, &root, log_file, &[], Some(&why));
        }
    };
    tracing::info!(
        job = kind.name(),
        target,
        program,
        log = log_file.map(|path| tracing::field::display(path.display())),
        "job started"
    );
    let started = Instant::now();
    let mut running = Running { job, ending };
    let (exit, is_timed_out) = follow(&mut running, &mut events, &mut log, &mut scanner, timeout, &target).await;
    let report = run::read_call_report(calls_path.as_deref()).await;
    let killed_at = killed_at(&running.ending);
    if let Some(mut file) = log {
        file.set_killed(killed_at, decanter_runtime::KILLED_EXIT_CODE);
        run::finish_log(file, exit, report.as_ref(), calls_path, kind, &target).await;
    }
    let is_killed = killed_at.is_some();
    run::trace_end(kind, &target, exit, is_timed_out, is_killed, started, log_file);
    let finished = run::finished(Some(scanner), exit, is_timed_out, log_file, report);
    if let Some(line) = finished.frame_overlap_off
        && let Err(err) = run::save_frame_overlap_off(&root, &game.title_id, line).await
    {
        tracing::warn!(title_id = %game.title_id, %err, "could not record that frame overlap is off for the title");
    }
    if let Some(frame_gen) = finished.frame_gen {
        run::save_frame_gen(&root, &game.title_id, frame_gen).await;
    }
    let found = finished.failures;
    let is_fatal = found
        .iter()
        .any(|failure| matches!(failure.kind, Kind::Crash | Kind::Stop | Kind::Exit));
    let outcome = if is_timed_out {
        Outcome::Timeout
    } else if is_fatal {
        Outcome::Failure
    } else {
        Outcome::Clean
    };
    let kinds = found.iter().map(|failure| failure.kind).collect::<Vec<_>>();
    if !found.is_empty()
        && let Err(err) = run::save_failures(run::failures_path(&root, &profile_id), found).await
    {
        tracing::warn!(title_id = %game.title_id, %err, "could not record the run's failures");
        let why = Text::new("ps.failures.notice.save_error").arg("error", err);
        return summary(outcome, &game.title_id, &root, log_file, &kinds, Some(&why));
    }
    summary(outcome, &game.title_id, &root, log_file, &kinds, None)
}

/// How the timeout reaches the game.
enum Ending {
    /// Its process group, with signals.
    Group,
    /// The session (a Job Object) `psport.exe` runs in, whose end kills every process in it.
    #[cfg(windows)]
    Session(String),
}

#[cfg(windows)]
fn ending_of(launcher: &Launcher) -> Ending {
    launcher.session_key().map_or(Ending::Group, Ending::Session)
}

#[cfg(not(windows))]
const fn ending_of(_launcher: &Launcher) -> Ending {
    Ending::Group
}

/// When decanter killed the game's session, since its exit code then reads like the game's own.
#[cfg(windows)]
fn killed_at(ending: &Ending) -> Option<SystemTime> {
    match ending {
        Ending::Group => None,
        Ending::Session(key) => decanter_runtime::take_killed(key),
    }
}

/// A process group's kill shows in its exit status as a signal.
#[cfg(not(windows))]
const fn killed_at(_ending: &Ending) -> Option<SystemTime> {
    None
}

/// The running game, and how the timeout reaches it.
struct Running {
    job: Job,
    ending: Ending,
}

impl Running {
    /// Asks the game to close; a session's own stop also ends it if it won't, as Play's does.
    fn stop(&mut self) {
        match &self.ending {
            Ending::Group => self.job.stop(),
            #[cfg(windows)]
            Ending::Session(key) => {
                let key = key.clone();
                tokio::spawn(async move { decanter_runtime::stop(&key).await });
            }
        }
    }

    fn kill(&mut self) {
        match &self.ending {
            Ending::Group => self.job.kill(),
            #[cfg(windows)]
            Ending::Session(key) => {
                let key = key.clone();
                tokio::spawn(async move { decanter_runtime::end(&key).await });
            }
        }
    }
}

impl Ending {
    /// What the run's log says was killed.
    const fn killed(&self) -> &'static str {
        match self {
            Self::Group => "its process group",
            #[cfg(windows)]
            Self::Session(_) => "its session and every process in it",
        }
    }
}

/// The game's output into its log and the scanner until it ends, and at the timeout a stop request,
/// then a kill once [`STOP_GRACE`] is over. How it exited, and whether the timeout ended it.
async fn follow(
    running: &mut Running,
    events: &mut UnboundedReceiver<Event>,
    log: &mut Option<RunLog>,
    scanner: &mut Scanner,
    timeout: Option<u64>,
    target: &str,
) -> (Option<ExitStatus>, bool) {
    let mut alarm = timeout.map(|secs| tokio::time::Instant::now() + Duration::from_secs(secs));
    let mut is_timed_out = false;
    let mut batch = Vec::with_capacity(BATCH);
    let mut exit = None;
    loop {
        tokio::select! {
            received = events.recv_many(&mut batch, BATCH) => {
                if received == 0 {
                    break;
                }
                run::log_lines(log, &batch, running.job.kind, target).await;
                for event in batch.drain(..) {
                    match event {
                        Event::Line(line) => scanner.line(&line),
                        Event::Exited(status) => exit = status,
                    }
                }
            }
            () = sleep_until(alarm), if alarm.is_some() => {
                note(log, &timeout_line(timeout, is_timed_out.then_some(&running.ending)), target).await;
                if is_timed_out {
                    tracing::warn!(target, "game still running after the stop request, killing it");
                    running.kill();
                    alarm = None;
                } else {
                    tracing::info!(target, "timeout reached, asking the game to stop");
                    is_timed_out = true;
                    running.stop();
                    alarm = Some(tokio::time::Instant::now() + STOP_GRACE);
                }
            }
        }
    }
    (exit, is_timed_out)
}

/// What the run's log says at the timeout, and again with what is killed once the grace after it ran
/// out.
fn timeout_line(timeout: Option<u64>, killing: Option<&Ending>) -> String {
    match killing {
        Some(ending) => format!(
            "# timeout: still running {}s after the stop request, decanter killed {}",
            STOP_GRACE.as_secs(),
            ending.killed()
        ),
        None => format!(
            "# timeout: {}s reached, decanter asked the game to stop",
            timeout.unwrap_or_default()
        ),
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => core::future::pending().await,
    }
}

/// A line of decanter's own in the run's log, `#` first like its header and footer.
async fn note(log: &mut Option<RunLog>, line: &str, target: &str) {
    if let Some(file) = log.as_mut()
        && let Err(err) = file.line(line).await
    {
        tracing::warn!(target, %err, "the job's log file stopped taking lines");
        *log = None;
    }
}

fn not_started(title: &str, why: &Text) -> Outcome {
    summary(Outcome::NotStarted, title, &paths::active(), None, &[], Some(why))
}

/// The block a script reads at the end, in English like the run's log: `key: value` lines. The log
/// is named inside the library, the only path printed.
fn summary(
    outcome: Outcome,
    title: &str,
    root: &Root,
    log: Option<&Path>,
    kinds: &[Kind],
    why: Option<&Text>,
) -> Outcome {
    let count = |kind: Kind| kinds.iter().filter(|found| **found == kind).count();
    println!("status: {}", outcome.status());
    println!("title: {title}");
    if let Some(why) = why {
        println!("reason: {why}");
    }
    println!(
        "failures: crash {}, stop {}, error {}, exit {}, missing {}",
        count(Kind::Crash),
        count(Kind::Stop),
        count(Kind::Error),
        count(Kind::Exit),
        count(Kind::Missing)
    );
    match log.and_then(|log| log.strip_prefix(root.library()).ok()) {
        Some(inside) => println!("log: {}", inside.display()),
        None => println!("log: none"),
    }
    if !kinds.is_empty() {
        println!("report: decanter ps report --title {title} --print");
    }
    outcome
}
