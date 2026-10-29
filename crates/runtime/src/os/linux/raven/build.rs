//! The steps of a Raven build: a shallow clone at the commit being built, then CMake and Ninja
//! inside the toolchain, every line of their output in the build log. Each tool runs in a process
//! group of its own, killed with it when the build's future is dropped. And the one question asked
//! of Raven's repository outside a build: its default branch's newest commit.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, bail};
use decanter_core::runner::{Commit, Progress, Stage};
use decanter_nix::{Recipe, Toolchain};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{ChildStderr, ChildStdout, Command};

use crate::runner::RavenHead;

const REPOSITORY: &str = "https://github.com/mirkobrombin/raven.git";

/// How long the remote may take to list its refs before the check gives up, since a stalled
/// connection would otherwise keep the section's check spinning.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(60);

/// Only the runtime: Raven's default build also builds its test programs, and nixpkgs' MinGW GCC
/// 16.2.0 stops on one of them (`native/vulkan-fixtures/client.exe`) with an internal compiler
/// error. `-k 0` so one failing step still shows every other one in the log.
const COMPILE: &str = "set -e; \
     cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo; \
     ninja -C build -k 0 native/raven";

/// The build's log, written as the build goes; a line it can't take is said once in decanter's own
/// log and the build goes on, since the log is only there to read.
pub(super) struct Log {
    file: File,
    is_broken: bool,
}

impl Log {
    pub(super) async fn create(path: &Path) -> anyhow::Result<Self> {
        let file = File::create(path)
            .await
            .with_context(|| format!("creating Raven's build log {}", path.display()))?;
        Ok(Self { file, is_broken: false })
    }

    pub(super) async fn line(&mut self, line: &str) {
        if self.is_broken {
            return;
        }
        let written = match self.file.write_all(line.as_bytes()).await {
            Ok(()) => self.file.write_all(b"\n").await,
            Err(err) => Err(err),
        };
        if let Err(err) = written {
            tracing::warn!(%err, "cannot write Raven's build log; the build goes on without it");
            self.is_broken = true;
        }
    }
}

/// `commit` alone, checked out at `checkout` and verified to be what git checked out.
pub(super) async fn clone(checkout: &Path, commit: &Commit, log: &mut Log) -> anyhow::Result<()> {
    let target = checkout.as_os_str();
    run(
        git(&["init", "--quiet", "--end-of-options"]).arg(target),
        "git init",
        log,
        |_| {},
    )
    .await?;
    run(
        git(&["-C"])
            .arg(target)
            .args([
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "--no-tags",
                "--end-of-options",
                REPOSITORY,
            ])
            .arg(commit.as_str()),
        "git fetch",
        log,
        |_| {},
    )
    .await?;
    run(
        git(&["-C"])
            .arg(target)
            .args(["checkout", "--quiet", "--detach", "FETCH_HEAD"]),
        "git checkout",
        log,
        |_| {},
    )
    .await?;
    let output = git(&["-C"])
        .arg(target)
        .args(["rev-parse", "--verify", "--end-of-options", "HEAD"])
        .stdin(Stdio::null())
        .output()
        .await
        .context("starting git rev-parse")?;
    let head = String::from_utf8_lossy(&output.stdout);
    let head = head.trim();
    log.line(&format!("checked out {head}")).await;
    if !output.status.success() || head != commit.as_str() {
        bail!("the checkout is at {head:?}, not {commit}");
    }
    Ok(())
}

