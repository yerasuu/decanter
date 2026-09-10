//! End-to-end checks against a real `umu-run` and a real Proton prefix.
//!
//! Ignored by default: these need umu-launcher installed and will download
//! a Proton build and the Steam Linux Runtime into a fresh prefix, which
//! takes minutes and a couple of gigabytes. Run them deliberately:
//!
//! ```text
//! cargo test -p achv-runtime -- --ignored --nocapture
//! ```

use core::time::Duration;
use std::path::PathBuf;

use achv_runtime::{Launch, Prefix, Proton, start};
use tokio::time::sleep;

/// A prefix under the target directory, so a run leaves nothing in the
/// user's own `~/Games`.
#[expect(
    clippy::implicit_return,
    reason = "needless_return elsewhere already wants no explicit return"
)]
fn scratch_prefix(name: &str) -> Prefix {
    Prefix::new(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name))
}

/// Lays the prefix out if this is its first run, so the tests below have a
/// `cmd.exe` to point at.
#[expect(
    clippy::implicit_return,
    reason = "needless_return elsewhere already wants no explicit return"
)]
#[expect(
    clippy::large_stack_frames,
    reason = "Proton is inherently the largest local; not worth boxing in a test helper"
)]
async fn ensure_prefix(prefix: &Prefix) -> anyhow::Result<PathBuf> {
    let cmd = prefix.drive_c().join("windows/system32/cmd.exe");
    if !cmd.is_file()
        && let Err(err) = prefix.initialize(Proton::Latest).await
    {
        return Err(err);
    }
    assert!(cmd.is_file(), "prefix was never laid out: {}", cmd.display());
    Ok(cmd)
}

// tests_outside_test_module wants these in a `#[cfg(test)] mod`; inline_modules wants
// that module split into its own file, which is out of scope for this file-only change.
#[expect(
    clippy::inline_modules,
    reason = "tests_outside_test_module already requires this module inline here"
)]
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs umu-launcher and downloads Proton"]
    async fn launches_and_waits_for_a_process_that_exits_on_its_own() -> anyhow::Result<()> {
        let prefix = scratch_prefix("smoke-exit");
        let cmd = match ensure_prefix(&prefix).await {
            Ok(cmd) => cmd,
            Err(err) => return Err(err),
        };

        let session = match start(&prefix, Launch::new(cmd, "umu-achv-test").arg("/c").arg("exit")).await {
            Ok(session) => session,
            Err(err) => return Err(err),
        };

        if let Err(err) = session.wait().await {
            return Err(err);
        }
        if !prefix.processes().is_empty() {
            return Err(anyhow::anyhow!(
                "wait() returned while the prefix still had live processes"
            ));
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "needs umu-launcher and downloads Proton"]
    async fn sees_a_running_process_and_stops_it() -> anyhow::Result<()> {
        let prefix = scratch_prefix("smoke-stop");
        let cmd = match ensure_prefix(&prefix).await {
            Ok(cmd) => cmd,
            Err(err) => return Err(err),
        };

        // `/k` keeps cmd waiting on input that never comes — a stand-in for a
        // game that runs until something stops it.
        let session = match start(&prefix, Launch::new(cmd, "umu-achv-test").arg("/k")).await {
            Ok(session) => session,
            Err(err) => return Err(err),
        };

        // umu unpacks the runtime and starts Proton before anything of ours
        // shows up in the prefix, so this is a poll, not a fixed sleep.
        let mut pids = Vec::new();
        // separated_literal_suffix and unseparated_literal_suffix want opposite spellings; `0_u32` wins
        #[expect(
            clippy::separated_literal_suffix,
            reason = "matches this crate's other numeric-suffix literals"
        )]
        for _ in 0_u32..60 {
            pids = prefix.processes();
            if !pids.is_empty() {
                break;
            }
            sleep(Duration::from_secs(1)).await;
        }
        if pids.is_empty() {
            return Err(anyhow::anyhow!("no process was ever seen in the prefix"));
        }

        if let Err(err) = prefix.stop().await {
            return Err(err);
        }
        if !prefix.processes().is_empty() {
            return Err(anyhow::anyhow!("stop() left processes behind"));
        }
        drop(session);
        Ok(())
    }

    /// Closing the app has to stop what it started, and `stop_all` is what
    /// does it — so it has to reach a session nobody is holding a handle to.
    #[tokio::test]
    #[ignore = "needs umu-launcher and downloads Proton"]
    async fn stop_all_reaches_a_session_started_earlier() -> anyhow::Result<()> {
        let prefix = scratch_prefix("smoke-stop-all");
        let cmd = match ensure_prefix(&prefix).await {
            Ok(cmd) => cmd,
            Err(err) => return Err(err),
        };

        if !achv_runtime::live_prefixes().is_empty() {
            return Err(anyhow::anyhow!(
                "nothing should be registered before anything is launched"
            ));
        }

        // Deliberately dropped: shutdown can't rely on a live `Session`, since
        // the task that owned one is gone by the time the app is closing.
        let session = match start(&prefix, Launch::new(cmd, "umu-achv-test").arg("/k")).await {
            Ok(session) => session,
            Err(err) => return Err(err),
        };
        if achv_runtime::live_prefixes().len() != 1 {
            return Err(anyhow::anyhow!("expected exactly one live prefix"));
        }
        drop(session);

        // separated_literal_suffix and unseparated_literal_suffix want opposite spellings; `0_u32` wins
        #[expect(
            clippy::separated_literal_suffix,
            reason = "matches this crate's other numeric-suffix literals"
        )]
        for _ in 0_u32..60 {
            if !prefix.processes().is_empty() {
                break;
            }
            sleep(Duration::from_secs(1)).await;
        }
        if prefix.processes().is_empty() {
            return Err(anyhow::anyhow!("nothing ever started"));
        }

        achv_runtime::stop_all().await;
        if !prefix.processes().is_empty() {
            return Err(anyhow::anyhow!("stop_all left a game running"));
        }
        if !achv_runtime::live_prefixes().is_empty() {
            return Err(anyhow::anyhow!("a stopped session should not still be registered"));
        }
        Ok(())
    }
}
