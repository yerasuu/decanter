//! Error reports that are safe to publish (decisions #988, #989, #991).
//!
//! A report is never a raw log: it is a closed structure built from an allow-list of fields, with
//! free text reduced to token shapes a failure line is made of. The same code builds reports in
//! decanter and validates them on the receiving server, so both sides agree on what a public
//! report may hold.
//!
//! Nothing here sends, stores or reads files: decanter hands the sources' text in and decides
//! what to do with the report.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "serde's derived visitors for the report structs cost more than the 512-byte threshold"
)]

mod build;
mod clean;
mod identity;
pub mod schema;
pub mod sources;
mod text;
pub mod validate;
mod vocab;

use sha2::{Digest, Sha256};

pub use build::{Host, PsportInput, decanter_report, psport_report};
pub use clean::{
    CLEAN_VERSION, CleanLine, CleanLog, MAX_CLEAN_BYTES, MAX_CLEAN_INPUT, MAX_CLEAN_LINES, clean_log, message_text,
    validate_clean_log,
};
pub use identity::{Guard, Identity};
pub use schema::Report;
pub use text::{Private, normalize_message};
pub use validate::{Rejection, Rule, check, validate};

use schema::{FailureKind, Slot};

/// A short key that groups duplicates.
///
/// The same kind of failure, with the same message template and named slots, on the same commit.
/// Numeric slots (counters, return codes, offsets) are left out of the key so one failure
/// repeating with a different count groups as one.
#[must_use]
pub fn fingerprint(report: &Report) -> String {
    let (failure, commit) = match report {
        Report::Decanter(report) => (&report.failure, report.decanter.commit.as_deref()),
        Report::Psport(report) => (&report.failure, report.psport.commit.as_deref()),
    };
    let kind = match failure.kind {
        FailureKind::Crash => "crash",
        FailureKind::Stop => "stop",
        FailureKind::Error => "error",
        FailureKind::Exit => "exit",
        FailureKind::Missing => "missing",
    };
    let key = format!("{kind}\n{}\n{}", message_key(failure), commit.unwrap_or_default());
    Sha256::digest(key)
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut hex, byte| {
            hex.push_str(&format!("{byte:02x}"));
            hex
        })
}

/// The template and the slots that name something (a call, an image, a class); the numbers are
/// left out, so a failure repeating with another count or offset groups as one.
fn message_key(failure: &schema::Failure) -> String {
    let Some(message) = &failure.message else {
        return String::new();
    };
    let named = |slots: &[Slot]| {
        slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Number(_) | Slot::Offset(_) | Slot::Code(_) => None,
                Slot::Call(s)
                | Slot::Version(s)
                | Slot::Image(s)
                | Slot::Vuid(s)
                | Slot::Const(s)
                | Slot::Class(s)
                | Slot::Choice(s) => Some(s.as_str()),
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let cause = message.cause.as_ref().map_or_else(String::new, |cause| {
        format!("{}[{}]", cause.template, named(&cause.slots))
    });
    format!("{}[{}]{cause}", message.template, named(&message.slots))
}
