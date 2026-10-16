//! Running Windows games under Proton, through `umu-run`.
//!
//! umu is the launcher Valve's own Proton expects around it: it resolves
//! (and downloads) a Proton build, sets up the Steam Linux Runtime
//! container, and applies protonfixes for a known game id. decanter drives
//! it as a plain subprocess — there is no daemon, no agent inside the
//! prefix, and no state of umu's that decanter has to keep in sync. See
//! [`umu`] for why `umu-run` itself is fetched by decanter rather than
//! resolved off `PATH`.
//!
//! What that buys over talking to a prefix directly: one process to spawn
//! and one to wait on. Exit detection is the child's own exit plus a sweep
//! for anything it left behind (see [`Prefix::processes`]), rather than
//! polling an in-prefix service.
//!
//! Split by concern: [`prefix`] (a prefix and which ones are live),
//! [`proton`] (which build to run, and finding Steam's own), [`launch`]
//! (one game launch, spawning it, and the running result), [`umu`]
//! (fetching decanter's own `umu-run`).

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "most async fns here cost more than the 512-byte threshold by design"
)]

mod launch;
mod prefix;
mod proton;
mod umu;

pub use launch::{Launch, Prepared, Session, prepare, start};
pub use prefix::{Prefix, live_prefixes, processes_by_prefix, sanitize_relative_path, stop_all};
pub use proton::{Proton, UMU_PROTON_REPO, steam_compat_dir, steam_runner_exists, steam_runners};
pub use umu::{UMU_LAUNCHER_REPO, UMU_LAUNCHER_VERSION, umu_run_path};
