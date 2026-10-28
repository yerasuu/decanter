//! Every change: one transaction over the reports it touches, their groups and the totals, retried from a fresh
//! read when another writer got there first. Index entries are added before the transaction and removed after it,
//! and objects (blobs, removed reports) are deleted only once the transaction that stopped referring to them landed.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use s3db::txn_update::UpdateOp;
use s3db::{Cas, Update};

use super::index::{self, ATTACHED, Entry, GONE, GROUP_ORDER, GROUP_TITLES, GROUPS, OPEN, PENDING, QUEUE, RESOLVED};
use super::model::{Group, Report, Sequence, Totals};
use super::{ATTACHMENTS, BATCH, MAX_ATTEMPTS, SEQUENCE, Store, TOTALS, backoff, eviction_batch};
use crate::secrets::{Hash, hex, random_hex};

/// Everything the upload derives from a validated report.
pub struct NewReport {
    pub id: String,
    pub delete_hash: Hash,
    pub fingerprint: String,
    pub product: &'static str,
    pub title_id: Option<String>,
    pub game_name: Option<String>,
    pub failure_kind: String,
    pub template_id: String,
    pub message: String,
    pub created_day: String,
    pub received_day: String,
    pub report_json: String,
}

pub enum Insert {
    Stored { evicted: usize },
    Full,
}

pub enum Attach {
    Stored { evicted: usize },
    NotFound,
    Resolved,
    Exists,
    Full,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Change {
    Hide,
    Restore,
    /// Keeps a report's first resolution day; also drops its attachment.
    Resolve(String),
    Delete,
    /// Drops the attachment of an unresolved report, which stays.
    Detach,
}

/// `matched` counts the reports that exist, changed or not (hiding a hidden report still answers 204).
#[derive(Default)]
pub struct Changed {
    pub matched: usize,
    pub removed: usize,
    pub attachments: usize,
}

impl Changed {
    pub const fn absorb(&mut self, other: &Self) {
        self.matched += other.matched;
        self.removed += other.removed;
        self.attachments += other.attachments;
    }
}

impl Change {
    fn apply(&self, report: &Report) -> Report {
        let mut after = report.clone();
        match self {
            Self::Hide => after.hidden = true,
            Self::Restore => after.hidden = false,
            Self::Resolve(day) => {
                after.resolved_at.get_or_insert_with(|| day.clone());
                after.attachment = None;
            }
            Self::Delete => after.deleted = true,
            Self::Detach => after.attachment = None,
        }
        after
    }
}

fn op<T: serde::Serialize>(key: String, value: &T, etag: Option<&str>) -> Result<UpdateOp> {
    match etag {
        Some(etag) => UpdateOp::update(key, value, etag),
        None => UpdateOp::create(key, value),
    }
}

fn report_op(report: &Report, etag: Option<&str>) -> Result<UpdateOp> {
    op(Report::COLLECTION.key(&report.id)?, report, etag)
}

fn group_op(group: &Group, etag: Option<&str>) -> Result<UpdateOp> {
    op(Group::COLLECTION.key(&group.fingerprint)?, group, etag)
}

fn totals_op(totals: &Totals, etag: Option<&str>) -> Result<UpdateOp> {
    op(TOTALS.key.to_owned(), totals, etag)
}

impl Store {
    /// Stores a validated report, evicting the oldest unresolved reports first when the store is at `max_reports`.
    ///
    /// # Errors
    ///
    /// The bucket fails, or the transaction keeps losing races past `MAX_ATTEMPTS`.
    pub async fn insert(&self, new: &NewReport, max_reports: u32) -> Result<Insert> {
        let report = Report {
            id: new.id.clone(),
            seq: self.next_seq().await?,
            delete_hash: hex(&new.delete_hash),
            fingerprint: new.fingerprint.clone(),
            product: new.product.to_owned(),
            title_id: new.title_id.clone(),
            game_name: new.game_name.clone(),
            failure_kind: new.failure_kind.clone(),
            template_id: new.template_id.clone(),
            message: new.message.clone(),
            created_day: new.created_day.clone(),
            received_day: new.received_day.clone(),
            json: new.report_json.clone(),
            ..Report::default()
        };
        // The sequence is reserved, so the entries are written once and every retry below is only a re-read and a
        // commit: the window in which another upload can win the race on the totals stays a few round trips wide.
        let pending = Entry {
            index: &QUEUE,
            value: PENDING.to_owned(),
            member: index::ordered_member(report.seq, &report.id),
        };
        let entries = index::entries_of(&report);
        self.add_entry_with(&pending, &pending_payload(&report)).await?;
        for entry in &entries {
            self.add_entry(entry).await?;
        }
        let outcome = self.insert_report(&report, max_reports).await;
        if !matches!(outcome, Ok(Insert::Stored { .. })) {
            for entry in &entries {
                self.remove_entry(entry).await;
            }
        }
        self.remove_entry(&pending).await;
        outcome
    }

