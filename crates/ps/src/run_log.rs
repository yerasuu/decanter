//! Every job's whole output on disk, under the PS folder's `logs/`.
//!
//! The in-app log keeps the last lines only; a crash after a long session,
//! or a startup report hundreds of lines long, needs all of it. The header
//! is what the job was started with, so a log read alone says which
//! command, environment, game and port commit produced it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncWriteExt as _, BufWriter};
use tokio::process::Command;

use crate::paths::Root;

/// Logs kept per job kind and target; older ones are deleted when a new one starts.
const KEEP: usize = 30;

pub fn dir(root: &Root) -> PathBuf {
    root.join("logs")
}

pub struct RunLog {
    path: PathBuf,
    file: BufWriter<tokio::fs::File>,
}

impl RunLog {
    /// `target` names what the job is about (a title ID, a port); `facts` go in the header
    /// before the command. Must be called before `command` is spawned.
    pub async fn start(
        root: &Root,
        target: &str,
        label: &str,
        facts: &[(&str, String)],
        command: &Command,
    ) -> io::Result<Self> {
        let dir = dir(root);
        tokio::fs::create_dir_all(&dir).await?;
        let prefix = format!("{}-{}-", slug(target), slug(label));
        prune(&dir, &prefix).await?;
        let stem = format!("{prefix}{}", file_stamp(now()));
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
        file.write_all(header(label, facts, command).as_bytes()).await?;
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn line(&mut self, line: &str) -> io::Result<()> {
        self.file.write_all(line.as_bytes()).await?;
        self.file.write_all(b"\n").await
    }

    pub async fn finish(mut self, code: Option<i32>) -> io::Result<()> {
        let end = match code {
            Some(code) => format!("# exited with code {code} at {}\n", crate::failures::format_time(now())),
            None => format!("# ended by a signal at {}\n", crate::failures::format_time(now())),
        };
        self.file.write_all(end.as_bytes()).await?;
        self.file.flush().await
    }
}

fn header(label: &str, facts: &[(&str, String)], command: &Command) -> String {
    let std = command.as_std();
    let mut text = format!(
        "# decanter {}\n# {label}, started {}\n",
        env!("CARGO_PKG_VERSION"),
        crate::failures::format_time(now())
    );
    for (name, value) in facts {
        text.push_str(&format!("# {name}: {value}\n"));
    }
    text.push_str(&format!("# program: {}\n", std.get_program().to_string_lossy()));
    for arg in std.get_args() {
        text.push_str(&format!("# arg: {}\n", arg.to_string_lossy()));
    }
    if let Some(dir) = std.get_current_dir() {
        text.push_str(&format!("# directory: {}\n", dir.display()));
    }
    // What decanter set, removed variables included: the rest is the environment it runs in.
    let mut envs: Vec<_> = std.get_envs().collect();
    envs.sort();
    for (key, value) in envs {
        match value {
            Some(value) => text.push_str(&format!(
                "# env: {}={}\n",
                key.to_string_lossy(),
                value.to_string_lossy()
            )),
            None => text.push_str(&format!("# env: {} removed\n", key.to_string_lossy())),
        }
    }
    text
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
        tokio::fs::remove_file(dir.join(name)).await?;
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `YYYYMMDD-HHMMSS` UTC, from the same calendar arithmetic as [`crate::failures::format_time`].
fn file_stamp(secs: u64) -> String {
    let day = crate::failures::format_time(secs);
    let date: String = day.chars().take(10).filter(char::is_ascii_digit).collect();
    format!(
        "{date}-{:02}{:02}{:02}",
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
