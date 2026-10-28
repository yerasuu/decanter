//! The sources of a build, from git: the newest commit of the branch psport's Testing channel follows,
//! as the remote names it, and then exactly that commit with the one submodule the Windows build
//! needs (`gpu/third_party/imgui`) at the commit the superproject records for it.
//!
//! Only the remotes written here are ever asked: psport's own ([`Port::upstream`], the one Linux
//! follows) and Dear ImGui's. Every value the remote sends back is checked before it goes anywhere
//! (a branch name as [`crate::os::linux::updater`] accepts it, 40-hex commits), and every argument that comes
//! from outside goes after `--end-of-options`. git is decanter's pinned MinGit ([`program`]), never
//! the user's, run with only the variables in [`ENV`] (no `GIT_*` of the caller's), without a system
//! configuration, never looking for a repository above the work folder, and with [`OPTIONS`]: HTTPS
//! only, certificates verified, no credential helper, no hooks, no Git LFS, received objects checked,
//! links checked out as plain files. The user's own git configuration (a proxy, an `insteadOf`)
//! still applies. Nothing in the fetched tree runs here.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::{Commit, Error, Head, Log, Stage, pins, toolchain};
use crate::os::linux::updater;
use crate::paths::Root;
use crate::platform;
use crate::port::Port;
use crate::remote::RemoteError;

/// The remote psport's sources come from: the one Linux follows.
pub const REMOTE: &str = Port::Bbport.upstream();
/// The submodule `CMakeLists.txt` needs, and the only remote it is fetched from.
const IMGUI_PATH: &str = "gpu/third_party/imgui";
const IMGUI_REMOTE: &str = "https://github.com/ocornut/imgui.git";
/// One clone or fetch, on a slow connection.
const NETWORK_TIMEOUT: Duration = Duration::from_mins(15);
const LOCAL_TIMEOUT: Duration = Duration::from_mins(5);

/// The pinned MinGit's git, installed into the toolchain cache first when it isn't there: whatever
/// git the user has, or hasn't, plays no part (decision #1107).
///
/// # Errors
/// When MinGit isn't pinned, or can't be downloaded, doesn't match its pin or holds anything its
/// checks refuse.
pub async fn program(root: &Root, on_stage: &mut dyn FnMut(Stage)) -> Result<PathBuf, Error> {
    toolchain::install(&toolchain::dir(root), &pins::mingit(), on_stage)
        .await
        .map(|folder| toolchain::mingit_git(&folder))
}

/// The caller's variables git keeps: where the user's configuration is, temporary files, and
/// proxies. Everything else, `GIT_DIR` and `GIT_CONFIG_*` included, is dropped.
const ENV: [&str; 16] = [
    "SystemRoot",
    "SystemDrive",
    "PATH",
    "USERPROFILE",
    "HOME",
    "APPDATA",
    "ProgramData",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
];

/// Ahead of the user's configuration on every call ([git config](https://git-scm.com/docs/git-config)).
const OPTIONS: [&str; 15] = [
    "protocol.allow=never",
    "protocol.https.allow=always",
    "http.sslVerify=true",
    "credential.helper=",
    "core.hooksPath=",
    "core.symlinks=false",
    "core.protectNTFS=true",
    "core.longpaths=true",
    "core.fsmonitor=false",
    "transfer.fsckObjects=true",
    "submodule.recurse=false",
    "filter.lfs.smudge=",
    "filter.lfs.process=",
    "filter.lfs.required=false",
    "advice.detachedHead=false",
];

