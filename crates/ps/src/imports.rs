//! The imports psport does not cover in a run, read back from its log ([`crate::run_log`]).
//!
//! Once the game is linked and before it runs, psport prints every import that will not do what it
//! asks and a `Runtime: imports` totals line (`src/main/bind.c`); the loader prints the imports it
//! left to the host runtime because only their versions differ and the system modules it serves
//! instead of linking them (`src/loader/loader_link.c`). While the game runs, the first call of an
//! unimplemented import under `PS_PERMISSIVE` prints an `unsupported import` line. A port that
//! predates these lines prints no totals line, which is how its runs are told apart from one where
//! nothing is missing.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::Path;

use crate::failures::CallReport;

/// Read of a log at most: the lines read are printed at startup, so a longer log only adds the
/// game's own output.
pub const MAX_LOG_BYTES: u64 = 64 << 20;
/// A call report is a few kilobytes; a larger file is not one.
const MAX_CALLS_BYTES: u64 = 1 << 20;
/// A title imports about a thousand functions in all, so more distinct ones is a damaged log.
const MAX_IMPORTS: usize = 4096;
const MAX_MODULES: usize = 512;
/// NIDs are 11 characters; the bound only rejects a damaged token.
const MAX_NID: usize = 16;
const MAX_NAME: usize = 256;

/// Why psport does not cover an import, most serious first: the order the imports are sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gap {
    /// The game stops if it calls it, or gets 0 under `PS_PERMISSIVE`.
    Unimplemented,
    /// It succeeds without doing anything.
    Stub,
    /// A system module exports it under another library or module version only; the loader leaves
    /// it to the host runtime unless `PS_LOOSE_IMPORT_VERSIONS=1`.
    LeftToHost,
    /// Seen only when the game called it, with no line from linking (a module the game loaded
    /// later, or a port that lists nothing at link time).
    Called,
}

#[derive(Debug)]
pub struct Import {
    /// Without the `#` suffix the port appends, an opaque library tag, so an import reached
    /// through several modules is one entry.
    pub nid: String,
    /// `None` when the port could not resolve it.
    pub name: Option<String>,
    pub gap: Gap,
    /// An `unsupported import` line or the call report showed the game calling it.
    pub called: bool,
    /// The calls the port's call report counted, when the run left one that names the import.
    pub calls: Option<u64>,
}

/// The `Runtime: imports` line: where each of the game's imports was bound. A part a port does not
/// print reads 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub imports: u64,
    pub host: u64,
    pub game_modules: u64,
    pub system_modules: u64,
    pub loaded_modules: u64,
    pub stubs: u64,
    pub unimplemented: u64,
}

/// A system module the loader did not link the usual way. `reason` is the port's own wording.
#[derive(Debug)]
pub enum Decision {
    /// Not linked: the port serves its library instead.
    Served { module: String, reason: String },
    /// Linked although its library goes through a part of the system the port stands in for,
    /// because the port lacks some of that library's imports; `lacks` is `None` when that part of
    /// the line did not parse.
    LinkedAlthough {
        module: String,
        reason: String,
        lacks: Option<Lacks>,
    },
}

#[derive(Debug)]
pub struct Lacks {
    pub count: u64,
    pub library: String,
    pub first: String,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    /// `YYYY-MM-DD HH:MM UTC`, from the log's header.
    pub started: Option<String>,
    /// `None` when the port that ran it predates these lines, or the run ended before linking.
    pub totals: Option<Totals>,
    /// By [`Gap`], then name (unresolved last), then NID.
    pub imports: Vec<Import>,
    /// Modules linked although the port lacks imports first, each kind in the log's order.
    pub decisions: Vec<Decision>,
    /// Bytes past [`MAX_LOG_BYTES`], not read.
    pub unread_bytes: u64,
    /// Lines past the caps on distinct imports and modules.
    pub dropped: u64,
}

struct Seen {
    name: Option<String>,
    gap: Gap,
    called: bool,
}

