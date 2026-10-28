//! Running games.
//!
//! On Windows and macOS they run as the plain programs they are;
//! on Linux, Windows games run under Proton through `umu-run`, which the
//! rest of this explains.
//!
//! umu is the launcher Valve's own Proton expects around it: it resolves
//! (and downloads) a Proton build, sets up the Steam Linux Runtime
//! container, and applies protonfixes for a known game id. decanter drives
//! it as a plain subprocess — there is no daemon, no agent inside the
//! prefix, and no state of umu's that decanter has to keep in sync. See
//! `os/linux/umu.rs` for why `umu-run` itself is fetched by decanter rather
//! than resolved off `PATH`.
//!
//! What that buys over talking to a prefix directly: one process to spawn
//! and one to wait on. Exit detection is the child's own exit plus a sweep
//! for anything it left behind (see [`Prefix::processes`]), rather than
//! polling an in-prefix service.
//!
//! The interface is the same on every system: a [`Launch`] describes what
//! to run, [`prepare`] and [`start`] run it as a [`Session`], and [`stop`],
//! [`end`], [`is_live`], [`stop_all`] and [`take_killed`] reach it by its
//! key. How each step is done is the system's own, under `os/`, which picks
//! one by `cfg` and is the only place that does; what a system has no use
//! for (a prefix or a Proton build off Linux) is plain data it ignores, and
//! what it has nothing of (a prefix's processes, Steam's Proton builds)
//! comes back empty.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "most async fns here cost more than the 512-byte threshold by design"
)]

mod launch;
mod os;
mod path;
mod prefix;
mod proton;
mod session;
mod umu;

pub use launch::Launch;
pub use path::sanitize_relative_path;
pub use prefix::{Prefix, live_prefixes, processes_by_prefix};
pub use proton::{Proton, UMU_PROTON_REPO, steam_compat_dir, steam_runner_exists, steam_runners};
pub use session::{Prepared, Session, end, is_live, prepare, start, stop, stop_all, take_killed};
pub use umu::{UMU_LAUNCHER_REPO, UMU_LAUNCHER_VERSION, umu_run_path};

use decanter_core::capabilities::Feature;
use decanter_core::platform;

/// Exit code decanter records for a run it killed on a stop request, because the dead process' own
/// status is 0 after a Job Object kill and reads like a clean exit. `0xDEC0` marks decanter in the
/// high half, which no psport code (139 and the like) and no Windows status (`0xC000_xxxx` NTSTATUS,
/// `0x8007_xxxx` HRESULT) uses, and the low half is decanter's own kind of exit (1: killed on stop).
pub const KILLED_EXIT_CODE: u32 = 0xDEC0_0001;

/// Whether games can launch here at all, as the capability table says:
/// elsewhere every launch fails up front and the UI doesn't offer it.
#[must_use]
#[inline]
pub fn available() -> bool {
    platform::support(Feature::Play).is_offered()
}
