//! Reports built from the local sources.
//!
//! Each value goes through its field's rule on the way in: an optional one that does not fit is
//! left out (and logged), free text is normalized, and the finished report passes [`check`]
//! before it is returned, so the client never sends what the server would refuse.

use crate::schema::{
    Action, Build, Call, Calls, DecanterReport, Failure, FailureKind, Fault, Gpu, Os, Perf, Product, PsportReport,
    Report, SCHEMA, System,
};
use crate::sources::day_log::DayFailure;
use crate::sources::day_of;
use crate::sources::failures::{FailureRecord, RecordCall, RecordCalls};
use crate::sources::game_log::GameLog;
use crate::text::{self, Mode, is_image, normalize_message};
use crate::validate::rules::{
    MAX_CALLS, MAX_COUNT, MAX_FRAMES, MAX_MESSAGE, is_commit, is_day, is_game_version, is_gpu_model, is_perf_value,
    is_setting_value, is_symbol, is_version,
};
use crate::validate::{Rejection, Rule, check};

const TARGET: &str = "decanter_report::build";

/// What decanter knows about itself and the machine it runs on, rather than reads from a log.
pub struct Host {
    pub os: Os,
    /// The release as the system names it (`SteamOS 3.8.1`, `Windows 11 26100`).
    pub os_version: Option<String>,
    pub gpu: Option<Gpu>,
    pub decanter_version: String,
    pub decanter_commit: Option<String>,
    /// Unix time, for a report whose sources carry no date.
    pub now: u64,
}

/// The sources of one psport report: a `failures.jsonl` record, the run's log, or both; the
/// record's failure wins over the log's, since it is the one the player chose to report.
pub struct PsportInput<'a> {
    pub record: Option<&'a FailureRecord>,
    pub log: Option<&'a GameLog>,
    /// `TITLE_ID` and `TITLE` of the dump's `param.sfo`, already read by decanter; they win over
    /// what the sources say, since a psport log alone does not always name its title.
    pub title_id: Option<&'a str>,
    pub title_name: Option<&'a str>,
}

fn keep(field: &'static str, value: Option<&str>, fits: impl Fn(&str) -> bool) -> Option<String> {
    let value = value?;
    if fits(value) {
        return Some(value.to_owned());
    }
    tracing::debug!(target: TARGET, field, "left a value out of the report: it does not fit its field");
    None
}

fn failure(kind: FailureKind, raw: &str, count: u32) -> Failure {
    let message = normalize_message(raw, MAX_MESSAGE);
    Failure {
        kind,
        message: if message.is_empty() {
            "<text>".to_owned()
        } else {
            message
        },
        count: count.clamp(1, MAX_COUNT),
    }
}

fn system(host: &Host, os: Option<Os>, gpu: Option<&Gpu>) -> System {
    let gpu = gpu.or(host.gpu.as_ref()).and_then(|gpu| {
        let fits = |text: &str| text.len() <= 96 && text::check(text, Mode::Label).is_empty();
        (fits(&gpu.model) && is_gpu_model(gpu.vendor, &gpu.model)).then(|| Gpu {
            vendor: gpu.vendor,
            model: gpu.model.clone(),
            driver: keep("system.gpu.driver", gpu.driver.as_deref(), |d| {
                d.len() <= 48 && text::check(d, Mode::Label).is_empty()
            }),
        })
    });
    System {
        os: os.unwrap_or(host.os),
        os_version: keep("system.os_version", host.os_version.as_deref(), |v| {
            v.len() <= 48 && text::check(v, Mode::Label).is_empty()
        }),
        gpu,
    }
}

fn decanter_build(host: &Host) -> Build {
    Build {
        version: keep("decanter.version", Some(&host.decanter_version), is_version),
        commit: keep("decanter.commit", host.decanter_commit.as_deref(), is_commit),
    }
}

/// A call's name is a symbol; the port also words some entries as text (`trophy configuration
/// unreadable: ...`), which has no field here.
fn fit_calls(calls: impl Iterator<Item = Call>) -> Vec<Call> {
    calls
        .filter(|call| {
            let fits = is_symbol(&call.name, 96) && call.count > 0;
            if !fits {
                tracing::debug!(target: TARGET, "left a call out of the report: its name is not a symbol");
            }
            fits
        })
        .take(MAX_CALLS)
        .collect()
}

fn record_call(call: &RecordCall) -> Call {
    Call {
        name: call.name.clone(),
        count: call.count,
        code: call.error,
    }
}

fn calls(record: Option<&RecordCalls>, log: Option<&Calls>) -> Option<Calls> {
    let calls = match (record, log) {
        (Some(record), _) => Calls {
            unimplemented: fit_calls(record.unimplemented.iter().map(record_call)),
            stubs: fit_calls(record.stubs.iter().map(record_call)),
            failed: fit_calls(record.failed.iter().map(record_call)),
            status: fit_calls(record.status.iter().map(record_call)),
        },
        (None, Some(log)) => Calls {
            unimplemented: fit_calls(log.unimplemented.iter().cloned()),
            stubs: fit_calls(log.stubs.iter().cloned()),
            failed: fit_calls(log.failed.iter().cloned()),
            status: fit_calls(log.status.iter().cloned()),
        },
        (None, None) => return None,
    };
    let empty =
        calls.unimplemented.is_empty() && calls.stubs.is_empty() && calls.failed.is_empty() && calls.status.is_empty();
    (!empty).then_some(calls)
}

