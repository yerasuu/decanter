//! The one way the app gets at runners: each operation a `match` over [`Runner`] onto the crate that
//! owns the runner, `decanter_runtime::runner` for umu-run, Proton, GE-Proton and Raven and
//! `decanter_ps::runner` for the PS ports; and the one place that words what each runner's
//! operations report for the UI, their errors ([`Error::text`]) and their progress ([`progress`]).

use std::borrow::Cow;
use std::fmt;
use std::io;
use std::path::PathBuf;

use decanter_core::capabilities::Support;
use decanter_core::runner::{Installed, Pin, Progress, Runner, Stage, Status};
use decanter_ps::os::windows::winbuild;
use decanter_ps::paths::Root;
use rust_i18n::t;

use crate::i18n::Text;
use crate::releases::{self, Release, ReleaseError};

/// Why a runner operation failed: [`fmt::Display`] is the failed step's own error, as the log has
/// it, and [`Error::text`] what the UI says.
pub enum Error {
    /// Getting a runner of `decanter_runtime` onto the machine.
    Prepare(Runner, anyhow::Error),
    /// A PS port, whose steps keep their own errors; `log` holds a build's whole output.
    Port {
        error: decanter_ps::runner::Error,
        log: Option<PathBuf>,
    },
    Confirm(Runner, io::Error),
    RemoveRelease(Runner, io::Error),
    RemoveBuild(Runner, io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare(_, err) => write!(f, "{err:#}"),
            Self::Port { error, .. } => error.fmt(f),
            Self::Confirm(_, err) | Self::RemoveRelease(_, err) | Self::RemoveBuild(_, err) => err.fmt(f),
        }
    }
}

impl Error {
    pub fn text(&self) -> Text {
        match self {
            Self::Prepare(Runner::Umu | Runner::Proton | Runner::GeProton, err) => {
                Text::new("ps.fsr.failures.proton_not_ready").arg("error", format!("{err:#}"))
            }
            Self::Prepare(Runner::Raven, err) => Text::new("raven.build.failed").arg("error", format!("{err:#}")),
            Self::Port { error, log } => Text::new("ps.branch.errors.failed")
                .text("error", port_error(error))
                .arg(
                    "log",
                    log.as_ref().map_or_else(String::new, |log| log.display().to_string()),
                ),
            Self::Confirm(Runner::Psport | Runner::AnyPs5, err) => {
                Text::new("ps.branch.errors.confirm").arg("error", err)
            }
            Self::RemoveRelease(Runner::Psport | Runner::AnyPs5, err) => {
                Text::new("ps.package.errors.delete").arg("error", err)
            }
            Self::RemoveBuild(Runner::Psport | Runner::AnyPs5, err) => {
                Text::new("ps.branch.errors.delete").arg("error", err)
            }
            Self::RemoveRelease(Runner::Raven, err) | Self::RemoveBuild(Runner::Raven, err) => {
                Text::new("raven.errors.remove_build").arg("error", err)
            }
            // Nothing in the UI prepares a port through `decanter_runtime` nor removes the runners
            // umu and decanter's FSR recording use, and a runner of `decanter_runtime` fails its
            // confirmation as its preparation, so no text was written for them.
            Self::Prepare(Runner::Psport | Runner::AnyPs5, _)
            | Self::Confirm(Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven, _)
            | Self::RemoveRelease(Runner::Umu | Runner::Proton | Runner::GeProton, _)
            | Self::RemoveBuild(Runner::Umu | Runner::Proton | Runner::GeProton, _) => Text::verbatim(self),
        }
    }
}

/// The newest release of a runner decanter follows upstream's releases of; `None` also for one it
/// only ever gets at its own pin (GE-Proton, Raven) or through its channel (the PS ports).
pub async fn latest(runner: Runner) -> Result<Option<Release>, ReleaseError> {
    match runner {
        Runner::Umu => releases::latest(decanter_runtime::UMU_LAUNCHER_REPO).await,
        Runner::Proton => releases::latest(decanter_runtime::UMU_PROTON_REPO).await,
        Runner::GeProton | Runner::Raven | Runner::Psport | Runner::AnyPs5 => Ok(None),
    }
}

pub const fn support(runner: Runner) -> Support {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => decanter_runtime::runner::support(runner),
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::support(runner),
    }
}

/// What is installed, read from disk only.
pub async fn status(root: &Root, runner: Runner) -> Status {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => decanter_runtime::runner::status(runner),
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::status(root, runner).await,
    }
}

