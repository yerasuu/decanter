//! Getting the runners this crate owns onto the machine: the same functions
//! on every system, each answered by the system's own part under `os/`.
//!
//! psport and AnyPS5 are `decanter_ps`'s (`decanter_ps::runner`): asked here, they answer as the PS
//! builds' feature says and refuse everything else.
//!
//! Cancelling is dropping the future. A preparation installs nothing unless
//! every step before it passed, and is a no-op for what is already there. A
//! runner built from a [`Pin::Commit`] runs code nobody vetted for decanter,
//! so it is built only once that commit was [`confirm`]ed.

use std::io;
use std::path::PathBuf;

use anyhow::bail;

use decanter_core::capabilities::{Feature, Support};
use decanter_core::platform;
use decanter_core::runner::{Commit, Installed, Pin, Progress, Runner, Status};

use crate::Launch;
use crate::os;

/// Why Steam's Proton builds are only listed: Steam and umu own `compatibilitytools.d`.
const UMU_INSTALLS_PROTON: &str = "Proton builds are installed by umu at launch or by the user, not by decanter";
const PS_PREPARES_PORTS: &str = "psport and AnyPS5 are prepared by decanter-ps";

/// The commit of [mirkobrombin/raven](https://github.com/mirkobrombin/raven) decanter builds, the
/// one its build was tried at (2026-10-29); bumped only after a build at the new one was tried.
pub const RAVEN_COMMIT: &str = "3179949f14330f14407a41cc923dc902b5aa2bb8";

/// Everything of Raven's in the active library: its build, toolchain and
/// build log, and the prefix and manifest of each program it runs.
#[must_use]
#[inline]
pub fn raven_dir() -> Option<PathBuf> {
    decanter_core::bootstrap::data_dir().map(|data_dir| data_dir.join("runners").join("raven"))
}

/// Raven's [`RAVEN_COMMIT`] as a pin; `None` only if that constant were no commit id.
#[must_use]
#[inline]
pub fn raven_pin() -> Option<Pin> {
    Commit::try_new(RAVEN_COMMIT).map(Pin::Commit)
}

#[must_use]
#[inline]
pub const fn support(runner: Runner) -> Support {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton => platform::support(Feature::Proton),
        Runner::Raven => platform::support(Feature::Raven),
        Runner::Psport | Runner::AnyPs5 => platform::support(Feature::PsBuilds),
    }
}

/// What is installed, read from disk only.
#[must_use]
#[inline]
pub fn status(runner: Runner) -> Status {
    match runner {
        Runner::Umu => os::umu_status(),
        Runner::Proton => os::proton_status(),
        Runner::GeProton => os::ge_proton_status(),
        Runner::Raven => os::raven_status(),
        Runner::Psport | Runner::AnyPs5 => Status::Unsupported,
    }
}

/// Records the owner's yes to building `pin`, which [`prepare`] asks for
/// before it builds a [`Pin::Commit`]; a release, checked against its pin
/// when it is downloaded, needs none.
///
/// # Errors
///
/// Fails if this system cannot get the runner, `pin` is not the one decanter
/// pins, or the confirmation cannot be written.
#[inline]
pub async fn confirm(runner: Runner, pin: &Pin) -> io::Result<()> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton => Ok(()),
        Runner::Raven => os::raven_confirm(pin).await,
        Runner::Psport | Runner::AnyPs5 => Err(io::Error::new(io::ErrorKind::Unsupported, PS_PREPARES_PORTS)),
    }
}

/// Installs `pin` unless it is there already.
///
/// # Errors
///
/// Fails if this system cannot get the runner, `pin` is not one decanter
/// tested it at, or a download or install step fails.
#[inline]
pub async fn prepare(runner: Runner, pin: &Pin, on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<Installed> {
    match runner {
        Runner::Umu => os::umu_prepare(pin, on_progress).await,
        Runner::Proton => bail!(UMU_INSTALLS_PROTON),
        Runner::GeProton => os::ge_proton_prepare(pin, on_progress).await,
        Runner::Raven => os::raven_prepare(pin, on_progress).await,
        Runner::Psport | Runner::AnyPs5 => bail!(PS_PREPARES_PORTS),
    }
}

/// Brings the runner to the version decanter wants, which for a pinned one
/// is its pin rather than the newest release, and only then removes the
/// versions it replaces.
///
/// # Errors
///
/// As [`prepare`].
#[inline]
pub async fn update(runner: Runner, on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<Installed> {
    match runner {
        Runner::Umu => os::umu_update(on_progress).await,
        Runner::Proton => bail!(UMU_INSTALLS_PROTON),
        Runner::GeProton => os::ge_proton_update(on_progress).await,
        Runner::Raven => os::raven_update(on_progress).await,
        Runner::Psport | Runner::AnyPs5 => bail!(PS_PREPARES_PORTS),
    }
}

/// Removes `pin`; one that is not there is already removed. Raven goes as a
/// whole: its build, its toolchain, its build log and the confirmation.
///
/// # Errors
///
/// Fails if this system cannot get the runner or the files cannot be removed.
#[inline]
pub async fn remove(runner: Runner, pin: &Pin) -> io::Result<()> {
    match runner {
        Runner::Umu => os::umu_remove(pin).await,
        Runner::Proton => Err(io::Error::new(io::ErrorKind::Unsupported, UMU_INSTALLS_PROTON)),
        Runner::GeProton => os::ge_proton_remove(pin).await,
        Runner::Raven => os::raven_remove(pin).await,
        Runner::Psport | Runner::AnyPs5 => Err(io::Error::new(io::ErrorKind::Unsupported, PS_PREPARES_PORTS)),
    }
}

/// The log of the last build of `pin`; `None` for a runner that is only
/// ever downloaded, or where there is no library to keep it in.
#[must_use]
#[inline]
pub fn log_path(runner: Runner, pin: &Pin) -> Option<PathBuf> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Psport | Runner::AnyPs5 => None,
        Runner::Raven => os::raven_log_path(pin),
    }
}

/// `launch`, of a Windows program in its Raven prefix ([`Launch::prefix`]), turned into the
/// Linux program that runs it under Raven, inside the toolchain that built Raven, with its session
/// named after that prefix.
///
/// # Errors
///
/// Fails if this system has no Raven, none is built, `launch` has no prefix, or a path in it is
/// not UTF-8.
#[inline]
pub async fn raven_launch(launch: Launch) -> anyhow::Result<Launch> {
    os::raven_launch(launch).await
}