#[derive(Default)]
struct Reader {
    report: ImportReport,
    imports: HashMap<String, Seen>,
    modules: HashSet<String>,
    malformed: u32,
}

impl ImportReport {
    /// Reads at most the first [`MAX_LOG_BYTES`] of the log, and the call report beside it when the
    /// run left one. File I/O: off the UI thread.
    ///
    /// # Errors
    ///
    /// When the log cannot be opened or read; a missing or unreadable call report only leaves the
    /// counts out.
    pub fn read(log: &Path) -> io::Result<Self> {
        let file = File::open(log)?;
        let size = file.metadata()?.len();
        let mut reader = BufReader::new(file.take(MAX_LOG_BYTES));
        let mut state = Reader::default();
        state.report.unread_bytes = size.saturating_sub(MAX_LOG_BYTES);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            if reader.read_until(b'\n', &mut buffer)? == 0 {
                break;
            }
            state.line(String::from_utf8_lossy(&buffer).trim());
        }
        if state.malformed > 0 {
            tracing::debug!(
                log = %log.display(),
                malformed = state.malformed,
                "import lines skipped"
            );
        }
        let calls = read_calls(&log.with_extension("calls"));
        Ok(state.finish(calls.as_ref()))
    }
}

impl Reader {
    fn line(&mut self, line: &str) {
        let parsed = if let Some(started) = line.strip_prefix("# started: ") {
            if self.report.started.is_none() {
                self.report.started = crate::frame_stats::display_time(started);
            }
            true
        } else if let Some(rest) = line.strip_prefix("Runtime: imports ") {
            parse_totals(rest)
                .map(|totals| self.report.totals = Some(totals))
                .is_some()
        } else if let Some(rest) = line.strip_prefix("Runtime: import ") {
            let gap = if rest.contains(") has no implementation") {
                Some(Gap::Unimplemented)
            } else if rest.contains(") is a stub") {
                Some(Gap::Stub)
            } else {
                None
            };
            gap.and_then(|gap| self.import(rest, gap)).is_some()
        } else if let Some(rest) = line.strip_prefix("Runtime: unsupported import ") {
            self.import(rest, Gap::Called).is_some()
        } else if let Some(rest) = line.strip_prefix("Loader: import ") {
            // A versionless match the port bound covers the import.
            !rest.contains(" only ignoring versions: left to the host runtime")
                || self.import(rest, Gap::LeftToHost).is_some()
        } else if let Some(rest) = line.strip_prefix("Loader: system module ") {
            self.module(rest)
        } else {
            true
        };
        if !parsed {
            self.malformed = self.malformed.saturating_add(1);
        }
    }

