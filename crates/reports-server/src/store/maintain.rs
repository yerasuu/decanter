//! Eviction at the caps and the daily sweep: retention, removals and inserts a failure left half done, blobs nothing
//! refers to, and index entries that no longer hold. The work lists are oldest first, so nothing here scans every
//! report to find the oldest one.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use s3db::Blob;

use super::index::{self, ATTACHED, Entry, GONE, GROUP_ORDER, GROUP_TITLES, GROUPS, OPEN, PENDING, QUEUE, RESOLVED};
use super::model::{Group, Report, Sequence, SweepMark};
use super::write::Change;
use super::{ATTACHMENTS, CLEAN_LOGS, LAST_SWEEP, SEQUENCE, Store};

const PAGE_KEYS: usize = 200;
/// A blob younger than this may belong to an upload whose transaction is still running.
const ORPHAN_GRACE: Duration = Duration::from_hours(1);

#[derive(Default)]
pub struct Swept {
    pub unresolved: usize,
    pub resolved: usize,
    pub attachments: usize,
    pub orphans: usize,
    pub repaired: usize,
}

/// What a walk over an oldest-first queue does with a live entry.
enum Step {
    Take,
    Stop,
}

impl Store {
    /// Walks the `<seq>-<id>` queue oldest first, removing entries that no longer hold, until `want` reports were
    /// taken or `step` says stop.
    async fn oldest(
        &self,
        queue: &str,
        want: usize,
        holds: impl Fn(&Report) -> bool,
        step: impl Fn(&Report) -> Step,
    ) -> Result<Vec<String>> {
        let prefix = QUEUE.members_prefix(queue)?;
        let mut taken = Vec::new();
        let mut after: Option<String> = None;
        loop {
            // An eviction wants a few reports: listing (and loading) a whole page for it would read far more.
            let limit = want.saturating_sub(taken.len()).saturating_add(4).min(PAGE_KEYS);
            let (members, next) = self.page(&prefix, after.as_deref(), limit).await?;
            let parsed: Vec<(u64, &str)> = members
                .iter()
                .filter_map(|member| index::parse_member(member, false))
                .collect();
            let ids: Vec<&str> = parsed.iter().map(|(_, id)| *id).collect();
            for ((seq, id), found) in parsed.iter().zip(self.reports(&ids).await?) {
                let live = found.filter(|(report, _)| report.seq == *seq && holds(report));
                let Some((report, _)) = live else {
                    self.remove_entry(&Entry {
                        index: &QUEUE,
                        value: queue.to_owned(),
                        member: index::ordered_member(*seq, id),
                    })
                    .await;
                    continue;
                };
                match step(&report) {
                    Step::Take => taken.push(report.id),
                    Step::Stop => return Ok(taken),
                }
                if taken.len() >= want {
                    return Ok(taken);
                }
            }
            match next {
                Some(next) => after = Some(next),
                None => return Ok(taken),
            }
        }
    }

    /// Resolved reports are the maintainer's record of what was fixed; only unresolved ones are sacrificed, oldest
    /// first (hidden ones included, as before).
    pub(super) async fn evict_oldest_unresolved(&self, want: usize) -> Result<usize> {
        let ids = self
            .oldest(OPEN, want, |report| report.resolved_at.is_none(), |_| Step::Take)
            .await?;
        Ok(self.change(&ids, &Change::Delete).await?.removed)
    }

    /// Detaches the oldest attachments of unresolved reports and keeps the reports.
    pub(super) async fn evict_oldest_attachments(&self, want: usize) -> Result<usize> {
        let ids = self
            .oldest(
                ATTACHED,
                want,
                |report| report.attachment.is_some() && report.resolved_at.is_none(),
                |_| Step::Take,
            )
            .await?;
        Ok(self.change(&ids, &Change::Detach).await?.attachments)
    }

