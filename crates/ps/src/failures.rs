//! Failures a port reports while running: faults, `STOP:` lines, errors and bad exits, kept
//! across sessions in a profile's `failures.jsonl` with the output around them and the port
//! commit, so each one can be reported upstream as it happened.

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Lines kept before a failure, and lines collected after it (backtraces, module reports).
const CONTEXT_BEFORE: usize = 15;
const CONTEXT_AFTER: usize = 25;
/// Oldest records are dropped past this, so the file and the list stay small.
const MAX_RECORDS: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Kind {
    /// The process crashed: guest or host fault, segfault, abort.
    Crash,
    /// bbport refused to continue (`STOP:`): something unimplemented or inconsistent.
    Stop,
    /// An error line that did not end the run by itself.
    Error,
    /// The process ended with a failure code without any line above explaining it.
    Exit,
    /// What the port answers without doing it: imports with no implementation or a stub,
    /// effects or modules it leaves out. One per run, listing them all.
    Missing,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Crash => "Crash",
            Kind::Stop => "STOP",
            Kind::Error => "Error",
            Kind::Exit => "Exit",
            Kind::Missing => "Missing",
        }
    }
}

/// The facts behind a failure decanter words itself instead of quoting a line, so the UI can word
/// it in its own language.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Summary {
    /// `total` things left undone, `called` of them reached by the game.
    Missing { total: usize, called: usize },
    /// Distinct entries of each list of the port's [`CallReport`].
    Calls {
        unimplemented: usize,
        stubs: usize,
        failed: usize,
    },
    /// `None` when a signal ended the process.
    Exit(Option<i32>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Failure {
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub game_dir: PathBuf,
    pub title_id: String,
    pub version: String,
    /// Port checkout at the time (`git rev-parse --short HEAD`), empty if unknown.
    pub commit: String,
    pub kind: Kind,
    pub message: String,
    /// `None` for a line quoted from the output, and for records written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    /// Times the same message repeated in that run.
    pub count: u32,
    pub before: Vec<String>,
    pub after: Vec<String>,
    /// The run's whole output; `None` when it had no log file, and for records written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    /// On a [`Kind::Missing`] record, when the port wrote its call report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<CallReport>,
}

/// What the port counted at exit (`BB_CALL_REPORT`, psport's `FORK.md` "Call report"): imports
/// the game called that nothing answers for real, or that failed. Each list most calls first.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CallReport {
    pub unimplemented: Vec<Call>,
    pub stubs: Vec<Call>,
    pub failed: Vec<Call>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub count: u64,
    /// `None` for a GPU library function, which is no import.
    pub nid: Option<String>,
    pub name: String,
    /// Only on a failed call: one entry per import and error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CallError>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum CallError {
    Sce(u32),
    Errno(i32),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sce(code) => write!(f, "{code:#010x}"),
            Self::Errno(errno) => write!(f, "-1, errno {errno}"),
        }
    }
}

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.count, self.name)?;
        if let Some(nid) = &self.nid {
            write!(f, " ({nid})")?;
        }
        if let Some(error) = self.error {
            write!(f, " -> {error}")?;
        }
        Ok(())
    }
}

impl CallReport {
    /// `None` without the summary's first line, so a truncated or foreign file is not taken for
    /// a run where nothing was missing.
    pub fn parse(text: &str) -> Option<Self> {
        let mut report = Self::default();
        let mut found = false;
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("Calls: summary") else {
                continue;
            };
            if rest.starts_with(':') {
                found = true;
                continue;
            }
            let mut fields = rest.trim_start().splitn(3, ' ');
            let (Some(list), Some(count), Some(call)) = (fields.next(), fields.next(), fields.next()) else {
                continue;
            };
            let (Ok(count), Some(call)) = (count.parse(), parse_call(call)) else {
                continue;
            };
            let call = Call { count, ..call };
            match list {
                "unimplemented" => report.unimplemented.push(call),
                "stub" => report.stubs.push(call),
                "failed" if call.error.is_some() => report.failed.push(call),
                _ => {}
            }
        }
        for list in [&mut report.unimplemented, &mut report.stubs, &mut report.failed] {
            list.sort_by_key(|call| Reverse(call.count));
        }
        found.then_some(report)
    }

    pub const fn is_empty(&self) -> bool {
        self.unimplemented.is_empty() && self.stubs.is_empty() && self.failed.is_empty()
    }
}

