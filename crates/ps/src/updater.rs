//! Keeps each port checkout on the newest commit of upstream's `develop`, or
//! its default branch when it has none, or on its latest release
//! ([`crate::channel`]), cloning it the first time.
//!
//! Clones, fast-forwards and resets that lose nothing happen on their own. A
//! checkout with commits upstream never published, or with local edits git
//! cannot carry over, waits for the user, and the reset offered then saves
//! the local diff first.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::paths::Root;
use crate::port::Port;
use crate::prebuilt::Release;
use crate::remote::RemoteError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    UpToDate,
    /// Upstream only added commits, or the checkout is on another branch
    /// than the one followed: `git merge --ff-only` reaches it.
    FastForward,
    /// Upstream rewrote history, but some ref it publishes (a branch, or an
    /// archive tag) still holds every local commit, so a reset loses nothing
    /// committed.
    Rewritten,
    /// Commits no upstream ref holds: never touched automatically.
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
    // English so `RemoteError::from_git` can read why the remote failed; no
    // prompt, since a vanished repository asks for credentials nobody can type.
    command
        .arg("-C")
        .arg(repo)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
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

/// Points `origin` at `upstream` first: a checkout cloned before a port
/// moved (bbport to psport) then follows it like any other.
async fn fetch_from(repo: &Path, upstream: &str, args: &[&str]) -> Result<(), String> {
    match git(repo, &["remote", "get-url", "origin"]).await {
        Ok(current) if current == upstream => {}
        Ok(_) => {
            if let Err(err) = git(repo, &["remote", "set-url", "origin", upstream]).await {
                return Err(err);
            }
        }
        Err(err) => return Err(err),
    }
    let mut fetch = vec!["fetch", "--quiet"];
    fetch.extend_from_slice(args);
    fetch.push("origin");
    git(repo, &fetch).await.map(drop)
}

