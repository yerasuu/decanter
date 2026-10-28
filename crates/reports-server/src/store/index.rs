//! The lists the pages and the sweeps read, as s3db `MultiIndex`es whose member names sort the way they are read:
//! object stores list keys in ascending byte order only, so newest-first lists use inverted days and sequences.
//!
//! Entries are written before the transaction that makes them true and removed after the one that makes them false,
//! so an entry may be stale but a live report is never missing from a list. Every reader checks what an entry names
//! against the report or group it points at and skips what no longer matches; the sweep removes the leftovers.

use s3db::{MultiIndex, Segment, ordinal};

use super::model::Report;

const ROOT: &str = "idx";
/// Newest first by received day: `<inverted day>/<inverted seq>-<id>`.
pub static ALL: MultiIndex = MultiIndex::new(ROOT, "all", Segment::Raw);
pub static BY_FINGERPRINT: MultiIndex = MultiIndex::new(ROOT, "fp", Segment::Raw);
pub static BY_PRODUCT: MultiIndex = MultiIndex::new(ROOT, "product", Segment::Raw);
pub static BY_TITLE: MultiIndex = MultiIndex::new(ROOT, "title", Segment::Raw);
/// Oldest first: the work lists of eviction and retention (`open`, `attached`: `<seq>-<id>`; `resolved`:
/// `<resolved day>/<id>`), reports whose removal still has objects to delete (`gone`: `<id>`), and inserts whose
/// index entries were written but whose transaction has not been seen to land (`pending`: `<seq>-<id>`).
pub static QUEUE: MultiIndex = MultiIndex::new(ROOT, "queue", Segment::Raw);
/// Groups newest first by their latest received day: `<inverted day>/<fingerprint>`.
pub static GROUPS: MultiIndex = MultiIndex::new(ROOT, "groups", Segment::Raw);
/// The groups that hold reports of one title: `<title>/<fingerprint>`.
pub static GROUP_TITLES: MultiIndex = MultiIndex::new(ROOT, "gtitle", Segment::Raw);

pub const OPEN: &str = "open";
pub const ATTACHED: &str = "attached";
pub const RESOLVED: &str = "resolved";
pub const GONE: &str = "gone";
pub const PENDING: &str = "pending";
pub const GROUP_ORDER: &str = "order";

/// One index entry: which index, which value, which member.
pub struct Entry {
    pub index: &'static MultiIndex,
    pub value: String,
    pub member: String,
}

impl Entry {
    fn new(index: &'static MultiIndex, value: impl Into<String>, member: impl Into<String>) -> Self {
        Self {
            index,
            value: value.into(),
            member: member.into(),
        }
    }
}

/// `YYYY-MM-DD` as eight digits that sort newest first. Days come from the server's clock or a validated filter, so
/// anything else is a bug and sorts last.
pub fn inverted_day(day: &str) -> String {
    let digits: String = day.chars().filter(char::is_ascii_digit).collect();
    let value = digits.parse::<u32>().unwrap_or(0);
    format!("{:08}", 99_999_999_u32.saturating_sub(value))
}

pub fn ordered_member(seq: u64, id: &str) -> String {
    format!("{}-{id}", ordinal::encode_u64(seq))
}

pub fn newest_member(seq: u64, id: &str) -> String {
    format!("{}-{id}", ordinal::encode_u64(u64::MAX - seq))
}

/// The `(seq, id)` a member written by `ordered_member` or `newest_member` names, with the day prefix (if any)
/// already removed.
pub fn parse_member(member: &str, newest_first: bool) -> Option<(u64, &str)> {
    let leaf = member.rsplit('/').next()?;
    let (seq, id) = leaf.split_once('-')?;
    let seq = ordinal::decode_u64(seq)?;
    Some((if newest_first { u64::MAX - seq } else { seq }, id))
}

/// The entries that list `report` where it belongs today; a removal deletes every one of them.
pub fn entries_of(report: &Report) -> Vec<Entry> {
    let day = inverted_day(&report.received_day);
    let dated = format!("{day}/{}", newest_member(report.seq, &report.id));
    let ordered = ordered_member(report.seq, &report.id);
    let mut entries = Vec::with_capacity(6);
    entries.push(Entry::new(&ALL, day, newest_member(report.seq, &report.id)));
    entries.push(Entry::new(&BY_FINGERPRINT, report.fingerprint.as_str(), dated.as_str()));
    entries.push(Entry::new(&BY_PRODUCT, report.product.as_str(), dated.as_str()));
    if let Some(title) = &report.title_id {
        entries.push(Entry::new(&BY_TITLE, title.as_str(), dated));
    }
    match &report.resolved_at {
        None => entries.push(Entry::new(&QUEUE, OPEN, ordered.as_str())),
        Some(day) => entries.push(Entry::new(&QUEUE, RESOLVED, format!("{day}/{}", report.id))),
    }
    if report.attachment.is_some() {
        entries.push(Entry::new(&QUEUE, ATTACHED, ordered));
    }
    entries
}

pub fn group_order_member(last_received: &str, fingerprint: &str) -> String {
    format!("{}/{fingerprint}", inverted_day(last_received))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverted_days_and_sequences_sort_newest_first() {
        assert!(inverted_day("2026-10-27") < inverted_day("2026-10-26"));
        assert!(newest_member(10, "a") < newest_member(9, "a"));
        assert!(ordered_member(9, "a") < ordered_member(10, "a"));
        assert_eq!(
            parse_member(&format!("x/{}", newest_member(7, "id")), true),
            Some((7, "id"))
        );
        assert_eq!(parse_member(&ordered_member(7, "id"), false), Some((7, "id")));
    }
}