/// `<NID> (<name>)[ -> <error>]`, or `<function> (HLE)`; the count is the caller's.
fn parse_call(text: &str) -> Option<Call> {
    let (import, rest) = text.split_once(" (")?;
    let (name, rest) = rest.split_once(')')?;
    let (nid, name) = if name == "HLE" {
        (None, import)
    } else {
        (Some(import.to_owned()), name)
    };
    let error = match rest.trim().strip_prefix("-> ") {
        None => None,
        Some(result) => Some(match result.strip_prefix("-1, errno ") {
            Some(errno) => CallError::Errno(errno.trim().parse().ok()?),
            None => CallError::Sce(u32::from_str_radix(result.trim().strip_prefix("0x")?, 16).ok()?),
        }),
    };
    Some(Call {
        count: 0,
        nid,
        name: name.to_owned(),
        error,
    })
}

impl Failure {
    /// The run's output around the failure; the message is one of its lines only when quoted
    /// from it, not when decanter worded it ([`Failure::summary`]).
    pub fn output(&self) -> impl Iterator<Item = &String> {
        let quoted = self.summary.is_none().then_some(&self.message);
        self.before.iter().chain(quoted).chain(&self.after)
    }

    /// Plain-text report for an issue: what, where, and the output around it.
    pub fn report(&self) -> String {
        let mut text = format!(
            "{} {} ({} v{}, commit {})\n{}\n",
            format_time(self.time),
            self.kind.label(),
            self.title_id,
            self.version,
            if self.commit.is_empty() { "?" } else { &self.commit },
            self.message
        );
        if self.count > 1 {
            text.push_str(&format!("repeated {} times\n", self.count));
        }
        text.push_str("\n```\n");
        for line in self.output() {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("```\n");
        if let Some(calls) = &self.calls {
            for (title, list) in [
                ("unimplemented", &calls.unimplemented),
                ("stub", &calls.stubs),
                ("failed", &calls.failed),
            ] {
                if !list.is_empty() {
                    text.push_str(&format!("\n{title}:\n"));
                    for call in list {
                        text.push_str(&format!("{call}\n"));
                    }
                }
            }
        }
        text
    }
}

/// Error-level lines that are expected, each with why: every listed fragment must appear.
const BENIGN: &[(&[&str], &str)] = &[(
    &["<Error>", "io_file.cpp", "/profile.bin"],
    "first start of a shader cache: shadPS4 reads the pipeline profile, finds none and writes it \
     (vk_pipeline_serialization.cpp)",
)];

fn is_benign(line: &str) -> bool {
    BENIGN
        .iter()
        .any(|(parts, _)| parts.iter().all(|part| line.contains(part)))
}

/// What the fork reports it leaves undone: called-unimplemented imports first (the game reached
/// them), then the startup report (imports with no implementation or a stub), then features of
/// its own libraries (Ngs2) and shadPS4's stubbed GPU calls. Lower is listed first.
fn missing_rank(line: &str) -> Option<u8> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("Runtime: unsupported import") {
        return Some(0);
    }
    if trimmed.starts_with("Runtime: import ")
        && (line.contains(" has no implementation") || line.contains(" is a stub"))
    {
        return Some(1);
    }
    if line.contains("not implemented") || line.contains("(STUBBED)") || line.contains("passes audio through") {
        return Some(2);
    }
    None
}