/// What `git ls-remote --symref` lists for `HEAD`: `ref: refs/heads/<branch>\tHEAD` and
/// `<commit>\tHEAD`.
pub(super) async fn head() -> anyhow::Result<RavenHead> {
    let output = tokio::time::timeout(
        NETWORK_TIMEOUT,
        git(&["ls-remote", "--symref", "--end-of-options", REPOSITORY, "HEAD"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Raven's repository did not answer in time")?
    .context("starting git ls-remote")?;
    if !output.status.success() {
        bail!(
            "git ls-remote failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    head_of(&String::from_utf8_lossy(&output.stdout))
}

/// The remote's answer goes into the UI and, once built, into a folder name and git's arguments,
/// so both parts are held to what they must look like here.
fn head_of(refs: &str) -> anyhow::Result<RavenHead> {
    let (mut branch, mut listed) = (None, None);
    for line in refs.lines() {
        if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
            branch = rest.strip_suffix("\tHEAD");
        } else if let Some(commit) = line.strip_suffix("\tHEAD") {
            listed = Some(commit);
        }
    }
    let shown = |text: &str| text.chars().take(80).collect::<String>();
    let branch = match branch {
        Some(branch) if is_branch_name(branch) => branch.to_owned(),
        Some(branch) => bail!("a branch name decanter doesn't take: {:?}", shown(branch)),
        None => bail!("Raven's repository names no default branch"),
    };
    let Some(listed) = listed else {
        bail!("Raven's repository lists no commit for {branch}");
    };
    match Commit::try_new(listed) {
        Some(commit) => Ok(RavenHead { branch, commit }),
        None => bail!("not a full commit for {branch}: {:?}", shown(listed)),
    }
}

fn is_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
}

fn git(args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.args(args).env("GIT_TERMINAL_PROMPT", "0");
    // The AppImage's own library path would make the host's git load its libraries.
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

/// Configures and compiles the runtime, its steps counted as Ninja counts them.
pub(super) async fn compile(
    toolchain: &Toolchain,
    toolchain_dir: &Path,
    recipe: &Recipe,
    checkout: &Path,
    log: &mut Log,
    on_progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<()> {
    on_progress(Progress {
        stage: Stage::Configuring,
        received: 0,
        total: None,
        item: None,
    });
    let mut command = toolchain
        .command(toolchain_dir, recipe, checkout, COMPILE)
        .await
        .context("entering Raven's toolchain")?;
    run(&mut command, "Raven's build", log, |line| {
        if let Some((received, total)) = ninja_step(line) {
            on_progress(Progress {
                stage: Stage::Compiling,
                received,
                total: Some(total),
                item: None,
            });
        }
    })
    .await
}

/// `[12/345]`, as Ninja starts each line of its progress.
fn ninja_step(line: &str) -> Option<(u64, u64)> {
    let (counts, _) = line.strip_prefix('[')?.split_once(']')?;
    let (done, total) = counts.split_once('/')?;
    Some((done.parse().ok()?, total.parse().ok()?))
}

/// Runs `command` to its end with its output in `log` and handed to `on_line`; fails unless it
/// exits with success.
async fn run(command: &mut Command, what: &str, log: &mut Log, mut on_line: impl FnMut(&str)) -> anyhow::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    log.line(&format!("$ {what}")).await;
    let mut child = command.spawn().with_context(|| format!("starting {what}"))?;
    let group = Group(child.id().and_then(|pid| i32::try_from(pid).ok()).map(Pid::from_raw));
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        bail!("{what} has no output to follow");
    };
    let mut output = Output {
        stdout: Some(BufReader::new(stdout).lines()),
        stderr: Some(BufReader::new(stderr).lines()),
    };
    while let Some(line) = output.next().await {
        log.line(&line).await;
        on_line(&line);
    }
    let status = child.wait().await.with_context(|| format!("waiting for {what}"))?;
    group.release();
    log.line(&format!("{what} exited: {status}")).await;
    if !status.success() {
        bail!("{what} failed ({status}); its output is in Raven's build log");
    }
    Ok(())
}

/// Both of a tool's streams, read line by line as either has one.
struct Output {
    stdout: Option<Lines<BufReader<ChildStdout>>>,
    stderr: Option<Lines<BufReader<ChildStderr>>>,
}

impl Output {
    /// The next line of either stream; `None` once both ended. A stream that can't be read ends.
    async fn next(&mut self) -> Option<String> {
        loop {
            let (is_stdout, read) = match (self.stdout.as_mut(), self.stderr.as_mut()) {
                (None, None) => return None,
                (Some(stdout), None) => (true, stdout.next_line().await),
                (None, Some(stderr)) => (false, stderr.next_line().await),
                (Some(stdout), Some(stderr)) => tokio::select! {
                    read = stdout.next_line() => (true, read),
                    read = stderr.next_line() => (false, read),
                },
            };
            match read {
                Ok(Some(line)) => return Some(line),
                Ok(None) | Err(_) => {
                    if is_stdout {
                        self.stdout = None;
                    } else {
                        self.stderr = None;
                    }
                }
            }
        }
    }
}

/// A tool's process group, killed when the build is dropped before the tool ended: what nix-shell,
/// CMake and Ninja started goes with them.
struct Group(Option<Pid>);

impl Group {
    /// After the tool exited by itself: nothing of its group is left to end.
    fn release(mut self) {
        self.0 = None;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(group) = self.0.take()
            && let Err(err) = killpg(group, Signal::SIGKILL)
        {
            tracing::debug!(pgid = %group, %err, "Raven's build tools were already gone");
        }
    }
}
