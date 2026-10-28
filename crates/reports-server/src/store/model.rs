//! What the bucket holds: one `Report` per id, one `Group` per fingerprint with the counters the index shows, and
//! the server-wide `Totals`. Only days are stored, never an IP, a user agent or a time of day.

use std::collections::BTreeMap;

use s3db::Entity;
use serde::{Deserialize, Serialize};

/// Titles one group tracks by name; reports of further titles only count in `other_titles`, so a group flooded with
/// made-up title ids cannot grow past s3db's single-object limit.
pub const MAX_TITLES: usize = 32;
/// Game names tracked per title, for the same reason.
pub const MAX_NAMES: usize = 8;

#[derive(Clone, Default, Serialize, Deserialize, Entity)]
#[entity(collection = "reports")]
pub struct Report {
    #[entity(key)]
    pub id: String,
    /// Insertion order from `Sequence`: the order the indexes sort by, since no time finer than a day is kept.
    pub seq: u64,
    pub delete_hash: String,
    pub fingerprint: String,
    pub product: String,
    pub title_id: Option<String>,
    pub game_name: Option<String>,
    pub failure_kind: String,
    pub template_id: String,
    pub message: String,
    pub created_day: String,
    pub received_day: String,
    pub hidden: bool,
    pub resolved_at: Option<String>,
    /// Blob name of the private attachment.
    pub attachment: Option<String>,
    /// Blob name of the public clean version of the attachment, which outlives the attachment (resolve and
    /// eviction keep it, since it holds only allow-listed data) and goes with the report.
    #[serde(default)]
    pub clean_log: Option<String>,
    /// Set in the transaction that removes the report, because s3db transactions cannot delete: the object itself,
    /// its index entries and its blob go right after, and the sweep finishes what a crash left.
    pub deleted: bool,
    /// What of this report its group's capped lists counted, so taking it out decrements exactly what putting it in
    /// incremented.
    pub tracked: Tracked,
    /// The validated report, byte for byte as received.
    pub json: String,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub enum Tracked {
    #[default]
    Nothing,
    Title,
    TitleAndName,
}

impl Report {
    pub const fn is_public(&self) -> bool {
        !self.deleted && !self.hidden
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TitleStats {
    pub count: u32,
    pub resolved: u32,
    pub names: BTreeMap<String, u32>,
    pub other_names: u32,
}

/// Counters over the group's visible reports (not hidden, not deleted), kept in the same transaction as every change
/// to one of its reports so `GET /` reads one object per group instead of scanning reports.
#[derive(Clone, Default, Serialize, Deserialize, Entity)]
#[entity(collection = "groups")]
pub struct Group {
    #[entity(key)]
    pub fingerprint: String,
    pub product: String,
    pub failure_kind: String,
    /// Message of the latest report added; the fingerprint fixes its template and named slots.
    pub message: String,
    /// Every report not deleted, hidden ones included.
    pub total: u32,
    pub visible: u32,
    pub resolved: u32,
    pub hidden: u32,
    /// Range of the visible reports' `created_day`. It only widens while reports come and go, because a minimum
    /// cannot be undone without a scan; it resets once no visible report is left.
    pub first_day: Option<String>,
    pub last_day: Option<String>,
    /// Latest `received_day` the group ever had: the key of its entry in the group order index.
    pub last_received: String,
    pub titles: BTreeMap<String, TitleStats>,
    pub other_titles: u32,
}

impl Group {
    /// Adds a report's contribution; decides (and records on the report) whether its title and name fit the caps.
    pub fn account(&mut self, report: &mut Report) {
        if report.deleted {
            return;
        }
        self.total += 1;
        if self.product.is_empty() {
            self.product.clone_from(&report.product);
            self.failure_kind.clone_from(&report.failure_kind);
        }
        if report.received_day > self.last_received {
            self.last_received.clone_from(&report.received_day);
        }
        if report.hidden {
            self.hidden += 1;
            return;
        }
        self.visible += 1;
        let resolved = u32::from(report.resolved_at.is_some());
        self.resolved += resolved;
        widen(&mut self.first_day, &report.created_day, |new, old| new < old);
        widen(&mut self.last_day, &report.created_day, |new, old| new > old);
        let Some(title) = &report.title_id else {
            return;
        };
        if self.titles.len() >= MAX_TITLES && !self.titles.contains_key(title) {
            report.tracked = Tracked::Nothing;
            self.other_titles += 1;
            return;
        }
        let stats = self.titles.entry(title.clone()).or_default();
        stats.count += 1;
        stats.resolved += resolved;
        report.tracked = match &report.game_name {
            Some(name) if stats.names.len() < MAX_NAMES || stats.names.contains_key(name) => {
                *stats.names.entry(name.clone()).or_default() += 1;
                Tracked::TitleAndName
            }
            _ => {
                stats.other_names += 1;
                Tracked::Title
            }
        };
    }

    /// Takes back exactly what `account` added for this report as it was then.
    pub fn unaccount(&mut self, report: &Report) {
        if report.deleted {
            return;
        }
        self.total = self.total.saturating_sub(1);
        if report.hidden {
            self.hidden = self.hidden.saturating_sub(1);
            return;
        }
        self.visible = self.visible.saturating_sub(1);
        let resolved = u32::from(report.resolved_at.is_some());
        self.resolved = self.resolved.saturating_sub(resolved);
        if self.visible == 0 {
            self.first_day = None;
            self.last_day = None;
        }
        let Some(title) = &report.title_id else {
            return;
        };
        if matches!(report.tracked, Tracked::Nothing) {
            self.other_titles = self.other_titles.saturating_sub(1);
            return;
        }
        let Some(stats) = self.titles.get_mut(title) else {
            return;
        };
        stats.count = stats.count.saturating_sub(1);
        stats.resolved = stats.resolved.saturating_sub(resolved);
        match (&report.game_name, report.tracked) {
            (Some(name), Tracked::TitleAndName) => {
                if let Some(count) = stats.names.get_mut(name) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        stats.names.remove(name);
                    }
                }
            }
            _ => stats.other_names = stats.other_names.saturating_sub(1),
        }
        if stats.count == 0 {
            self.titles.remove(title);
        }
    }

