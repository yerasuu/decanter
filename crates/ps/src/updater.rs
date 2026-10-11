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

/// The first time a port is needed. Into a sibling directory renamed into
/// place at the end, so an interrupted clone never looks like a checkout.
pub fn clone_command(port: Port) -> Command {
    let checkout = port.checkout();
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

/// The automatic update. `--ff-only` refuses rather than merges, and git
/// itself refuses when an incoming change touches a locally edited file,
/// so nothing local is lost. `branch` comes from a [`Check`].
pub fn fast_forward_command(repo: &Path, branch: &str) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(
            "set -e; git merge --ff-only --end-of-options \"origin/$1\"; \
             git submodule update --init --recursive; git log -1 --oneline",
        )
        .arg("update")
        .arg(branch)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

/// Where [`reset_command`] saves the local diff of `port`'s checkout.
pub fn backup_path(port: Port, head: &str) -> PathBuf {
    crate::paths::backups().join(format!("{}-{head}.patch", port.name().to_lowercase()))
}

/// Only ever started by an explicit user action: saves `git diff HEAD` to
/// `backup`, then puts the checkout on upstream exactly.
pub fn reset_command(repo: &Path, branch: &str, backup: &Path) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(
            "set -e; mkdir -p -- \"$(dirname -- \"$2\")\"; git diff HEAD > \"$2\"; \
             echo \"local changes saved to $2\"; git reset --hard --end-of-options \"origin/$1\"; \
             git submodule update --init --recursive; git log -1 --oneline",
        )
        .arg("reset")
        .arg(branch)
        .arg(backup)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultBranch=main",
                // The user's global config may sign commits; test repositories have no key.
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// An upstream repository and a clone of it, in a temporary folder.
    fn repos(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("decanter-ps-updater-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (upstream, clone) = (root.join("upstream"), root.join("clone"));
        std::fs::create_dir_all(&upstream).unwrap();
        run(&upstream, &["init", "-q"]);
        std::fs::write(upstream.join("a"), "1").unwrap();
        run(&upstream, &["add", "a"]);
        run(&upstream, &["commit", "-qm", "one"]);
        run(&root, &["clone", "-q", "upstream", "clone"]);
        (root, upstream, clone)
    }

    #[tokio::test]
    async fn a_moved_upstream_becomes_origin_and_fast_forwards() {
        let (root, _upstream, clone) = repos("moved");
        let fork = root.join("fork");
        run(&root, &["clone", "-q", "upstream", "fork"]);
        std::fs::write(fork.join("b"), "2").unwrap();
        run(&fork, &["add", "b"]);
        run(&fork, &["commit", "-qm", "fork"]);
        let found = check(&clone, fork.to_str().unwrap()).await.unwrap();
        assert_eq!((found.plan, found.behind), (Plan::FastForward, 1));
        assert_eq!(
            git(&clone, &["remote", "get-url", "origin"]).await.unwrap(),
            fork.to_str().unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn plans_fast_forward_then_up_to_date() {
        let (root, upstream, clone) = repos("ff");
        assert_eq!(
            check(&clone, upstream.to_str().unwrap()).await.unwrap().plan,
            Plan::UpToDate
        );
        std::fs::write(upstream.join("b"), "2").unwrap();
        run(&upstream, &["add", "b"]);
        run(&upstream, &["commit", "-qm", "two"]);
        let found = check(&clone, upstream.to_str().unwrap()).await.unwrap();
        assert_eq!(
            (found.plan, found.behind, found.branch.as_str()),
            (Plan::FastForward, 1, "main")
        );
        assert!(fast_forward_command(&clone, "main").status().await.unwrap().success());
        assert_eq!(
            check(&clone, upstream.to_str().unwrap()).await.unwrap().plan,
            Plan::UpToDate
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tells_rewritten_history_from_local_commits() {
        let (root, upstream, clone) = repos("rewrite");
        std::fs::write(upstream.join("b"), "2").unwrap();
        run(&upstream, &["add", "b"]);
        run(&upstream, &["commit", "-qm", "two"]);
        run(&clone, &["pull", "-q"]);
        // Upstream amends its last commit (same patch, new hash) and adds one.
        run(&upstream, &["commit", "-q", "--amend", "-m", "two, reworded"]);
        std::fs::write(upstream.join("c"), "3").unwrap();
        run(&upstream, &["add", "c"]);
        run(&upstream, &["commit", "-qm", "three"]);
        assert_eq!(
            check(&clone, upstream.to_str().unwrap()).await.unwrap().plan,
            Plan::Rewritten
        );
        std::fs::write(clone.join("mine"), "x").unwrap();
        run(&clone, &["add", "mine"]);
        run(&clone, &["commit", "-qm", "mine"]);
        assert_eq!(
            check(&clone, upstream.to_str().unwrap()).await.unwrap().plan,
            Plan::LocalCommits(1)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn branch_names_from_the_remote_are_held_to_branch_shapes() {
        assert!(valid_branch("main"));
        assert!(valid_branch("release/0.4"));
        assert!(!valid_branch("--upload-pack=x"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch("../x"));
    }
}