/// Classifies one output line. Formats come from bbport (`src/probe.c` faults, `STOP:` and
/// `ERROR:` in `src/runtime_*.c`, the GPU library's `<Error>`/`<Critical>` log levels,
/// `prepare failed:` from the scripts) plus generic crash, compiler and Python failures.
pub fn classify(line: &str) -> Option<Kind> {
    if is_benign(line) {
        return None;
    }
    let trimmed = line.trim_start();
    let crash = [
        "Guest fault",
        "Host fault",
        "Fault (signal",
        "Segmentation fault",
        "core dumped",
        "Aborted",
    ];
    if crash
        .iter()
        .any(|pattern| trimmed.starts_with(pattern) || line.contains(&format!(" {pattern}")))
    {
        return Some(Kind::Crash);
    }
    if trimmed.starts_with("STOP:") {
        return Some(Kind::Stop);
    }
    let error = trimmed.starts_with("ERROR:")
        || trimmed.starts_with("prepare failed:")
        || trimmed.starts_with("Traceback (most recent call last)")
        || trimmed.starts_with("fatal error:")
        || line.contains("<Error>")
        || line.contains("<Critical>")
        || line.contains("Assertion Failed")
        || line.contains(": error:")
        || line.contains(": fatal error:");
    error.then_some(Kind::Error)
}

/// Watches one run's output and produces its failures.
pub struct Scanner {
    game_dir: PathBuf,
    title_id: String,
    version: String,
    commit: String,
    recent: VecDeque<String>,
    /// The latest failure, still collecting the lines after it.
    pending: Option<Failure>,
    /// This run's failures so far, finished ones included, for counting repeats.
    found: Vec<Failure>,
    /// What [`missing_rank`] matched, with its rank, each line once.
    missing: Vec<(u8, String)>,
}

impl Scanner {
    pub fn new(game_dir: &Path, title_id: &str, version: &str, commit: String) -> Self {
        Self {
            game_dir: game_dir.to_owned(),
            title_id: title_id.to_owned(),
            version: version.to_owned(),
            commit,
            recent: VecDeque::with_capacity(CONTEXT_BEFORE),
            pending: None,
            found: Vec::new(),
            missing: Vec::new(),
        }
    }