    /// The title most visible reports name (ties to the lowest id), and the name most of those give it.
    pub fn dominant_game(&self) -> (Option<&str>, Option<&str>) {
        let Some((title, stats)) = self
            .titles
            .iter()
            .max_by(|(ta, a), (tb, b)| a.count.cmp(&b.count).then_with(|| tb.cmp(ta)))
        else {
            return (None, None);
        };
        let name = stats
            .names
            .iter()
            .max_by(|(na, a), (nb, b)| a.cmp(b).then_with(|| nb.cmp(na)))
            .map(|(name, _)| name.as_str());
        (Some(title.as_str()), name)
    }

    /// Distinct titles among the visible reports; untracked ones count as one more at least.
    pub fn distinct_titles(&self) -> u32 {
        let tracked = u32::try_from(self.titles.len()).unwrap_or(u32::MAX);
        tracked + u32::from(self.other_titles > 0)
    }
}

fn widen(slot: &mut Option<String>, day: &str, wider: impl Fn(&str, &str) -> bool) {
    match slot {
        Some(current) if !wider(day, current) => {}
        _ => *slot = Some(day.to_owned()),
    }
}

/// The server-wide counters the caps are checked against. Every insert, removal and attachment change goes through it
/// in its transaction.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub reports: u32,
    pub attachments: u32,
}

/// The next insertion number. Never written inside a transaction: on S3 a transaction does not lock against plain
/// writes, and a plain write landing mid-transaction would be overwritten.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Sequence {
    pub next: u64,
}

/// What the previous sweep saw. An insert whose sequence is below `sequence` was in flight a whole sweep ago (or in
/// a process that has since stopped), so its leftover entries cannot belong to a running upload.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SweepMark {
    pub day: String,
    pub sequence: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(title: &str, name: &str) -> Report {
        Report {
            title_id: Some(title.to_owned()),
            game_name: Some(name.to_owned()),
            created_day: "2026-10-21".to_owned(),
            ..Report::default()
        }
    }

    #[test]
    fn unaccount_undoes_account_even_past_the_caps() {
        let mut group = Group::default();
        let mut reports: Vec<Report> = (0..MAX_TITLES + 5)
            .map(|n| report(&format!("CUSA{n:05}"), "G"))
            .chain((0..MAX_NAMES + 3).map(|n| report("CUSA00000", &format!("name {n}"))))
            .collect();
        for report in &mut reports {
            group.account(report);
        }
        assert_eq!(group.titles.len(), MAX_TITLES);
        assert_eq!(group.other_titles, 5);
        assert_eq!(group.titles["CUSA00000"].names.len(), MAX_NAMES);
        for report in &reports {
            group.unaccount(report);
        }
        assert_eq!((group.total, group.visible, group.other_titles), (0, 0, 0));
        assert!(group.titles.is_empty() && group.first_day.is_none());
    }

    #[test]
    fn hidden_reports_count_only_as_hidden() {
        let mut group = Group::default();
        let mut hidden = Report {
            hidden: true,
            ..report("CUSA00001", "G")
        };
        group.account(&mut hidden);
        assert_eq!((group.total, group.visible, group.hidden), (1, 0, 1));
        assert!(group.dominant_game().0.is_none());
    }
}