    /// Every report of a group, hidden ones included, for the bulk admin actions.
    pub async fn group_ids(&self, fingerprint: &str) -> Result<Vec<String>> {
        let prefix = index::BY_FINGERPRINT.members_prefix(fingerprint)?;
        let mut ids = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let (members, next) = self.page(&prefix, after.as_deref(), PAGE_KEYS).await?;
            ids.extend(
                members
                    .iter()
                    .filter_map(|member| index::parse_member(member, true))
                    .map(|(_, id)| id.to_owned()),
            );
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Deletes unresolved reports received before `unresolved_cutoff` and, when given, reports resolved before
    /// `resolved_cutoff`, then repairs what failures left behind.
    ///
    /// # Errors
    ///
    /// The bucket fails; the parts already done stay done.
    pub async fn sweep(&self, today: &str, unresolved_cutoff: &str, resolved_cutoff: Option<&str>) -> Result<Swept> {
        let mut swept = Swept {
            repaired: self.finish_removals().await? + self.finish_inserts(today).await?,
            ..Swept::default()
        };
        let expired = self
            .oldest(
                OPEN,
                usize::MAX,
                |report| report.resolved_at.is_none(),
                |report| {
                    if report.received_day.as_str() < unresolved_cutoff {
                        Step::Take
                    } else {
                        Step::Stop
                    }
                },
            )
            .await?;
        let removed = self.change(&expired, &Change::Delete).await?;
        swept.unresolved = removed.removed;
        swept.attachments += removed.attachments;
        if let Some(cutoff) = resolved_cutoff {
            let ids = self.expired_resolved(cutoff).await?;
            let removed = self.change(&ids, &Change::Delete).await?;
            swept.resolved = removed.removed;
            swept.attachments += removed.attachments;
        }
        swept.orphans = self.remove_orphan_blobs(ORPHAN_GRACE).await?;
        swept.repaired += self.repair_group_entries().await?;
        Ok(swept)
    }

    async fn expired_resolved(&self, cutoff: &str) -> Result<Vec<String>> {
        let prefix = QUEUE.members_prefix(RESOLVED)?;
        let mut ids = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let (members, next) = self.page(&prefix, after.as_deref(), PAGE_KEYS).await?;
            for member in &members {
                let Some((day, id)) = member.split_once('/') else {
                    continue;
                };
                if day >= cutoff {
                    return Ok(ids);
                }
                match self.report(id).await? {
                    Some((report, _)) if report.resolved_at.as_deref() == Some(day) => ids.push(id.to_owned()),
                    _ => {
                        self.remove_entry(&Entry {
                            index: &QUEUE,
                            value: RESOLVED.to_owned(),
                            member: member.clone(),
                        })
                        .await;
                    }
                }
            }
            match next {
                Some(next) => after = Some(next),
                None => return Ok(ids),
            }
        }
    }

    /// Removals whose transaction landed but whose objects were not all deleted.
    async fn finish_removals(&self) -> Result<usize> {
        let prefix = QUEUE.members_prefix(GONE)?;
        let mut finished = 0;
        let mut after: Option<String> = None;
        loop {
            let (ids, next) = self.page(&prefix, after.as_deref(), PAGE_KEYS).await?;
            for id in &ids {
                let (report, _) = Report::get(self.backend(), &self.cipher, id).await?;
                self.purge(&report.unwrap_or_else(|| Report {
                    id: id.clone(),
                    ..Report::default()
                }))
                .await;
                finished += 1;
            }
            match next {
                Some(next) => after = Some(next),
                None => return Ok(finished),
            }
        }
    }

    /// Inserts that wrote their index entries and then lost their transaction or died. Only entries older than the
    /// previous sweep are touched, since a newer one may belong to an upload still running.
    async fn finish_inserts(&self, today: &str) -> Result<usize> {
        let (mark, _) = LAST_SWEEP.get::<SweepMark>(self.backend(), &self.cipher).await?;
        let (sequence, _) = SEQUENCE.get::<Sequence>(self.backend(), &self.cipher).await?;
        let prefix = QUEUE.members_prefix(PENDING)?;
        let members = self.members(&prefix).await?;
        let mut finished = 0;
        for member in members {
            let Some((seq, _)) = index::parse_member(&member, false) else {
                continue;
            };
            if seq >= mark.sequence {
                continue;
            }
            let Some(pending) = QUEUE
                .payload::<Report>(self.backend(), &self.cipher, PENDING, &member)
                .await?
            else {
                continue;
            };
            let landed = self
                .report(&pending.id)
                .await?
                .is_some_and(|(report, _)| report.seq == seq);
            if !landed {
                for entry in index::entries_of(&pending) {
                    self.remove_entry(&entry).await;
                }
            }
            self.remove_entry(&Entry {
                index: &QUEUE,
                value: PENDING.to_owned(),
                member,
            })
            .await;
            finished += 1;
        }
        let next = SweepMark {
            day: today.to_owned(),
            sequence: sequence.next,
        };
        LAST_SWEEP
            .update(self.backend(), &self.cipher, |mark: &mut SweepMark| {
                mark.clone_from(&next);
                Ok(s3db::Update::Write)
            })
            .await?;
        Ok(finished)
    }

