//! decanter's day log (`decanter/logs/decanter-YYYY-MM-DD.log`, tracing's full format).
//!
//! The build that ran, and the warnings and errors of decanter's own modules. Of each line only
//! the event's text and the fields that say what failed (`job`, `exit`, `error`, `err`) are read;
//! ids, paths and targets in the other fields are never looked at.

use crate::schema::Action;
use crate::text::cap_line;
use crate::validate::rules::MAX_MESSAGE;

#[derive(Debug, Default)]
pub struct DayLog {
    /// The last `decanter started build="<version> (<commit>)"`.
    pub decanter_version: Option<String>,
    pub decanter_commit: Option<String>,
    pub failures: Vec<DayFailure>,
}

#[derive(Debug)]
pub struct DayFailure {
    /// `YYYY-MM-DD` of its first occurrence.
    pub day: String,
    pub action: Action,
    /// Raw: normalized only when a report is built.
    pub message: String,
    pub count: u32,
}

struct Line<'a> {
    day: &'a str,
    level: &'a str,
    target: &'a str,
    message: &'a str,
    fields: &'a str,
}

fn split(line: &str) -> Option<Line<'_>> {
    let (time, rest) = line.trim_start_matches('\u{feff}').split_once(' ')?;
    let (level, rest) = rest.trim_start().split_once(' ')?;
    let (target, rest) = rest.split_once(": ")?;
    let rest = cap_line(rest, MAX_MESSAGE * 4);
    let fields_at = field_starts(rest).next().unwrap_or(rest.len());
    let (message, fields) = rest.split_at(fields_at);
    Some(Line {
        day: time.get(..10)?,
        level,
        target,
        message: message.trim(),
        fields,
    })
}

/// Where each ` key=` begins in a tracing line.
fn field_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.match_indices('=').filter_map(|(at, _)| {
        let before = &text[..at];
        let start = before.rfind(' ').map_or(0, |space| space + 1);
        let key = &before[start..];
        (!key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')).then_some(start)
    })
}

/// A field's value, unquoted: tracing quotes `?` values and leaves `%` ones bare, and a bare one
/// runs until the next field.
fn field<'a>(fields: &'a str, name: &str) -> Option<&'a str> {
    let starts: Vec<usize> = field_starts(fields).collect();
    starts.iter().enumerate().find_map(|(index, &start)| {
        let value = fields[start..].strip_prefix(name)?.strip_prefix('=')?;
        if let Some(quoted) = value.strip_prefix('"') {
            return quoted.split('"').next();
        }
        let end = starts.get(index + 1).map_or(fields.len(), |&next| next);
        Some(fields[start + name.len() + 1..end].trim())
    })
}

fn action(target: &str, job: Option<&str>) -> Action {
    match job {
        Some("build") => Action::Build,
        Some("update" | "rollback") => Action::Update,
        Some("game") => Action::Launch,
        Some("install") => Action::Install,
        _ if target.contains("gog_library") || target.contains("achievement") || target.contains("library::data") => {
            Action::Sync
        }
        _ if target.contains("settings") => Action::Settings,
        _ if target.contains("install") => Action::Install,
        _ if target.contains("launch") || target.contains("runtime") => Action::Launch,
        _ => Action::Other,
    }
}

/// The event's text with the fields that say what failed, in the order the vocabulary reads them.
fn failure_message(line: &Line<'_>) -> String {
    let mut message = line.message.to_owned();
    if let Some(job) = field(line.fields, "job") {
        message.push_str(&format!(" job={job}"));
    }
    if let Some(exit) = field(line.fields, "exit") {
        message.push_str(&format!(" exit={exit}"));
    }
    if let Some(error) = field(line.fields, "error").or_else(|| field(line.fields, "err")) {
        message.push_str(&format!(": {error}"));
    }
    message
}

/// A warning or error of decanter's own modules as [`DayLog::parse`] reads it; `None` for every
/// other line.
pub(crate) fn failing_line(text: &str) -> Option<String> {
    let line = split(text)?;
    (line.target.starts_with("decanter") && matches!(line.level, "WARN" | "ERROR")).then(|| failure_message(&line))
}

impl DayLog {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut log = Self::default();
        for line in text.lines().filter_map(split) {
            if !line.target.starts_with("decanter") {
                continue;
            }
            if line.message == "decanter started"
                && let Some(build) = field(line.fields, "build")
            {
                let (version, commit) = build.split_once(" (").unwrap_or((build, ""));
                log.decanter_version = Some(version.to_owned());
                log.decanter_commit = commit.strip_suffix(')').map(str::to_owned);
                continue;
            }
            if !matches!(line.level, "WARN" | "ERROR") {
                continue;
            }
            let message = failure_message(&line);
            let action = action(line.target, field(line.fields, "job"));
            match log
                .failures
                .iter_mut()
                .find(|f| f.action == action && f.message == message)
            {
                Some(seen) => seen.count = seen.count.saturating_add(1),
                None => log.failures.push(DayFailure {
                    day: line.day.to_owned(),
                    action,
                    message,
                    count: 1,
                }),
            }
        }
        log
    }
}