/// Fetches `upstream` with its tags and compares the checkout with its
/// `develop`, or its default branch when it has none.
pub async fn check(repo: &Path, upstream: &str) -> Result<Check, RemoteError> {
    // Forced for the reason given in `on_release`; the tags are needed here
    // because an archive tag may be the only ref still holding the commits of
    // a rewritten branch.
    if let Err(err) = fetch_from(repo, upstream, &["--tags", "--force"]).await {
        return Err(RemoteError::from_git(err));
    }
    let refs = match git(repo, &["ls-remote", "--symref", "origin"]).await {
        Ok(refs) => refs,
        Err(err) => return Err(RemoteError::from_git(err)),
    };
    let has_develop = refs
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("refs/heads/develop"));
    let Some(branch) = has_develop
        .then_some("develop")
        .or_else(|| {
            refs.lines()
                .find_map(|line| line.strip_prefix("ref: refs/heads/"))
                .filter(|rest| rest.ends_with("\tHEAD"))
                .and_then(|rest| rest.split_whitespace().next())
        })
        .filter(|branch| valid_branch(branch))
        .map(str::to_owned)
    else {
        return Err(RemoteError::Failed("origin has no usable default branch".to_owned()));
    };
    let mut rev_list = vec!["rev-list", "--count", "--ignore-missing", "HEAD", "--not"];
    rev_list.extend(
        refs.lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|&(commit, name)| {
                (name.starts_with("refs/heads/") || name.starts_with("refs/tags/"))
                    && crate::prebuilt::valid_commit(commit)
            })
            .map(|(commit, _)| commit),
    );
    let remote = format!("origin/{branch}");
    let (head, upstream, behind, dirty, unpublished, current) = match (
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
        git(repo, &rev_list).await,
        git(repo, &["branch", "--show-current"]).await,
    ) {
        (Ok(head), Ok(upstream), Ok(behind), Ok(dirty), Ok(unpublished), Ok(current)) => {
            (head, upstream, behind, dirty, unpublished, current)
        }
        (Err(err), ..)
        | (_, Err(err), ..)
        | (_, _, Err(err), ..)
        | (_, _, _, Err(err), ..)
        | (.., Err(err), _)
        | (.., Err(err)) => {
            return Err(RemoteError::Failed(err));
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
    let unpublished = unpublished.parse().unwrap_or(0);
    let plan = if behind == 0 && current == branch {
        Plan::UpToDate
    } else if contained {
        Plan::FastForward
    } else if unpublished == 0 {
        Plan::Rewritten
    } else {
        Plan::LocalCommits(unpublished)
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

/// The highest `vX.Y.Z` tag on `commit`, a full one [`upstream_commit`] vouched for: the release
/// whose bundle stands for the checkout. From the checkout's own tags, so it works offline.
pub async fn release_tag(repo: &Path, commit: &str) -> Option<String> {
    let tags = match git(repo, &["tag", "--list", "--points-at", commit, "v*"]).await {
        Ok(tags) => tags,
        Err(err) => {
            tracing::warn!(repo = %repo.display(), commit, %err, "could not list the tags on the checkout's commit");
            return None;
        }
    };
    let version = |tag: &str| -> Option<(u64, u64, u64)> {
        let mut parts = tag.strip_prefix('v')?.split('.').map(str::parse);
        match (parts.next(), parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) => Some((major, minor, patch)),
            _ => None,
        }
    };
    tags.lines()
        .filter(|tag| crate::prebuilt::valid_tag(tag))
        .max_by_key(|tag| version(tag))
        .map(str::to_owned)
}

/// Fetches `upstream`'s tags, forced since a tag upstream deleted and pushed
/// again would otherwise fail every fetch, and tells whether the checkout
/// already stands on `release`'s commit. The tag must name that commit here
/// too, so a pointer and a tag that disagree never move the checkout.
pub async fn on_release(repo: &Path, upstream: &str, release: &Release) -> Result<bool, RemoteError> {
    if let Err(err) = fetch_from(repo, upstream, &["--tags", "--force"]).await {
        return Err(RemoteError::from_git(err));
    }
    let tag_ref = format!("refs/tags/{}^{{commit}}", release.tag);
    let (tagged, head) = match (
        git(
            repo,
            &["rev-parse", "--verify", "--quiet", "--end-of-options", &tag_ref],
        )
        .await,
        git(repo, &["rev-parse", "--verify", "HEAD"]).await,
    ) {
        (Ok(tagged), Ok(head)) => (tagged, head),
        // `--quiet` leaves nothing else to tell: the tag isn't there.
        (Err(_), _) => return Err(RemoteError::Failed(format!("upstream has no tag {}", release.tag))),
        (_, Err(err)) => return Err(RemoteError::Failed(err)),
    };
    if tagged != release.commit {
        return Err(RemoteError::Failed(format!(
            "tag {} is {tagged} upstream, not the published {}",
            release.tag, release.commit
        )));
    }
    Ok(head == release.commit)
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

/// Deinitializes the submodules that the revision `$1` tracks as plain files instead, since git won't
/// check files out over a submodule's checkout. Their checkout is discarded: upstream's tree now
/// carries that content.
const DROP_FORMER_SUBMODULES: &str = "git ls-files --stage | while read -r mode _ _ path; do \
     [ \"$mode\" = 160000 ] || continue; \
     git ls-tree --end-of-options \"$1\" \"$path\" | grep -q '^160000 ' && continue; \
     git submodule deinit -f -- \"$path\"; done";

/// The automatic update. `--ff-only` refuses rather than merges, and git
/// itself refuses when an incoming change touches a locally edited file,
/// so nothing local is lost, except the checkout of a submodule upstream
/// turned into plain files ([`DROP_FORMER_SUBMODULES`]). `branch` comes from a [`Check`].
/// A checkout Release left on a tag goes back onto the branch first, so it
/// follows it again; one on another branch (`master` before upstream had
/// `develop`) takes `branch` where it stands, unless a local `branch` holds
/// commits HEAD lacks.
pub fn fast_forward_command(repo: &Path, branch: &str) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "set -e; if ! git symbolic-ref -q HEAD >/dev/null; then git checkout --quiet --end-of-options \"$2\"; \
             elif [ \"$(git branch --show-current)\" != \"$2\" ]; then \
             if git rev-parse --quiet --verify \"refs/heads/$2\" >/dev/null \
             && ! git merge-base --is-ancestor \"refs/heads/$2\" HEAD; then \
             echo \"local branch $2 has commits the checkout lacks\" >&2; exit 1; fi; \
             git checkout --quiet -B \"$2\"; fi; git branch --quiet --set-upstream-to=\"$1\"; \
             {DROP_FORMER_SUBMODULES}; git merge --ff-only --end-of-options \"$1\"; \
             git submodule update --init --recursive; git log -1 --oneline"
        ))
        .arg("update")
        .arg(format!("origin/{branch}"))
        .arg(branch)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

/// Release's update: puts the checkout on `release`'s commit, detached, once
/// [`on_release`] vouched for it. Local edits are carried over or refused by
/// git, as in [`fast_forward_command`].
pub fn release_command(repo: &Path, release: &Release) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "set -e; {DROP_FORMER_SUBMODULES}; \
             git -c advice.detachedHead=false checkout --quiet --detach --end-of-options \"$1\"; \
             git submodule update --init --recursive; echo \"release $2\"; git log -1 --oneline"
        ))
        .arg("release")
        .arg(&release.commit)
        .arg(&release.tag)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}

/// Whether the checkout still has `commit`, a full one, to go back to.
pub async fn has_commit(repo: &Path, commit: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{commit}^{{commit}}"),
        ],
    )
    .await
    .is_ok()
}

/// Puts the checkout back on `commit`, an earlier one it built at, after the build of where it
/// moved failed. Only for a checkout [`upstream_commit`] vouches for, so the reset loses nothing:
/// the branch, or the detached release, follows upstream again from there.
pub fn rollback_command(repo: &Path, commit: &str) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "set -e; {DROP_FORMER_SUBMODULES}; git reset --quiet --hard --end-of-options \"$1\"; \
             git submodule update --init --recursive; echo \"back on $1\"; git log -1 --oneline"
        ))
        .arg("rollback")
        .arg(commit)
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

/// Puts the checkout on `branch` at upstream exactly.
///
/// Started by the user, or on its own for a [`Plan::Rewritten`] checkout
/// without local edits. Saves `git diff HEAD` to `backup` first when there
/// is one; the branch it leaves keeps its commits.
pub fn reset_command(repo: &Path, branch: &str, backup: &Path) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "set -e; mkdir -p -- \"$(dirname -- \"$2\")\"; git diff HEAD > \"$2\"; \
             if [ -s \"$2\" ]; then echo \"local changes saved to $2\"; else rm -f -- \"$2\"; fi; \
             {DROP_FORMER_SUBMODULES}; \
             git checkout --quiet --force -B \"$3\" --end-of-options \"$1\"; \
             git branch --quiet --set-upstream-to=\"$1\"; \
             git submodule update --init --recursive; git log -1 --oneline"
        ))
        .arg("reset")
        .arg(format!("origin/{branch}"))
        .arg(backup)
        .arg(branch)
        .current_dir(repo)
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
}