    /// Blobs no report names: a crash between writing a blob and the transaction naming it, or a delete that did
    /// not land. Also the parts of blob generations no manifest names.
    pub(super) async fn remove_orphan_blobs(&self, grace: Duration) -> Result<usize> {
        let removed = self
            .remove_orphans_of(&ATTACHMENTS, grace, |report| report.attachment.as_deref())
            .await?;
        Ok(removed
            + self
                .remove_orphans_of(&CLEAN_LOGS, grace, |report| report.clean_log.as_deref())
                .await?)
    }

    /// The blobs of `blobs` older than `grace` whose report does not name them through `named`.
    async fn remove_orphans_of(
        &self,
        blobs: &Blob,
        grace: Duration,
        named: impl Fn(&Report) -> Option<&str>,
    ) -> Result<usize> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let grace = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
        let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        let mut removed = 0;
        for name in blobs.list_names(self.backend()).await? {
            let id = name.split_once('-').map_or(name.as_str(), |(id, _)| id);
            let referenced = self
                .report(id)
                .await?
                .is_some_and(|(report, _)| named(&report) == Some(name.as_str()));
            if referenced {
                continue;
            }
            let (manifest, _) = blobs.head(self.backend(), &self.cipher, &name).await?;
            if manifest.is_some_and(|manifest| now_ms.saturating_sub(manifest.written_at_ms) >= grace) {
                if let Err(error) = blobs.delete(self.backend(), &name).await {
                    tracing::warn!(target: "decanter_reports::store", %error, "orphan blob not removed");
                }
                removed += 1;
            }
        }
        blobs
            .sweep_all_orphan_parts(self.backend(), &self.cipher, grace)
            .await?;
        Ok(removed)
    }

    /// Puts every group with visible reports in the order index under its current day, and removes the entries of
    /// the order and title indexes that no longer match their group.
    async fn repair_group_entries(&self) -> Result<usize> {
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        let names = Group::COLLECTION.list_names(self.backend()).await?;
        for (name, group) in names.iter().zip(self.load_groups(&names).await?) {
            if let Some(group) = group {
                groups.insert(name.clone(), group);
            }
        }
        let mut repaired = 0;
        let mut live = HashSet::new();
        for group in groups.values().filter(|group| group.visible > 0) {
            let member = index::group_order_member(&group.last_received, &group.fingerprint);
            live.insert(member.clone());
            let entry = Entry {
                index: &GROUPS,
                value: GROUP_ORDER.to_owned(),
                member,
            };
            if !GROUPS.contains(self.backend(), &entry.value, &entry.member).await? {
                self.add_entry(&entry).await?;
                repaired += 1;
            }
        }
        let members = self.members(&GROUPS.members_prefix(GROUP_ORDER)?).await?;
        for member in members.into_iter().filter(|member| !live.contains(member)) {
            self.remove_entry(&Entry {
                index: &GROUPS,
                value: GROUP_ORDER.to_owned(),
                member,
            })
            .await;
            repaired += 1;
        }
        let root = format!("{}/{}/", GROUP_TITLES.root, GROUP_TITLES.kind);
        let members = self.members(&root).await?;
        for member in members {
            let Some((title, fp)) = member.split_once('/') else {
                continue;
            };
            if groups.get(fp).is_some_and(|group| group.titles.contains_key(title)) {
                continue;
            }
            self.remove_entry(&Entry {
                index: &GROUP_TITLES,
                value: title.to_owned(),
                member: fp.to_owned(),
            })
            .await;
            repaired += 1;
        }
        Ok(repaired)
    }
}
