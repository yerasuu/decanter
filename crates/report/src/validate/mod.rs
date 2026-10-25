//! The one gate a report passes.
//!
//! On the client before it is sent and on the server before it is published (decision #988): the
//! bytes must parse into the closed shape of [`crate::schema`], then every field must fit its
//! rule. Any miss rejects the whole report.

mod locate;
pub(crate) mod rules;

use serde::{Deserialize, Serialize};

use crate::schema::{DecanterReport, PsportReport, Report};
use crate::text::{self, Private};

pub use rules::check;

/// A report is a few kilobytes; anything larger is refused before it is parsed.
pub const MAX_BYTES: usize = 64 * 1024;

/// Which field failed and which rule; never the value, since a rejection is shown and may be
/// logged, and the value is what could be private.
#[derive(Debug, Serialize)]
pub struct Rejection {
    /// `failure.message`, `calls.failed[3].name`; empty for the report as a whole.
    pub path: String,
    pub rule: Rule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    TooLarge,
    Syntax,
    UnknownField,
    MissingField,
    DuplicateField,
    UnknownVariant,
    WrongType,
    /// `product` names no shape, or not the one the fields belong to.
    Product,
    Schema,
    TooLong,
    TooMany,
    OutOfRange,
    /// The value does not have the one shape its field allows (`CUSA00000`, `YYYY-MM-DD`).
    Pattern,
    Text(Private),
}

impl Rejection {
    pub(crate) fn new(path: impl Into<String>, rule: Rule) -> Self {
        Self {
            path: path.into(),
            rule,
        }
    }
}

#[derive(Deserialize)]
struct Peek {
    product: Option<String>,
}

/// Parses and checks a report as received.
///
/// # Errors
///
/// Every rejection found: the shape is checked first and stops at its first miss, since nothing
/// past it parsed; field rules then report all their misses at once.
pub fn validate(bytes: &[u8]) -> Result<Report, Vec<Rejection>> {
    if bytes.len() > MAX_BYTES {
        return Err(vec![Rejection::new("", Rule::TooLarge)]);
    }
    let shape = |error: &serde_json::Error| vec![locate::rejection(bytes, error)];
    let peek: Peek = serde_json::from_slice(bytes).map_err(|error| shape(&error))?;
    let report = match peek.product.as_deref() {
        Some("decanter") => serde_json::from_slice::<DecanterReport>(bytes).map(Report::Decanter),
        Some("psport") => serde_json::from_slice::<PsportReport>(bytes).map(|report| Report::Psport(Box::new(report))),
        _ => return Err(vec![Rejection::new("product", Rule::Product)]),
    }
    .map_err(|error| shape(&error))?;
    check(&report)?;
    Ok(report)
}

pub(crate) fn text_rejections(path: &str, value: &str, mode: text::Mode, out: &mut Vec<Rejection>) {
    out.extend(
        text::check(value, mode)
            .into_iter()
            .map(|reason| Rejection::new(path, Rule::Text(reason))),
    );
}