    async fn insert_report(&self, report: &Report, max_reports: u32) -> Result<Insert> {
        let mut evicted = 0;
        for attempt in 0..MAX_ATTEMPTS {
            let (totals, totals_etag) = self.totals().await?;
            if totals.reports >= max_reports {
                let want = totals.reports - max_reports + eviction_batch(max_reports);
                let freed = self.evict_oldest_unresolved(want as usize).await?;
                if freed == 0 {
                    return Ok(Insert::Full);
                }
                evicted += freed;
                continue;
            }
            let (group, group_etag) = Group::get(self.backend(), &self.cipher, &report.fingerprint).await?;
            let mut group = group.unwrap_or_else(|| Group {
                fingerprint: report.fingerprint.clone(),
                ..Group::default()
            });
            let mut report = report.clone();
            group.account(&mut report);
            group.message.clone_from(&report.message);
            self.add_group_entries(&group, report.title_id.as_deref()).await?;
            let next = Totals {
                reports: totals.reports + 1,
                ..totals
            };
            let ops = vec![
                report_op(&report, None)?,
                group_op(&group, group_etag.as_deref())?,
                totals_op(&next, totals_etag.as_deref())?,
            ];
            if self.commit(ops).await? {
                return Ok(Insert::Stored { evicted });
            }
            backoff(attempt).await;
        }
        bail!("insert: the transaction kept losing races")
    }

    /// Hands out insertion order from its own small counter, outside the transactions, so reserving a number is one
    /// compare-and-swap on a tiny object instead of a stake in the totals' race.
    async fn next_seq(&self) -> Result<u64> {
        let mut last_error = None;
        for attempt in 0..4 {
            match SEQUENCE
                .update(self.backend(), &self.cipher, |sequence: &mut Sequence| {
                    sequence.next += 1;
                    Ok(Update::Write)
                })
                .await
            {
                Ok(sequence) => return Ok(sequence.next - 1),
                Err(error) => last_error = Some(error),
            }
            backoff(attempt).await;
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("sequence not reserved")))
    }

    /// The group's place in the index and in its title's list; idempotent, stale ones are skipped by readers.
    async fn add_group_entries(&self, group: &Group, title: Option<&str>) -> Result<()> {
        let order = Entry {
            index: &GROUPS,
            value: GROUP_ORDER.to_owned(),
            member: index::group_order_member(&group.last_received, &group.fingerprint),
        };
        self.add_entry(&order).await?;
        if let Some(title) = title {
            let entry = Entry {
                index: &GROUP_TITLES,
                value: title.to_owned(),
                member: group.fingerprint.clone(),
            };
            self.add_entry(&entry).await?;
        }
        Ok(())
    }

    /// Applies `change` to every report of `ids` that exists, in transactions of at most `BATCH` reports.
    ///
    /// # Errors
    ///
    /// The bucket fails, or a transaction keeps losing races past `MAX_ATTEMPTS`.
    pub async fn change(&self, ids: &[String], change: &Change) -> Result<Changed> {
        let mut total = Changed::default();
        for chunk in ids.chunks(BATCH) {
            total.absorb(&self.change_batch(chunk, change).await?);
        }
        Ok(total)
    }