/// Records the owner's yes to building `pin`, which [`prepare`] asks for before it builds a
/// [`Pin::Commit`] of a runner of `decanter_runtime`; a port's is recorded with the branch it was
/// listed on, so for a port this only checks it.
///
/// # Errors
///
/// Fails if this system cannot get the runner, `pin` is not the one decanter pins, or the
/// confirmation cannot be written or is not there.
pub async fn confirm(root: &Root, runner: Runner, pin: &Pin) -> Result<(), Error> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => {
            decanter_runtime::runner::confirm(runner, pin)
                .await
                .map_err(|err| Error::Prepare(runner, anyhow::Error::new(err).context("recording the confirmation")))
        }
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::confirm(root, runner, pin)
            .await
            .map_err(|err| Error::Confirm(runner, err)),
    }
}

/// Installs `pin` unless it is there already. `on_line` gets a port's build output as it comes;
/// the other runners keep theirs to their log.
///
/// # Errors
///
/// Fails if this system cannot get the runner or `pin` that way, or any step fails.
pub async fn prepare(
    root: &Root,
    runner: Runner,
    pin: &Pin,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => {
            decanter_runtime::runner::prepare(runner, pin, on_progress)
                .await
                .map_err(|err| Error::Prepare(runner, err))
        }
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::prepare(root, runner, pin, on_progress, on_line)
            .await
            .map_err(|error| Error::Port {
                error,
                log: decanter_ps::runner::log_path(root, runner, pin),
            }),
    }
}

/// Brings the runner to the version decanter wants, which for a pinned one is its pin rather than
/// the newest release.
///
/// # Errors
///
/// As [`prepare`], and when the releases cannot be listed.
pub async fn update(
    root: &Root,
    runner: Runner,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => {
            decanter_runtime::runner::update(runner, on_progress)
                .await
                .map_err(|err| Error::Prepare(runner, err))
        }
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::update(root, runner, on_progress, on_line)
            .await
            .map_err(|error| Error::Port { error, log: None }),
    }
}

/// Removes `pin`; one that is not there is already removed.
///
/// # Errors
///
/// Fails if this system cannot get the runner or the files cannot be removed.
pub async fn remove(root: &Root, runner: Runner, pin: &Pin) -> Result<(), Error> {
    let removed = match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => {
            decanter_runtime::runner::remove(runner, pin).await
        }
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::remove(root, runner, pin).await,
    };
    removed.map_err(|err| match pin {
        Pin::Release(_) => Error::RemoveRelease(runner, err),
        Pin::Commit(_) => Error::RemoveBuild(runner, err),
    })
}

/// The log of the last build of `pin`; `None` for a runner that is only ever downloaded.
pub fn log_path(root: &Root, runner: Runner, pin: &Pin) -> Option<PathBuf> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => {
            decanter_runtime::runner::log_path(runner, pin)
        }
        Runner::Psport | Runner::AnyPs5 => decanter_ps::runner::log_path(root, runner, pin),
    }
}

/// What a preparation of `runner` shows while it runs, and how far along its bar is, where it has
/// one.
pub fn progress(runner: Runner, progress: &Progress) -> (Cow<'static, str>, Option<f64>) {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton => (recorder_progress(runner, progress), None),
        Runner::Raven => raven_progress(progress),
        Runner::Psport | Runner::AnyPs5 => build_progress(progress),
    }
}

/// The FSR row's words for the runners decanter gets for the recording, named since it gets two.
fn recorder_progress(runner: Runner, progress: &Progress) -> Cow<'static, str> {
    let name = match runner {
        Runner::Umu => "umu-run",
        Runner::Proton => "Proton",
        Runner::GeProton => "GE-Proton",
        Runner::Raven => "Raven",
        Runner::Psport => "psport",
        Runner::AnyPs5 => "AnyPS5",
    };
    match (&progress.stage, progress.total) {
        (Stage::Downloading, Some(total)) => t!(
            "ps.extras.fsr.progress.runner_downloading",
            runner = name,
            received = crate::format::bytes(progress.received),
            total = crate::format::bytes(total)
        ),
        (Stage::Downloading, None) => t!(
            "ps.extras.fsr.progress.runner_downloading_size",
            runner = name,
            received = crate::format::bytes(progress.received)
        ),
        (Stage::Unpacking | Stage::Verifying, _) => t!("ps.extras.fsr.progress.runner_unpacking", runner = name),
        (Stage::Sources | Stage::Configuring | Stage::Compiling | Stage::Assembling, _) => {
            t!("ps.extras.fsr.progress.runner_building", runner = name)
        }
    }
}

