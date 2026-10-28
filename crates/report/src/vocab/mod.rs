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

use pattern::{Kind, Segment};

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
pub const MAX_LINE: usize = 4096;

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
        instance(template.id, &segments, &line, family)
    })
}

fn instance(id: &str, segments: &[Segment<'_>], line: &str, family: Family) -> Option<Message> {
    let (slots, rest) = pattern::run(segments, line)?;
    let cause = match (family, rest) {
        (_, "") => None,
        (Family::Decanter, tail) => cause_of(tail.strip_prefix(": ")?),
        (Family::Psport, _) => return None,
    };
    Some(Message {
        template: id.to_owned(),
        slots,
        cause,
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
    check_at("failure.message", message, family, out);
}

/// [`check`] for a message that is not a report's failure (a line of a clean log), named by `path`.
pub fn check_at(path: &str, message: &Message, family: Family, out: &mut Vec<Rejection>) {
    check_part(path, &message.template, &message.slots, family.tables(), out);
    if let Some(cause) = &message.cause {
        if family == Family::Psport {
            out.push(Rejection::new(format!("{path}.cause"), Rule::UnknownTemplate));
        }
        check_part(&format!("{path}.cause"), &cause.template, &cause.slots, CAUSES, out);
    }
}

/// One family's templates parsed once, so a whole log is matched without parsing every pattern
/// again for each of its lines.
pub struct Matcher {
    family: Family,
    templates: Vec<(&'static str, Vec<Segment<'static>>)>,
}

impl Matcher {
    pub fn new(family: Family) -> Self {
        let templates = family
            .templates()
            .filter_map(|template| Some((template.id, pattern::parse(template.pattern)?)))
            .collect();
        Self { family, templates }
    }

    /// Same answer as [`normalize`].
    pub fn normalize(&self, raw: &str) -> Option<Message> {
        let line = flatten(raw)?;
        self.templates
            .iter()
            .find_map(|(id, segments)| instance(id, segments, &line, self.family))
    }
}

/// A template with its slots as the line reads: the pattern's literal text, each slot's value and
/// `…` where the line had text that is never reported. `None` when the template is unknown or a
/// slot does not fit it, so nothing that skipped [`check`] is ever rendered.
fn render_part(id: &str, slots: &[Slot], tables: Tables) -> Option<String> {
    let template = find(id, tables)?;
    let mut values = slots.iter();
    let mut out = String::with_capacity(template.pattern.len());
    for segment in pattern::parse(template.pattern)? {
        match segment {
            Segment::Literal(text) => out.push_str(text),
            Segment::Slot(Kind::Drop) => out.push('\u{2026}'),
            Segment::Slot(kind) => {
                let slot = values.next().filter(|slot| pattern::fits(kind, slot))?;
                out.push_str(&pattern::show(slot));
            }
        }
    }
    values.next().is_none().then_some(out)
}

/// The readable text of a message of `family`, its cause after `: ` as decanter's lines write it.
pub fn render(message: &Message, family: Family) -> Option<String> {
    let mut text = render_part(&message.template, &message.slots, family.tables())?;
    if let Some(cause) = &message.cause {
        if family == Family::Psport {
            return None;
        }
        text.push_str(": ");
        text.push_str(&render_part(&cause.template, &cause.slots, CAUSES)?);
    }
    Some(text)
}
