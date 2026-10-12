//! Keeps each port checkout on the newest commit of its upstream default
//! branch, cloning it the first time.
//!
//! Only clones and fast-forwards happen on their own. A checkout whose
//! history upstream rewrote, or that carries local edits git cannot carry
//! over, waits for the user, and the reset offered then saves the local
//! diff first.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::paths::Root;
use crate::port::Port;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    UpToDate,
    /// Upstream only added commits: `git merge --ff-only` reaches it.
    FastForward,
    /// Upstream rewrote history; every local commit has an equivalent
    /// upstream (same patch), so a reset loses nothing committed.
    Rewritten,
    /// Local commits upstream lacks: never touched automatically.
    LocalCommits(usize),
}

#[derive(Debug, Clone)]
pub struct Check {
    pub branch: String,
    pub head: String,
    pub upstream: String,
    /// Upstream commits missing locally.
    pub behind: usize,
    /// Tracked files with uncommitted changes.
    pub dirty: Vec<String>,
    pub plan: Plan,
}

fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

async fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = match git_command(repo).args(args).output().await {
        Ok(output) => output,
        Err(err) => return Err(format!("git: {err}")),
    };
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The branch name comes from the remote and ends up in git arguments and
/// a shell script's parameters, so it is held to what branch names look
/// like in practice before anything uses it.
fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
}

/// Fetches and compares with `upstream`, pointing `origin` at it first: a
/// checkout cloned before a port moved (bbport to decanter's fork) then
/// fast-forwards onto it like any other update.
pub async fn check(repo: &Path, upstream: &str) -> Result<Check, String> {
    match git(repo, &["remote", "get-url", "origin"]).await {
        Ok(current) if current == upstream => {}
        Ok(_) => {
            if let Err(err) = git(repo, &["remote", "set-url", "origin", upstream]).await {
                return Err(err);
            }
        }
        Err(err) => return Err(err),
    }
    if let Err(err) = git(repo, &["fetch", "--quiet", "origin"]).await {
        return Err(err);
    }
    let symref = match git(repo, &["ls-remote", "--symref", "origin", "HEAD"]).await {
        Ok(symref) => symref,
        Err(err) => return Err(err),
    };
    let Some(branch) = symref
        .lines()
        .find_map(|line| line.strip_prefix("ref: refs/heads/"))
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|branch| valid_branch(branch))
        .map(str::to_owned)
    else {
        return Err("origin has no usable default branch".to_owned());
    };
    let remote = format!("origin/{branch}");
    let (head, upstream, behind, dirty, cherry) = match (
        git(repo, &["rev-parse", "--short", "HEAD"]).await,
        git(repo, &["rev-parse", "--short", "--end-of-options", &remote]).await,
        git(
            repo,
            &["rev-list", "--count", "--end-of-options", &format!("HEAD..{remote}")],
        )
        .await,
        git(
            repo,
            &[
                "status",
                "--porcelain",
                "--untracked-files=no",
                "--ignore-submodules=dirty",
            ],
        )
        .await,
        git(repo, &["cherry", "--end-of-options", &remote, "HEAD"]).await,
    ) {
        (Ok(head), Ok(upstream), Ok(behind), Ok(dirty), Ok(cherry)) => (head, upstream, behind, dirty, cherry),
        (Err(err), ..) | (_, Err(err), ..) | (_, _, Err(err), ..) | (.., Err(err), _) | (.., Err(err)) => {
            return Err(err);
        }
    };
    let behind = behind.parse().unwrap_or(0);
    let dirty = dirty
        .lines()
        .map(|line| line.get(3..).unwrap_or(line).to_owned())
        .collect();
    let contained = git_command(repo)
        .args(["merge-base", "--is-ancestor", "HEAD", &remote])
        .status()
        .await
        .is_ok_and(|status| status.success());
    // `git cherry` marks local commits whose patch upstream lacks with `+`.
    let unique = cherry.lines().filter(|line| line.starts_with('+')).count();
    let plan = if behind == 0 {
        Plan::UpToDate
    } else if contained {
        Plan::FastForward
    } else if unique == 0 {
        Plan::Rewritten
    } else {
        Plan::LocalCommits(unique)
    };
    Ok(Check {
        branch,
        head,
        upstream,
        behind,
        dirty,
        plan,
    })
}