    async fn change_batch(&self, ids: &[String], change: &Change) -> Result<Changed> {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        for attempt in 0..MAX_ATTEMPTS {
            let found: Vec<(Report, String)> = self.reports(&refs).await?.into_iter().flatten().collect();
            let mut done = Changed {
                matched: found.len(),
                ..Changed::default()
            };
            let mut groups: BTreeMap<String, (Group, Option<String>)> = BTreeMap::new();
            let mut ops = Vec::with_capacity(found.len() + 2);
            let mut changed = Vec::with_capacity(found.len());
            for (before, etag) in found {
                let mut after = change.apply(&before);
                if !differs(&before, &after) {
                    continue;
                }
                if !groups.contains_key(&before.fingerprint) {
                    let (group, group_etag) = Group::get(self.backend(), &self.cipher, &before.fingerprint).await?;
                    let group = group.unwrap_or_else(|| Group {
                        fingerprint: before.fingerprint.clone(),
                        ..Group::default()
                    });
                    groups.insert(before.fingerprint.clone(), (group, group_etag));
                }
                if let Some((group, _)) = groups.get_mut(&before.fingerprint) {
                    group.unaccount(&before);
                    group.account(&mut after);
                }
                done.removed += usize::from(after.deleted);
                done.attachments += usize::from(before.attachment.is_some() && after.attachment.is_none());
                ops.push(report_op(&after, Some(&etag))?);
                if after.deleted {
                    let gone = QUEUE.entry_key(GONE, &after.id)?;
                    ops.push(UpdateOp::create(gone, &())?);
                }
                changed.push((before, after));
            }
            if changed.is_empty() {
                return Ok(done);
            }
            for (group, etag) in groups.values() {
                ops.push(group_op(group, etag.as_deref())?);
            }
            if done.removed + done.attachments > 0 {
                let (totals, etag) = self.totals().await?;
                let next = Totals {
                    reports: totals.reports.saturating_sub(count(done.removed)),
                    attachments: totals.attachments.saturating_sub(count(done.attachments)),
                };
                ops.push(totals_op(&next, etag.as_deref())?);
            }
            let resolved: Vec<Entry> = changed
                .iter()
                .filter(|(before, after)| before.resolved_at.is_none() && !after.deleted)
                .filter_map(|(_, after)| after.resolved_at.as_ref().map(|day| resolved_entry(day, &after.id)))
                .collect();
            for entry in &resolved {
                self.add_entry(entry).await?;
            }
            if self.commit(ops).await? {
                for (before, after) in &changed {
                    self.finish(before, after).await;
                    // The sweep drops the index entry of a group with nothing visible, so a restore puts it back.
                    if matches!(change, Change::Restore)
                        && let Some((group, _)) = groups.get(&before.fingerprint)
                        && let Err(error) = self.add_group_entries(group, before.title_id.as_deref()).await
                    {
                        tracing::warn!(target: "decanter_reports::store", %error, "group index entry not restored");
                    }
                }
                return Ok(done);
            }
            for entry in &resolved {
                self.remove_entry(entry).await;
            }
            backoff(attempt).await;
        }
        bail!("change: the transaction kept losing races")
    }

    /// What a landed change leaves to delete: the entries that no longer hold and the objects nothing refers to.
    async fn finish(&self, before: &Report, after: &Report) {
        if after.deleted {
            self.purge(before).await;
            return;
        }
        if before.resolved_at.is_none() && after.resolved_at.is_some() {
            self.remove_entry(&queue_entry(OPEN, before)).await;
        }
        if before.attachment.is_some() && after.attachment.is_none() {
            self.remove_entry(&queue_entry(ATTACHED, before)).await;
            if let Some(name) = &before.attachment {
                self.remove_blob(name).await;
            }
        }
    }

    /// Deletes a removed report's entries, blob and object, and only then its `gone` entry, so the sweep can finish
    /// whatever a failure here leaves.
    pub(super) async fn purge(&self, report: &Report) {
        for entry in index::entries_of(report) {
            self.remove_entry(&entry).await;
        }
        if let Some(name) = &report.attachment {
            self.remove_blob(name).await;
        }
        match Report::COLLECTION.key(&report.id) {
            Ok(key) => s3db::delete_object(self.backend(), &key).await,
            Err(error) => tracing::warn!(target: "decanter_reports::store", %error, "removed report not purged"),
        }
        self.remove_entry(&Entry {
            index: &QUEUE,
            value: GONE.to_owned(),
            member: report.id.clone(),
        })
        .await;
    }