/// Where hooks are looked for: nowhere.
const NO_HOOKS: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// git in `dir`, its working folder and `-C`, never looking above `ceiling` for a repository
/// ([`GIT_CEILING_DIRECTORIES`](https://git-scm.com/docs/git#Documentation/git.txt-GITCEILINGDIRECTORIES)).
fn command(git: &Path, dir: &Path, ceiling: &Path) -> Command {
    let mut command = Command::new(git);
    for option in OPTIONS {
        command.arg("-c");
        if option == "core.hooksPath=" {
            command.arg(format!("core.hooksPath={NO_HOOKS}"));
        } else {
            command.arg(option);
        }
    }
    // MinGit names its CA bundle in its system configuration, which `GIT_CONFIG_NOSYSTEM` leaves out,
    // so certificates are checked against Windows' own store instead
    // ([http.sslBackend](https://git-scm.com/docs/git-config#Documentation/git-config.txt-httpsslBackend)).
    if cfg!(windows) {
        command.args(["-c", "http.sslBackend=schannel"]);
    }
    command.arg("-C").arg(dir).current_dir(dir).env_clear();
    for name in ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    // English so `RemoteError::from_git` can read why the remote failed; no prompt, since nobody can
    // answer one.
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // Ceilings are folders git won't move up into, so the one above `ceiling` keeps it at `ceiling`.
        .env("GIT_CEILING_DIRECTORIES", ceiling.parent().unwrap_or(ceiling))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    platform::without_console(&mut command);
    command
}

/// Where git runs and how far up it may look for a repository.
#[derive(Clone, Copy)]
struct Place<'path> {
    dir: &'path Path,
    ceiling: &'path Path,
}

/// `args` in `place`, its output, each stderr line also in `log`.
async fn git(
    git: &Path,
    place: Place<'_>,
    args: &[&OsStr],
    timeout: Duration,
    log: Option<&mut Log<'_>>,
) -> Result<String, String> {
    let shown: Vec<_> = args.iter().map(|arg| arg.to_string_lossy()).collect();
    let mut command = command(git, place.dir, place.ceiling);
    let output = match tokio::time::timeout(timeout, command.args(args).output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(format!("git: {err}")),
        Err(_) => return Err(format!("git {}: timed out", shown.join(" "))),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(log) = log {
        log.line(format!("$ git {}", shown.join(" "))).await;
        for line in stderr.lines() {
            log.line(line.to_owned()).await;
        }
    }
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    } else {
        Err(format!("git {}: {}", shown.join(" "), stderr.trim()))
    }
}

/// The branch the Testing channel follows on [`REMOTE`], as on Linux: `develop`, or the default
/// branch when there is none; and its newest commit.
///
/// # Errors
/// When `work`, the build's work folder git runs in, can't be made, or the remote can't be asked or
/// answers without a usable branch.
pub async fn head(program: &Path, work: &Path) -> Result<Head, Error> {
    if let Err(err) = tokio::fs::create_dir_all(work).await {
        return Err(err.into());
    }
    let refs = match git(
        program,
        Place {
            dir: work,
            ceiling: work,
        },
        &[
            OsStr::new("ls-remote"),
            OsStr::new("--symref"),
            OsStr::new("--end-of-options"),
            OsStr::new(REMOTE),
            OsStr::new("HEAD"),
            OsStr::new("refs/heads/*"),
        ],
        NETWORK_TIMEOUT,
        None,
    )
    .await
    {
        Ok(refs) => refs,
        Err(err) => return Err(Error::Remote(RemoteError::from_git(err))),
    };
    head_of(&refs)
}

/// [`head`] from `git ls-remote --symref` lines: `ref: refs/heads/<branch>\tHEAD` and
/// `<commit>\t<ref>`.
fn head_of(refs: &str) -> Result<Head, Error> {
    let commit_of = |name: &str| {
        refs.lines()
            .filter_map(|line| line.split_once('\t'))
            .find(|&(_, listed)| listed == name)
            .map(|(commit, _)| commit)
    };
    let default = refs
        .lines()
        .find_map(|line| line.strip_prefix("ref: refs/heads/"))
        .and_then(|rest| rest.strip_suffix("\tHEAD"));
    let branch = if commit_of("refs/heads/develop").is_some() {
        "develop"
    } else {
        match default {
            Some(branch) => branch,
            None => return Err(Error::Rejected("the remote names no default branch".to_owned())),
        }
    };
    if !updater::valid_branch(branch) {
        return Err(Error::Rejected(format!(
            "a branch name decanter doesn't take: {:?}",
            branch.chars().take(80).collect::<String>()
        )));
    }
    let Some(listed) = commit_of(&format!("refs/heads/{branch}")) else {
        return Err(Error::Rejected(format!("no commit listed for {branch}")));
    };
    match listed.parse::<Commit>() {
        Ok(commit) => Ok(Head {
            branch: branch.to_owned(),
            commit,
        }),
        Err(_) => Err(Error::Rejected(format!(
            "not a full commit for {branch}: {:?}",
            listed.chars().take(80).collect::<String>()
        ))),
    }
}