/// The full commit the checkout stands on when it is exactly one upstream
/// has, without local edits to tracked files: the only checkouts a build
/// published by upstream's CI stands for. Validated, since it goes into a
/// URL.
pub async fn upstream_commit(repo: &Path) -> Option<String> {
    let (head, dirty, upstream) = match (
        git(repo, &["rev-parse", "--verify", "HEAD"]).await,
        git(
            repo,
            &[
                "status",
                "--porcelain",
                "--untracked-files=no",
                "--ignore-submodules=dirty",
            ],
        )
        .await,
        git(
            repo,
            &[
                "for-each-ref",
                "--contains",
                "HEAD",
                "--format=%(refname)",
                "refs/remotes/origin/",
            ],
        )
        .await,
    ) {
        (Ok(head), Ok(dirty), Ok(upstream)) => (head, dirty, upstream),
        (Err(err), ..) | (_, Err(err), _) | (.., Err(err)) => {
            tracing::warn!(repo = %repo.display(), %err, "could not tell which upstream commit the checkout is on");
            return None;
        }
    };
    if !dirty.is_empty() {
        tracing::info!(repo = %repo.display(), "checkout has local changes");
        return None;
    }
    if upstream.is_empty() {
        tracing::info!(repo = %repo.display(), head, "checkout is on a commit upstream lacks");
        return None;
    }
    crate::prebuilt::valid_commit(&head).then_some(head)
}

/// The first time a port is needed. Into a sibling directory renamed into
/// place at the end, so an interrupted clone never looks like a checkout.
pub fn clone_command(root: &Root, port: Port) -> Command {
    let checkout = port.checkout(root);
    let partial = checkout.with_extension("cloning");
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(
            "set -e; rm -rf -- \"$2\"; mkdir -p -- \"$(dirname -- \"$2\")\"; \
             git clone --recursive --end-of-options \"$1\" \"$2\"; mv -T -- \"$2\" \"$3\"",
        )
        .arg("clone")
        .arg(port.upstream())
        .arg(partial)
        .arg(checkout)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

/// Deinitializes the submodules that `origin/$1` tracks as plain files instead, since git won't
/// check files out over a submodule's checkout. Their checkout is discarded: upstream's tree now
/// carries that content.
const DROP_FORMER_SUBMODULES: &str = "git ls-files --stage | while read -r mode _ _ path; do \
     [ \"$mode\" = 160000 ] || continue; \
     git ls-tree --end-of-options \"origin/$1\" \"$path\" | grep -q '^160000 ' && continue; \
     git submodule deinit -f -- \"$path\"; done";

/// The automatic update. `--ff-only` refuses rather than merges, and git
/// itself refuses when an incoming change touches a locally edited file,
/// so nothing local is lost, except the checkout of a submodule upstream
/// turned into plain files ([`DROP_FORMER_SUBMODULES`]). `branch` comes from a [`Check`].
pub fn fast_forward_command(repo: &Path, branch: &str) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "set -e; {DROP_FORMER_SUBMODULES}; git merge --ff-only --end-of-options \"origin/$1\"; \
             git submodule update --init --recursive; git log -1 --oneline"
        ))
        .arg("update")
        .arg(branch)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

/// Where [`reset_command`] saves the local diff of `port`'s checkout.
pub fn backup_path(root: &Root, port: Port, head: &str) -> PathBuf {
    root.backups()
        .join(format!("{}-{head}.patch", port.name().to_lowercase()))
}

/// Only ever started by an explicit user action: saves `git diff HEAD` to
/// `backup`, then puts the checkout on upstream exactly.
pub fn reset_command(repo: &Path, branch: &str, backup: &Path) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "set -e; mkdir -p -- \"$(dirname -- \"$2\")\"; git diff HEAD > \"$2\"; \
             echo \"local changes saved to $2\"; {DROP_FORMER_SUBMODULES}; \
             git reset --hard --end-of-options \"origin/$1\"; \
             git submodule update --init --recursive; git log -1 --oneline"
        ))
        .arg("reset")
        .arg(branch)
        .arg(backup)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}
