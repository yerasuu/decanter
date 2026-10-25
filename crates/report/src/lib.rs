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
pub mod schema;
pub mod sources;
mod text;
pub mod validate;

use sha2::{Digest, Sha256};

pub use build::{Host, PsportInput, decanter_report, psport_report};
pub use schema::Report;
pub use text::{Private, normalize_message};
pub use validate::{Rejection, Rule, check, validate};

use schema::FailureKind;

/// A short key that groups duplicates.
///
/// The same kind of failure, with the same message, on the same commit. Standalone decimal
/// numbers (counters, return codes, line numbers) are left out of the key so one failure
/// repeating with a different count groups as one; hex offsets stay.
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
    let key = format!(
        "{kind}\n{}\n{}",
        without_numbers(&failure.message),
        commit.unwrap_or_default()
    );
    Sha256::digest(key)
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut hex, byte| {
            hex.push_str(&format!("{byte:02x}"));
            hex
        })
}

fn without_numbers(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    let mut previous = ' ';
    while let Some(c) = rest.chars().next() {
        let run = if c.is_ascii_digit() {
            rest.find(|d: char| !d.is_ascii_digit()).unwrap_or(rest.len())
        } else {
            c.len_utf8()
        };
        let (piece, tail) = rest.split_at(run);
        let standalone = !previous.is_ascii_alphanumeric() && !tail.starts_with(|n: char| n.is_ascii_alphanumeric());
        out.push_str(if c.is_ascii_digit() && standalone { "#" } else { piece });
        previous = piece.chars().last().unwrap_or(c);
        rest = tail;
    }
    out
}