    /// Stores `bytes` as the report's private attachment, evicting the oldest attachments first at `max_attachments`.
    /// The blob is written before the transaction that names it, so no report ever points at a missing blob; a blob
    /// whose transaction never landed is removed here or, after a crash, by the sweep.
    ///
    /// # Errors
    ///
    /// The bucket fails, or the transaction keeps losing races past `MAX_ATTEMPTS`.
    pub async fn attach(&self, id: &str, bytes: &[u8], max_attachments: u32) -> Result<Attach> {
        if let Some(refused) = attach_refusal(self.report(id).await?.as_ref().map(|(r, _)| r)) {
            return Ok(refused);
        }
        let name = format!("{id}-{}", random_hex()?);
        if ATTACHMENTS
            .put(self.backend(), &self.cipher, &name, bytes, None)
            .await?
            == Cas::Conflict
        {
            bail!("attachment name collision");
        }
        let outcome = self.attach_blob(id, &name, max_attachments).await;
        if !matches!(outcome, Ok(Attach::Stored { .. })) {
            self.remove_blob(&name).await;
        }
        outcome
    }

    async fn attach_blob(&self, id: &str, name: &str, max_attachments: u32) -> Result<Attach> {
        let mut evicted = 0;
        for attempt in 0..MAX_ATTEMPTS {
            let (totals, totals_etag) = self.totals().await?;
            if totals.attachments >= max_attachments {
                let want = totals.attachments - max_attachments + eviction_batch(max_attachments);
                let freed = self.evict_oldest_attachments(want as usize).await?;
                if freed == 0 {
                    return Ok(Attach::Full);
                }
                evicted += freed;
                continue;
            }
            let found = self.report(id).await?;
            if let Some(refused) = attach_refusal(found.as_ref().map(|(r, _)| r)) {
                return Ok(refused);
            }
            let Some((mut report, etag)) = found else {
                return Ok(Attach::NotFound);
            };
            report.attachment = Some(name.to_owned());
            let entry = queue_entry(ATTACHED, &report);
            self.add_entry(&entry).await?;
            let next = Totals {
                attachments: totals.attachments + 1,
                ..totals
            };
            let ops = vec![
                report_op(&report, Some(&etag))?,
                totals_op(&next, totals_etag.as_deref())?,
            ];
            if self.commit(ops).await? {
                return Ok(Attach::Stored { evicted });
            }
            self.remove_entry(&entry).await;
            backoff(attempt).await;
        }
        bail!("attach: the transaction kept losing races")
    }
}

const fn attach_refusal(report: Option<&Report>) -> Option<Attach> {
    match report {
        None => Some(Attach::NotFound),
        Some(report) if report.resolved_at.is_some() => Some(Attach::Resolved),
        Some(report) if report.attachment.is_some() => Some(Attach::Exists),
        Some(_) => None,
    }
}

fn differs(before: &Report, after: &Report) -> bool {
    before.hidden != after.hidden
        || before.resolved_at != after.resolved_at
        || before.attachment != after.attachment
        || before.deleted != after.deleted
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

pub(super) fn queue_entry(queue: &str, report: &Report) -> Entry {
    Entry {
        index: &QUEUE,
        value: queue.to_owned(),
        member: index::ordered_member(report.seq, &report.id),
    }
}

fn resolved_entry(day: &str, id: &str) -> Entry {
    Entry {
        index: &QUEUE,
        value: RESOLVED.to_owned(),
        member: format!("{day}/{id}"),
    }
}

/// What the sweep needs to find a failed insert's entries again: the fields `entries_of` reads, no contents.
fn pending_payload(report: &Report) -> Report {
    Report {
        id: report.id.clone(),
        seq: report.seq,
        fingerprint: report.fingerprint.clone(),
        product: report.product.clone(),
        title_id: report.title_id.clone(),
        received_day: report.received_day.clone(),
        ..Report::default()
    }
}