fn fault(fault: &Fault) -> Option<Fault> {
    let frames = fault
        .frames
        .iter()
        .filter(|frame| {
            u32::try_from(frame.offset).is_ok() && frame.module.as_deref().is_none_or(|m| m.len() <= 64 && is_image(m))
        })
        .take(MAX_FRAMES)
        .map(|frame| crate::schema::Frame {
            module: frame.module.clone(),
            offset: frame.offset,
            symbol: keep("fault.frames.symbol", frame.symbol.as_deref(), |s| is_symbol(s, 128)),
        })
        .collect();
    (1..=64).contains(&fault.signal).then_some(Fault {
        side: fault.side,
        signal: fault.signal,
        frames,
    })
}

fn perf(perf: &Perf) -> Option<Perf> {
    let fits = [perf.fps_mean, perf.fps_min, perf.worst_frame_ms]
        .into_iter()
        .all(is_perf_value);
    (fits && (1..=100_000).contains(&perf.samples)).then_some(Perf {
        samples: perf.samples,
        fps_mean: (perf.fps_mean * 10.0).round() / 10.0,
        fps_min: perf.fps_min,
        worst_frame_ms: perf.worst_frame_ms,
    })
}

/// The log's own failure when no record is given: its fault, else its first stop or error line,
/// else its call report.
fn log_failure(log: &GameLog) -> Option<Failure> {
    if let Some((line, _)) = &log.fault {
        return Some(failure(FailureKind::Crash, line, 1));
    }
    if let Some((kind, line, count)) = &log.error {
        return Some(failure(*kind, line, *count));
    }
    let calls = log.calls.as_ref()?;
    let raw = format!(
        "{} unimplemented, {} stub, {} failed, {} status",
        calls.unimplemented.len(),
        calls.stubs.len(),
        calls.failed.len(),
        calls.status.len()
    );
    Some(failure(FailureKind::Missing, &raw, 1))
}

/// # Errors
///
/// The rejections of [`check`], or a `failure` one when neither source holds a failure.
pub fn psport_report(input: &PsportInput<'_>, host: &Host) -> Result<Report, Vec<Rejection>> {
    let PsportInput {
        record,
        log,
        title_id,
        title_name,
    } = *input;
    let failure = record
        .map_or_else(
            || log.and_then(log_failure),
            |record| Some(failure(record.kind.into(), &record.message, record.count)),
        )
        .ok_or_else(|| vec![Rejection::new("failure", Rule::MissingField)])?;
    let date = record
        .map(|record| day_of(record.time))
        .or_else(|| log.and_then(|log| log.day.clone()).filter(|day| is_day(day)))
        .unwrap_or_else(|| day_of(host.now));
    let title_id = title_id
        .or_else(|| record.map(|record| record.title_id.as_str()))
        .or_else(|| log.and_then(|log| log.title_id.as_deref()))
        .unwrap_or_default()
        .to_owned();
    let game_version = record
        .map(|record| record.version.as_str())
        .or_else(|| log.and_then(|log| log.game_version.as_deref()));
    let name = title_name.or_else(|| log.and_then(|log| log.game_name.as_deref()));
    let psport_commit = record
        .map(|record| record.commit.as_str())
        .filter(|commit| !commit.is_empty())
        .or_else(|| log.and_then(|log| log.psport_commit.as_deref()));
    let calls = calls(
        record.and_then(|record| record.calls.as_ref()),
        log.and_then(|log| log.calls.as_ref()),
    );
    let report = Report::Psport(Box::new(PsportReport {
        schema: SCHEMA,
        product: Product::Psport,
        date,
        decanter: decanter_build(host),
        psport: Build {
            version: keep(
                "psport.version",
                log.and_then(|log| log.psport_version.as_deref()),
                is_version,
            ),
            commit: keep("psport.commit", psport_commit, is_commit),
        },
        system: system(host, log.and_then(|log| log.os), log.and_then(|log| log.gpu.as_ref())),
        game: crate::schema::Game {
            title_id,
            name: keep("game.name", name, |n| {
                n.len() <= 128 && text::check(n, Mode::Name).is_empty()
            }),
            version: keep("game.version", game_version, is_game_version),
            profile: log.is_some_and(|log| log.profile),
        },
        settings: log
            .map(|log| {
                log.settings
                    .iter()
                    .filter(|&(&setting, value)| is_setting_value(setting, value))
                    .map(|(&setting, value)| (setting, value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        failure,
        calls,
        fault: log.and_then(|log| log.fault.as_ref()).and_then(|(_, f)| fault(f)),
        perf: log.and_then(|log| log.perf.as_ref()).and_then(perf),
    }));
    check(&report)?;
    Ok(report)
}

/// # Errors
///
/// The rejections of [`check`].
pub fn decanter_report(failure_entry: &DayFailure, host: &Host) -> Result<Report, Vec<Rejection>> {
    let kind = if failure_entry.action == Action::Launch && failure_entry.message.contains(" exit=") {
        FailureKind::Exit
    } else {
        FailureKind::Error
    };
    let report = Report::Decanter(DecanterReport {
        schema: SCHEMA,
        product: Product::Decanter,
        date: if is_day(&failure_entry.day) {
            failure_entry.day.clone()
        } else {
            day_of(host.now)
        },
        decanter: decanter_build(host),
        system: system(host, None, None),
        action: failure_entry.action,
        failure: failure(kind, &failure_entry.message, failure_entry.count),
    });
    check(&report)?;
    Ok(report)
}
