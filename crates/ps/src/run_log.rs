//! Every job's whole output on disk, under the PS folder's `logs/`.
//!
//! The in-app log keeps the last lines only; a crash after a long session,
//! or a startup report hundreds of lines long, needs all of it. The header
//! is what the job was started with, so a log read alone says which
//! command, environment, game and port commit produced it; the footer how
//! it ended. Both are `# key: value` lines in English whatever the UI's
//! language, so a log reads the same for whoever it is sent to.

use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncWriteExt as _, BufWriter};
use tokio::process::Command;

use crate::failures::CallReport;
use crate::paths::Root;
use crate::process::JobKind;

/// Logs kept per job kind and target; older ones are deleted when a new one starts.
const KEEP: usize = 30;

pub fn dir(root: &Root) -> PathBuf {
    root.join("logs")
}

pub struct RunLog {
    path: PathBuf,
    file: BufWriter<tokio::fs::File>,
    calls: Option<PathBuf>,
    started: Instant,
}

impl RunLog {
    /// `target` names what the job is about (a title ID, a port); `facts` go in the header
    /// before the command. With `call_report`, the port is told to write its call report to
    /// [`RunLog::calls_path`]. Must be called before `command` is spawned.
    pub async fn start(
        root: &Root,
        target: &str,
        kind: JobKind,
        facts: &[(&str, String)],
        command: &mut Command,
        call_report: bool,
    ) -> io::Result<Self> {
        let dir = dir(root);
        tokio::fs::create_dir_all(&dir).await?;
        let prefix = format!("{}-{}-", slug(target), kind.name());
        prune(&dir, &prefix).await?;
        let started_at = now();
        let stem = format!("{prefix}{}", file_stamp(started_at));
        let mut path = dir.join(format!("{stem}.log"));
        let mut attempt: u32 = 1;
        let mut file = loop {
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(created) => break BufWriter::new(created),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    attempt = attempt.saturating_add(1);
                    path = dir.join(format!("{stem}-{attempt}.log"));
                }
                Err(err) => return Err(err),
            }
        };
        let calls = call_report.then(|| path.with_extension("calls"));
        if let Some(report) = &calls {
            command.env("BB_CALL_REPORT", report);
        }
        let header = header(kind, target, started_at, facts, command, calls.as_deref());
        file.write_all(header.as_bytes()).await?;
        Ok(Self {
            path,
            file,
            calls,
            started: Instant::now(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the port writes its call report when the process ends, beside the log and pruned
    /// with it, when it was asked for one; absent when the port predates the report.
    pub fn calls_path(&self) -> Option<&Path> {
        self.calls.as_deref()
    }

    pub async fn line(&mut self, line: &str) -> io::Result<()> {
        self.file.write_all(line.as_bytes()).await?;
        self.file.write_all(b"\n").await
    }

    /// `exit` is `None` when the process never started or could not be waited on; `report` is
    /// the one read from [`RunLog::calls_path`], when there was one.
    pub async fn finish(mut self, exit: Option<ExitStatus>, report: Option<&CallReport>) -> io::Result<()> {
        let mut footer = vec![
            "---".to_owned(),
            format!("ended: {}", iso_time(now())),
            format!("duration: {:.3}s", self.started.elapsed().as_secs_f64()),
            format!("exit: {}", describe_exit(exit)),
        ];
        if let Some(path) = &self.calls {
            footer.push(report.map_or_else(
                || "call_report: absent".to_owned(),
                |calls| {
                    format!(
                        "call_report: {} (unimplemented {}, stubs {}, failed {}, status {})",
                        path.display(),
                        calls.unimplemented.len(),
                        calls.stubs.len(),
                        calls.failed.len(),
                        calls.status.len()
                    )
                },
            ));
        }
        self.file.write_all(comments(&footer).as_bytes()).await?;
        self.file.flush().await
    }
}

/// `code N`, or `signal N` with `(core dumped)` when the kernel wrote one.
pub fn describe_exit(exit: Option<ExitStatus>) -> String {
    let Some(status) = exit else {
        return "none".to_owned();
    };
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("code {code}"),
        (None, Some(signal)) if status.core_dumped() => format!("signal {signal} (core dumped)"),
        (None, Some(signal)) => format!("signal {signal}"),
        (None, None) => status.to_string(),
    }
}

fn header(
    kind: JobKind,
    target: &str,
    started_at: u64,
    facts: &[(&str, String)],
    command: &Command,
    calls_path: Option<&Path>,
) -> String {
    let std = command.as_std();
    let mut lines = vec![
        format!("decanter: {}", decanter_core::BUILD),
        format!("job: {}", kind.name()),
        format!("target: {target}"),
        format!("started: {}", iso_time(started_at)),
    ];
    lines.extend(facts.iter().map(|(name, value)| format!("{name}: {value}")));
    lines.push(format!("program: {}", std.get_program().to_string_lossy()));
    lines.extend(std.get_args().map(|arg| format!("arg: {}", arg.to_string_lossy())));
    if let Some(dir) = std.get_current_dir() {
        lines.push(format!("cwd: {}", dir.display()));
    }
    // What decanter set, removed variables included: the rest is the environment it runs in.
    let mut envs: Vec<_> = std.get_envs().collect();
    envs.sort();
    lines.extend(envs.into_iter().map(|(key, value)| match value {
        Some(set) => format!("env: {}={}", key.to_string_lossy(), set.to_string_lossy()),
        None => format!("env_removed: {}", key.to_string_lossy()),
    }));
    if let Some(path) = calls_path {
        lines.push(format!("call_report: {}", path.display()));
    }
    lines.push("---".to_owned());
    comments(&lines)
}

fn comments(lines: &[String]) -> String {
    lines.iter().flat_map(|line| ["# ", line, "\n"]).collect()
}

/// File-name safe: letters and digits kept, anything else one `_`.
fn slug(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' { c } else { '_' })
        .collect()
}

async fn prune(dir: &Path, prefix: &str) -> io::Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut logs = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) && name.ends_with(".log") {
            logs.push(name);
        }
    }
    // The stamp sorts like the time it records.
    logs.sort();
    let excess = (logs.len() + 1).saturating_sub(KEEP);
    for name in logs.iter().take(excess) {
        let log = dir.join(name);
        if let Err(err) = tokio::fs::remove_file(log.with_extension("calls")).await
            && err.kind() != io::ErrorKind::NotFound
        {
            return Err(err);
        }
        tokio::fs::remove_file(log).await?;
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// ISO 8601 UTC to the second, from the same calendar arithmetic as [`crate::failures::format_time`].
fn iso_time(secs: u64) -> String {
    let day: String = crate::failures::format_time(secs).chars().take(10).collect();
    format!(
        "{day}T{:02}:{:02}:{:02}Z",
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// `YYYYMMDD-HHMMSS` UTC.
fn file_stamp(secs: u64) -> String {
    iso_time(secs)
        .chars()
        .filter_map(|c| match c {
            'T' => Some('-'),
            digit if digit.is_ascii_digit() => Some(digit),
            _ => None,
        })
        .collect()
}