    /// What a run's log header records about the game.
    pub fn facts(&self) -> [(&'static str, String); 3] {
        [
            ("title_id", self.title_id.clone()),
            ("version", self.version.clone()),
            ("game_dir", self.game_dir.display().to_string()),
        ]
    }

    fn finish_pending(&mut self) {
        if let Some(failure) = self.pending.take() {
            self.found.push(failure);
        }
    }

    pub fn line(&mut self, line: &str) {
        let classified = classify(line);
        let fatal = matches!(classified, Some(Kind::Crash | Kind::Stop));
        if !fatal && let Some(rank) = missing_rank(line) {
            let line = line.trim();
            if !self.missing.iter().any(|(_, seen)| seen == line) {
                self.missing.push((rank, line.to_owned()));
            }
            return;
        }
        if let Some(pending) = &mut self.pending {
            if classified.is_none() && pending.after.len() < CONTEXT_AFTER {
                pending.after.push(line.to_owned());
                return;
            }
            self.finish_pending();
        }
        if let Some(kind) = classified {
            let message = line.trim().to_owned();
            if let Some(seen) = self.found.iter_mut().find(|f| f.kind == kind && f.message == message) {
                seen.count += 1;
            } else {
                let failure = Failure {
                    time: now(),
                    game_dir: self.game_dir.clone(),
                    title_id: self.title_id.clone(),
                    version: self.version.clone(),
                    commit: self.commit.clone(),
                    kind,
                    message,
                    summary: None,
                    count: 1,
                    before: self.recent.iter().cloned().collect(),
                    after: Vec::new(),
                    log: None,
                    calls: None,
                };
                self.pending = Some(failure);
            }
        }
        if self.recent.len() == CONTEXT_BEFORE {
            self.recent.pop_front();
        }
        self.recent.push_back(line.to_owned());
    }

    /// Ends the run. A failing exit is a failure of its own only when nothing above explains it;
    /// an exit the user asked for is never one. `calls`, when the port wrote it, words the run's
    /// missing record instead of the lines scanned from the output, which it keeps for detail.
    pub fn exit(
        mut self,
        code: Option<i32>,
        stop_requested: bool,
        log: Option<&Path>,
        calls: Option<CallReport>,
    ) -> Vec<Failure> {
        self.finish_pending();
        let calls = calls.filter(|calls| !calls.is_empty());
        if !self.missing.is_empty() || calls.is_some() {
            self.missing.sort_by_key(|(rank, _)| *rank);
            let (message, summary) = if let Some(calls) = &calls {
                (
                    format!(
                        "{} unimplemented, {} stub, {} failed",
                        calls.unimplemented.len(),
                        calls.stubs.len(),
                        calls.failed.len()
                    ),
                    Summary::Calls {
                        unimplemented: calls.unimplemented.len(),
                        stubs: calls.stubs.len(),
                        failed: calls.failed.len(),
                    },
                )
            } else {
                let called = self.missing.iter().filter(|(rank, _)| *rank == 0).count();
                let total = self.missing.len();
                (
                    format!("{total} unimplemented, {called} called by the game"),
                    Summary::Missing { total, called },
                )
            };
            self.found.push(Failure {
                time: now(),
                game_dir: self.game_dir.clone(),
                title_id: self.title_id.clone(),
                version: self.version.clone(),
                commit: self.commit.clone(),
                kind: Kind::Missing,
                message,
                summary: Some(summary),
                count: 1,
                before: Vec::new(),
                after: self.missing.drain(..).map(|(_, line)| line).collect(),
                log: None,
                calls,
            });
        }
        let fatal = self.found.iter().any(|f| matches!(f.kind, Kind::Crash | Kind::Stop));
        if !stop_requested && code != Some(0) && !fatal {
            self.found.push(Failure {
                time: now(),
                game_dir: self.game_dir.clone(),
                title_id: self.title_id.clone(),
                version: self.version.clone(),
                commit: self.commit.clone(),
                kind: Kind::Exit,
                message: match code {
                    Some(code) => format!("exited with code {code}"),
                    None => "killed by a signal".to_owned(),
                },
                summary: Some(Summary::Exit(code)),
                count: 1,
                before: self.recent.iter().cloned().collect(),
                after: Vec::new(),
                log: None,
                calls: None,
            });
        }
        for failure in &mut self.found {
            failure.log = log.map(Path::to_owned);
        }
        self.found
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// `YYYY-MM-DD HH:MM UTC`, without a date library (days-from-civil inverse, H. Hinnant).
pub fn format_time(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (hour, minute) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// Plain file I/O on every call: callers keep it off the UI thread.
pub struct Log {
    path: PathBuf,
    records: Vec<Failure>,
}

impl Log {
    /// Unreadable lines are skipped: one damaged record must not hide the others.
    pub fn load(path: PathBuf) -> Log {
        let records = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Failure>(line).ok())
            // Records from before a line was known to be harmless.
            .filter(|failure| !is_benign(&failure.message))
            .collect();
        Log { path, records }
    }

    pub fn append(&mut self, failures: Vec<Failure>) -> io::Result<()> {
        if failures.is_empty() {
            return Ok(());
        }
        self.records.extend(failures);
        if self.records.len() > MAX_RECORDS {
            self.records.drain(..self.records.len() - MAX_RECORDS);
        }
        self.write()
    }

    pub fn clear_all(&mut self) -> io::Result<()> {
        self.records.clear();
        self.write()
    }

    pub fn clear(&mut self, game_dir: &Path) -> io::Result<()> {
        self.records.retain(|f| f.game_dir != game_dir);
        self.write()
    }

    fn write(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = Vec::new();
        for record in &self.records {
            serde_json::to_writer(&mut text, record).map_err(io::Error::other)?;
            text.write_all(b"\n")?;
        }
        crate::files::write_atomic(&self.path, &text)
    }

    pub fn records(&self) -> &[Failure] {
        &self.records
    }
}

/// Short commit of a port checkout, for the records.
pub async fn commit_of(repo: &Path) -> String {
    rev_parse_head(repo, &["--short"]).await
}

/// Full commit of a port checkout, stable across fetches unlike the short form whose length grows with the repo.
pub async fn full_commit_of(repo: &Path) -> String {
    rev_parse_head(repo, &[]).await
}

async fn rev_parse_head(repo: &Path, flags: &[&str]) -> String {
    let mut git = tokio::process::Command::new("git");
    git.arg("-C").arg(repo).arg("rev-parse").args(flags).arg("HEAD");
    decanter_core::strip_launcher_env(git.as_std_mut());
    git.output()
        .await
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default()
}
