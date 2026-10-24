//! macOS: a session is the process group the game starts as the leader of,
//! and a stop is a signal to that group.
//!
//! Neither Job Objects nor cgroups exist here, so a process that leaves the
//! group (`setsid`, `setpgid`) leaves the session too, the same gap
//! crates/ps's jobs have on this system.

use std::path::Path;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::process::{Child, Command};

use super::Remaining;

/// init's pid, the highest a `killpg` group must stay above.
const INIT_PID: i32 = 1;

/// A process group decanter may signal. Never 0 or 1, since `killpg` reads
/// 0 as decanter's own group and leaves 1 or less undefined
/// ([POSIX `killpg`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/killpg.html)).
#[derive(Clone, Copy)]
pub(super) struct Group(Pid);

impl Group {
    /// The only way to a [`Group`], so no stop can reach decanter's own
    /// group or every process.
    fn led_by(pid: u32) -> Option<Self> {
        i32::try_from(pid)
            .ok()
            .filter(|&raw| raw > INIT_PID)
            .map(|raw| Self(Pid::from_raw(raw)))
    }
}

/// What a live session's processes are found by.
pub(super) struct Tracked {
    /// The game's process group, whose id is the pid of the process
    /// decanter started, since [`command`] makes it the group's leader.
    /// `None` for a pid no group may be signalled by: the session then
    /// ends with that process and a stop sends nothing.
    group: Option<Group>,
}

/// The process group a stop signals.
pub(super) type Targets = Option<Group>;

impl Tracked {
    /// Follows the group `pid` leads, which [`command`] set up before the spawn.
    pub(super) fn new(_: &Child, pid: u32, key: &str) -> Self {
        let group = Group::led_by(pid);
        if group.is_none() {
            tracing::warn!(
                key,
                pid,
                "spawned pid cannot name a process group to signal; the session is not followed"
            );
        }
        Self { group }
    }

    /// Whether anything is left in the group once the process decanter
    /// started has exited: signal 0 checks for members without sending
    /// anything ([POSIX `kill`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/kill.html)).
    /// No count, since nothing short of listing every process says how many.
    pub(super) fn remaining(&self, key: &str) -> Remaining {
        let Some(Group(group)) = self.group else {
            return Remaining::Done;
        };
        match killpg(group, None) {
            Ok(()) => Remaining::Running(None),
            Err(Errno::ESRCH) => Remaining::Done,
            Err(err) => {
                tracing::warn!(key, group = group.as_raw(), %err, "cannot check the session's process group; ending the session");
                Remaining::Done
            }
        }
    }

    /// The whole group, whoever in it is still running.
    pub(super) const fn targets(&self) -> Targets {
        self.group
    }
}

/// The game's executable as a command that starts a process group of its
/// own, so a stop reaches what the game starts and nothing of decanter's
/// ([`Command::process_group`](https://docs.rs/tokio/1.53.2/tokio/process/struct.Command.html#method.process_group)).
pub(super) fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.process_group(0);
    command
}

/// SIGTERM to the whole group, so the game can save and quit on its own,
/// or SIGKILL when `force`d. Racy by nature: the group may already be gone,
/// which is the outcome being asked for anyway, so a failure is traced
/// rather than warned about.
#[expect(
    clippy::unused_async,
    reason = "the same signature as Windows' close, which waits for taskkill"
)]
pub(super) async fn close(targets: Targets, force: bool) {
    let Some(Group(group)) = targets else {
        tracing::debug!("no process group to signal");
        return;
    };
    let signal = if force { Signal::SIGKILL } else { Signal::SIGTERM };
    let sent = killpg(group, signal);
    tracing::trace!(group = group.as_raw(), %signal, ?sent, "signalled the session's process group");
}