/// `head`'s commit checked out in `dir`, a new folder below `work`, with [`IMGUI_PATH`] at the commit
/// it records.
///
/// # Errors
/// When a fetch fails, or what is checked out isn't the commit asked for.
pub async fn checkout(program: &Path, work: &Path, dir: &Path, head: &Head, log: &mut Log<'_>) -> Result<(), Error> {
    let place = Place { dir, ceiling: work };
    if let Err(err) = fetch_commit(program, place, REMOTE, head.commit.as_str(), log).await {
        return Err(err);
    }
    let listed = match git(
        program,
        place,
        &[
            OsStr::new("ls-tree"),
            OsStr::new("--end-of-options"),
            OsStr::new(head.commit.as_str()),
            OsStr::new(IMGUI_PATH),
        ],
        LOCAL_TIMEOUT,
        None,
    )
    .await
    {
        Ok(listed) => listed,
        Err(err) => return Err(Error::Git(err)),
    };
    let imgui = match gitlink(&listed) {
        Some(imgui) => imgui,
        None => {
            return Err(Error::Rejected(format!(
                "{IMGUI_PATH} is not a submodule at {}: {:?}",
                head.commit,
                listed.chars().take(120).collect::<String>()
            )));
        }
    };
    let submodule = dir.join(IMGUI_PATH);
    let place = Place {
        dir: &submodule,
        ceiling: work,
    };
    fetch_commit(program, place, IMGUI_REMOTE, &imgui, log).await
}

/// The commit of a `160000 commit <sha>\t<path>` line, the only one listed.
fn gitlink(listed: &str) -> Option<String> {
    let mut lines = listed.lines();
    let (meta, path) = lines.next()?.split_once('\t')?;
    let mut fields = meta.split(' ');
    match (fields.next(), fields.next(), fields.next(), fields.next(), lines.next()) {
        (Some("160000"), Some("commit"), Some(commit), None, None)
            if path == IMGUI_PATH && commit.parse::<Commit>().is_ok() =>
        {
            Some(commit.to_owned())
        }
        _ => None,
    }
}

/// `commit` of `remote`, alone and without history, checked out detached in `dir`, then read back.
async fn fetch_commit(
    program: &Path,
    place: Place<'_>,
    remote: &str,
    commit: &str,
    log: &mut Log<'_>,
) -> Result<(), Error> {
    if let Err(err) = tokio::fs::create_dir_all(place.dir).await {
        return Err(err.into());
    }
    let steps: [(&[&str], Duration); 3] = [
        (&["init", "--quiet"], LOCAL_TIMEOUT),
        (
            &[
                "fetch",
                "--quiet",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                "--end-of-options",
                remote,
                commit,
            ],
            NETWORK_TIMEOUT,
        ),
        (
            &["checkout", "--quiet", "--detach", "--end-of-options", commit],
            LOCAL_TIMEOUT,
        ),
    ];
    for (args, timeout) in steps {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        if let Err(err) = git(program, place, &args, timeout, Some(&mut *log)).await {
            return Err(Error::Git(err));
        }
    }
    let at = git(
        program,
        place,
        &[OsStr::new("rev-parse"), OsStr::new("--verify"), OsStr::new("HEAD")],
        LOCAL_TIMEOUT,
        None,
    )
    .await;
    match at {
        Ok(at) if at == commit => {
            log.line(format!("decanter: {remote} at {commit}")).await;
            Ok(())
        }
        Ok(at) => Err(Error::Rejected(format!("{remote} checked out {at}, not {commit}"))),
        Err(err) => Err(Error::Git(err)),
    }
}