    /// `TOKEN (NAME) ...`; `None` when it does not parse.
    fn import(&mut self, text: &str, gap: Gap) -> Option<()> {
        let (token, rest) = text.split_once(" (")?;
        let (name, _) = rest.split_once(')')?;
        let nid = token.split('#').next()?;
        let is_nid = !nid.is_empty()
            && nid.len() <= MAX_NID
            && nid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'_'));
        if !is_nid || name.is_empty() || name.len() > MAX_NAME {
            return None;
        }
        let name = (!matches!(name, "name not resolved" | "unnamed")).then(|| name.to_owned());
        if let Some(seen) = self.imports.get_mut(nid) {
            seen.gap = seen.gap.min(gap);
            seen.called |= gap == Gap::Called;
            if seen.name.is_none() {
                seen.name = name;
            }
        } else if self.imports.len() < MAX_IMPORTS {
            self.imports.insert(
                nid.to_owned(),
                Seen {
                    name,
                    gap,
                    called: gap == Gap::Called,
                },
            );
        } else {
            self.report.dropped = self.report.dropped.saturating_add(1);
        }
        Some(())
    }

    /// `X not linked, the port serves L: REASON` or `X linked although REASON: the port lacks N of
    /// the imports of L, first F`; `X not linked: served natively ...` is the usual case and left
    /// out. `false` when a line of either kind does not parse.
    fn module(&mut self, text: &str) -> bool {
        let Some((module, rest)) = text.split_once(' ') else {
            return false;
        };
        let decision = if let Some(rest) = rest.strip_prefix("not linked, the port serves ") {
            let Some((_, reason)) = rest.split_once(": ") else {
                return false;
            };
            Decision::Served {
                module: module.to_owned(),
                reason: reason.to_owned(),
            }
        } else if let Some(rest) = rest.strip_prefix("linked although ") {
            let (reason, lacks) = rest
                .split_once(": the port lacks ")
                .map_or((rest, None), |(reason, lacks)| (reason, parse_lacks(lacks)));
            Decision::LinkedAlthough {
                module: module.to_owned(),
                reason: reason.to_owned(),
                lacks,
            }
        } else {
            return true;
        };
        // The loader asks again on every pass while a module stays unlinked.
        if !self.modules.contains(module) {
            if self.modules.len() < MAX_MODULES {
                self.modules.insert(module.to_owned());
                self.report.decisions.push(decision);
            } else {
                self.report.dropped = self.report.dropped.saturating_add(1);
            }
        }
        true
    }

    fn finish(mut self, calls: Option<&CallReport>) -> ImportReport {
        let mut counted: HashMap<&str, u64> = HashMap::new();
        for call in calls
            .into_iter()
            .flat_map(|report| report.unimplemented.iter().chain(&report.stubs))
        {
            if let Some(nid) = call.nid.as_deref().and_then(|token| token.split('#').next()) {
                let count = counted.entry(nid).or_default();
                *count = count.saturating_add(call.count);
            }
        }
        let mut imports = Vec::with_capacity(self.imports.len());
        imports.extend(self.imports.into_iter().map(|(nid, seen)| {
            let calls = counted.get(nid.as_str()).copied();
            Import {
                called: seen.called || calls.is_some_and(|count| count > 0),
                calls,
                name: seen.name,
                gap: seen.gap,
                nid,
            }
        }));
        imports.sort_unstable_by(|a, b| {
            a.gap
                .cmp(&b.gap)
                .then_with(|| a.name.is_none().cmp(&b.name.is_none()))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.nid.cmp(&b.nid))
        });
        self.report.imports = imports;
        self.report
            .decisions
            .sort_by_key(|decision| matches!(decision, Decision::Served { .. }));
        self.report
    }
}

/// `969: host 482, game modules 214, system modules 191, modules the game loads 0, stubs 10,
/// without implementation 72`; parts it does not know are skipped.
fn parse_totals(text: &str) -> Option<Totals> {
    let (imports, parts) = text.split_once(": ")?;
    let mut totals = Totals {
        imports: imports.parse().ok()?,
        ..Totals::default()
    };
    for part in parts.split(", ") {
        let (label, count) = part.trim().rsplit_once(' ')?;
        let count = count.parse().ok()?;
        match label {
            "host" => totals.host = count,
            "game modules" => totals.game_modules = count,
            "system modules" => totals.system_modules = count,
            "modules the game loads" => totals.loaded_modules = count,
            "stubs" => totals.stubs = count,
            "without implementation" => totals.unimplemented = count,
            _ => {}
        }
    }
    Some(totals)
}

/// `N of the imports of L, first F`.
fn parse_lacks(text: &str) -> Option<Lacks> {
    let (count, rest) = text.split_once(" of the imports of ")?;
    let (library, first) = rest.split_once(", first ")?;
    Some(Lacks {
        count: count.parse().ok()?,
        library: library.to_owned(),
        first: first.to_owned(),
    })
}

fn read_calls(path: &Path) -> Option<CallReport> {
    let mut bytes = Vec::new();
    let read = File::open(path).and_then(|file| file.take(MAX_CALLS_BYTES).read_to_end(&mut bytes));
    match read {
        Ok(_) => CallReport::parse(&String::from_utf8_lossy(&bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(report = %path.display(), %err, "could not read a run's call report");
            None
        }
    }
}
