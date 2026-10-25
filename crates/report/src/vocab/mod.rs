//! The closed vocabulary of failure messages (decision #1034).
//!
//! A shape allow-list cannot tell a one-word user or host name from a technical word, so a
//! message is not free text: it is one of the templates below with typed slots, or it is not
//! reported at all. Adding a message is adding one entry to a table (`t(id, pattern)`; the
//! pattern language is in [`pattern`]). The same tables classify on the client and validate on
//! the server.

mod cause;
mod classes;
mod decanter;
mod pattern;
mod psport;

use pattern::Kind;

use crate::schema::{Cause, Message, Slot};
use crate::validate::{Rejection, Rule};

pub struct Template {
    pub id: &'static str,
    pub pattern: &'static str,
}

const fn t(id: &'static str, pattern: &'static str) -> Template {
    Template { id, pattern }
}

/// Lines longer than this are never matched; a failure line is a few hundred bytes at most.
const MAX_LINE: usize = 4096;

/// Which tables a message may draw from: a report's product decides it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Psport,
    Decanter,
}

impl Family {
    const fn tables(self) -> Tables {
        match self {
            Self::Psport => &[psport::TEMPLATES],
            Self::Decanter => decanter::TABLES,
        }
    }

    fn templates(self) -> impl Iterator<Item = &'static Template> {
        self.tables().iter().flat_map(|table| table.iter())
    }
}

type Tables = &'static [&'static [Template]];

const CAUSES: Tables = &[cause::TEMPLATES];

fn find(id: &str, tables: Tables) -> Option<&'static Template> {
    tables
        .iter()
        .flat_map(|table| table.iter())
        .find(|template| template.id == id)
}

fn flatten(raw: &str) -> Option<String> {
    (raw.len() <= MAX_LINE).then(|| raw.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The cause text of a decanter line as a template of its own; `None` leaves it out.
fn cause_of(text: &str) -> Option<Cause> {
    cause::TEMPLATES.iter().find_map(|template| {
        let segments = pattern::parse(template.pattern)?;
        let (slots, rest) = pattern::run(&segments, text)?;
        rest.is_empty().then(|| Cause {
            template: template.id.to_owned(),
            slots,
        })
    })
}

/// The template a failure line is an instance of, with its slot values; `None` when the line
/// matches no template, so the report says only the failure's kind.
#[must_use]
pub fn normalize(raw: &str, family: Family) -> Option<Message> {
    let line = flatten(raw)?;
    family.templates().find_map(|template| {
        let segments = pattern::parse(template.pattern)?;
        let (slots, rest) = pattern::run(&segments, &line)?;
        let cause = match (family, rest) {
            (_, "") => None,
            (Family::Decanter, tail) => cause_of(tail.strip_prefix(": ")?),
            (Family::Psport, _) => return None,
        };
        Some(Message {
            template: template.id.to_owned(),
            slots,
            cause,
        })
    })
}

fn kinds(template: &Template) -> Option<Vec<Kind<'_>>> {
    Some(
        pattern::parse(template.pattern)?
            .into_iter()
            .filter_map(|segment| match segment {
                pattern::Segment::Slot(Kind::Drop) | pattern::Segment::Literal(_) => None,
                pattern::Segment::Slot(kind) => Some(kind),
            })
            .collect(),
    )
}

fn check_part(path: &str, id: &str, slots: &[Slot], tables: Tables, out: &mut Vec<Rejection>) {
    let Some(template) = find(id, tables) else {
        out.push(Rejection::new(format!("{path}.template"), Rule::UnknownTemplate));
        return;
    };
    let Some(kinds) = kinds(template) else {
        out.push(Rejection::new(format!("{path}.template"), Rule::UnknownTemplate));
        return;
    };
    if kinds.len() != slots.len() {
        out.push(Rejection::new(format!("{path}.slots"), Rule::BadSlot));
        return;
    }
    for (index, (kind, slot)) in kinds.into_iter().zip(slots).enumerate() {
        if !pattern::fits(kind, slot) {
            out.push(Rejection::new(format!("{path}.slots[{index}]"), Rule::BadSlot));
        }
    }
}

/// Every way a received message departs from the vocabulary of its product.
pub fn check(message: &Message, family: Family, out: &mut Vec<Rejection>) {
    let path = "failure.message";
    check_part(path, &message.template, &message.slots, family.tables(), out);
    if let Some(cause) = &message.cause {
        if family == Family::Psport {
            out.push(Rejection::new(format!("{path}.cause"), Rule::UnknownTemplate));
        }
        check_part(&format!("{path}.cause"), &cause.template, &cause.slots, CAUSES, out);
    }
}