fn raven_progress(progress: &Progress) -> (Cow<'static, str>, Option<f64>) {
    match (&progress.stage, progress.total) {
        (Stage::Sources, _) => (t!("raven.build.stage.sources"), None),
        (Stage::Downloading, Some(total)) => (
            t!(
                "raven.build.stage.downloading_total",
                received = crate::format::bytes(progress.received),
                total = crate::format::bytes(total)
            ),
            Some(progress.received as f64 / total.max(1) as f64),
        ),
        (Stage::Downloading, None) => (
            t!(
                "raven.build.stage.downloading",
                received = crate::format::bytes(progress.received)
            ),
            None,
        ),
        // Raven's build never verifies nor assembles anything: those read as the step nearest them.
        (Stage::Unpacking | Stage::Verifying, _) => (t!("raven.build.stage.unpacking"), None),
        (Stage::Configuring | Stage::Assembling, _) => (t!("raven.build.stage.configuring"), None),
        (Stage::Compiling, total) => (
            t!(
                "raven.build.stage.compiling",
                done = progress.received,
                total = total.unwrap_or_default()
            ),
            total.map(|total| progress.received as f64 / total.max(1) as f64),
        ),
    }
}

/// The branch card's words for a port's build, naming the piece of its toolchain it works on.
fn build_progress(progress: &Progress) -> (Cow<'static, str>, Option<f64>) {
    let name = progress.item.as_deref().unwrap_or_default();
    match progress.stage {
        Stage::Downloading => (
            t!(
                "ps.branch.stage.downloading",
                name = name,
                size = crate::format::bytes(progress.received)
            ),
            progress
                .total
                .map(|total| progress.received as f64 / total.max(1) as f64),
        ),
        Stage::Unpacking => (t!("ps.branch.stage.unpacking", name = name), None),
        Stage::Verifying => {
            let total = progress.total.unwrap_or_default();
            (
                t!("ps.branch.stage.verifying", checked = progress.received, total = total),
                (total > 0).then(|| progress.received as f64 / total as f64),
            )
        }
        Stage::Sources => (t!("ps.branch.stage.sources"), None),
        Stage::Configuring => (t!("ps.branch.stage.configuring"), None),
        Stage::Compiling => (t!("ps.branch.stage.compiling"), None),
        Stage::Assembling => (t!("ps.branch.stage.assembling"), None),
    }
}

/// Why preparing a port failed: a build's own errors as [`build_error`] words them.
fn port_error(err: &decanter_ps::runner::Error) -> Text {
    match err {
        decanter_ps::runner::Error::Build(err) => build_error(err),
        decanter_ps::runner::Error::NotAPort
        | decanter_ps::runner::Error::Unsupported(_)
        | decanter_ps::runner::Error::NotOffered(_)
        | decanter_ps::runner::Error::Package(_)
        | decanter_ps::runner::Error::AppJob => Text::new("ps.branch.errors.raw").arg("error", err),
    }
}

/// Why a psport build or the check of its branch failed, in the UI's language where the owner can
/// act on it.
pub fn build_error(err: &winbuild::Error) -> Text {
    match err {
        winbuild::Error::NotPinned => Text::new("ps.branch.errors.not_pinned"),
        winbuild::Error::Tampered(file) => Text::new("ps.branch.errors.tampered").arg("file", file),
        winbuild::Error::PathTooLong { path, length } => Text::new("ps.branch.errors.path_too_long")
            .arg("path", path)
            .arg("length", length)
            .arg("max", winbuild::MAX_CHECKOUT_PATH),
        winbuild::Error::Remote(_)
        | winbuild::Error::Git(_)
        | winbuild::Error::Rejected(_)
        | winbuild::Error::NotConfirmed(_)
        | winbuild::Error::Busy(_)
        | winbuild::Error::Download(_)
        | winbuild::Error::Archive(_)
        | winbuild::Error::Manifest(_)
        | winbuild::Error::Confinement(_)
        | winbuild::Error::Tool { .. }
        | winbuild::Error::TimedOut { .. }
        | winbuild::Error::TooManyProcesses { .. }
        | winbuild::Error::Output(_)
        | winbuild::Error::Package(_)
        | winbuild::Error::Io(_) => Text::new("ps.branch.errors.raw").arg("error", err),
    }
}
