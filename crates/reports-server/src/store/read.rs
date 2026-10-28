//! What the pages and the admin list read. Every list walks an index in the order it is shown and loads only what
//! it shows (plus what it skips for the page offset), checking each entry against the object it names.

use std::collections::HashSet;

use anyhow::Result;
use s3db::{BatchLoader, MemoryBudget};
use serde::Serialize;

use super::Store;
use super::index::{self, ALL, BY_FINGERPRINT, BY_PRODUCT, BY_TITLE, GROUP_ORDER, GROUP_TITLES, GROUPS};
use super::model::{Group, Report};

/// Index keys listed per round trip.
const PAGE_KEYS: usize = 200;
/// Groups one title page may sort; past it the oldest groups of that title are not shown. The title list is
/// sorted in memory because the order index is not per title.
const MAX_TITLE_GROUPS: usize = 10_000;

#[derive(Default)]
pub struct Filter {
    pub product: Option<&'static str>,
    pub title: Option<String>,
    pub resolved: Option<bool>,
}

#[derive(Default)]
pub struct AdminFilter {
    pub fingerprint: Option<String>,
    pub day: Option<String>,
    pub product: Option<&'static str>,
    pub title: Option<String>,
}

pub struct GroupRow {
    pub fingerprint: String,
    pub count: u32,
    pub first_day: String,
    pub last_day: String,
    pub resolved: u32,
    pub product: String,
    pub title_id: Option<String>,
    pub titles: u32,
    pub game_name: Option<String>,
    pub failure_kind: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct AdminRow {
    pub id: String,
    pub fingerprint: String,
    pub product: String,
    pub title_id: Option<String>,
    pub failure_kind: String,
    pub template_id: String,
    pub created_day: String,
    pub received_day: String,
    pub hidden: bool,
    pub resolved_at: Option<String>,
    pub has_attachment: bool,
}

impl From<Report> for AdminRow {
    fn from(report: Report) -> Self {
        Self {
            has_attachment: report.attachment.is_some(),
            id: report.id,
            fingerprint: report.fingerprint,
            product: report.product,
            title_id: report.title_id,
            failure_kind: report.failure_kind,
            template_id: report.template_id,
            created_day: report.created_day,
            received_day: report.received_day,
            hidden: report.hidden,
            resolved_at: report.resolved_at,
        }
    }
}

/// The row a group shows under `filter`, or `None` when none of its visible reports match. Counts follow the
/// filter; the day range and the game name are the group's whole visible range (see docs/design.md).
fn group_row(group: Group, filter: &Filter) -> Option<GroupRow> {
    if group.visible == 0 || filter.product.is_some_and(|product| product != group.product) {
        return None;
    }
    let (count, resolved, title_id, game_name, titles) = if let Some(title) = &filter.title {
        let stats = group.titles.get(title)?;
        let name = stats
            .names
            .iter()
            .max_by(|(na, a), (nb, b)| a.cmp(b).then_with(|| nb.cmp(na)))
            .map(|(name, _)| name.clone());
        (stats.count, stats.resolved, Some(title.clone()), name, 1)
    } else {
        let (title, name) = group.dominant_game();
        let (title, name) = (title.map(str::to_owned), name.map(str::to_owned));
        (group.visible, group.resolved, title, name, group.distinct_titles())
    };
    let (count, resolved) = match filter.resolved {
        None => (count, resolved),
        Some(false) => (count.saturating_sub(resolved), 0),
        Some(true) => (resolved, resolved),
    };
    (count > 0).then(|| GroupRow {
        count,
        resolved,
        title_id,
        game_name,
        titles,
        first_day: group.first_day.unwrap_or_default(),
        last_day: group.last_day.unwrap_or_default(),
        fingerprint: group.fingerprint,
        product: group.product,
        failure_kind: group.failure_kind,
        message: group.message,
    })
}

impl Store {
    pub(super) async fn load_groups(&self, fingerprints: &[String]) -> Result<Vec<Option<Group>>> {
        let keys = fingerprints
            .iter()
            .map(|fp| Group::COLLECTION.key(fp))
            .collect::<Result<Vec<_>>>()?;
        let mut loaded = BatchLoader::new(MemoryBudget::default())
            .load_many::<Group>(&self.backend, &self.cipher, keys.iter().cloned())
            .await;
        keys.iter().map(|key| loaded.remove(key).unwrap_or(Ok(None))).collect()
    }

    pub async fn find_public(&self, id: &str) -> Result<Option<Report>> {
        Ok(self
            .report(id)
            .await?
            .map(|(report, _)| report)
            .filter(Report::is_public))
    }

    /// The report's delete-code hash, for a report that exists.
    pub async fn delete_hash(&self, id: &str) -> Result<Option<String>> {
        Ok(self.report(id).await?.map(|(report, _)| report.delete_hash))
    }

    pub async fn attachment(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let Some(name) = self.report(id).await?.and_then(|(report, _)| report.attachment) else {
            return Ok(None);
        };
        Ok(super::ATTACHMENTS.get(&self.backend, &self.cipher, &name).await?.0)
    }

    /// Walks the newest-first entries under `prefix`, keeping the reports that still match what their entry says and
    /// that `keep` accepts; skips `skip` of them and returns at most `take`.
    async fn walk_reports(
        &self,
        prefix: &str,
        keep: impl Fn(&Report) -> bool,
        skip: usize,
        take: usize,
    ) -> Result<Vec<Report>> {
        let mut out = Vec::with_capacity(take.min(PAGE_KEYS));
        let mut skipped = 0;
        let mut after: Option<String> = None;
        loop {
            let wanted = (skip + take).saturating_sub(skipped + out.len());
            let limit = wanted.saturating_add(2).min(PAGE_KEYS);
            let (members, next) = self.page(prefix, after.as_deref(), limit).await?;
            let parsed: Vec<(u64, &str)> = members
                .iter()
                .filter_map(|member| index::parse_member(member, true))
                .collect();
            let ids: Vec<&str> = parsed.iter().map(|(_, id)| *id).collect();
            for ((seq, _), found) in parsed.iter().zip(self.reports(&ids).await?) {
                let Some((report, _)) = found else { continue };
                if report.seq != *seq || !keep(&report) {
                    continue;
                }
                if skipped < skip {
                    skipped += 1;
                } else {
                    out.push(report);
                    if out.len() >= take {
                        return Ok(out);
                    }
                }
            }
            match next {
                Some(next) => after = Some(next),
                None => return Ok(out),
            }
        }
    }

    /// The visible reports of a group, newest first.
    pub async fn group_members(&self, fingerprint: &str, limit: u32, offset: u32) -> Result<Vec<Report>> {
        let (group, _) = Group::get(self.backend(), &self.cipher, fingerprint).await?;
        if group.is_none_or(|group| group.visible == 0) {
            return Ok(Vec::new());
        }
        let prefix = BY_FINGERPRINT.members_prefix(fingerprint)?;
        self.walk_reports(
            &prefix,
            |report| report.is_public() && report.fingerprint == fingerprint,
            offset as usize,
            limit as usize,
        )
        .await
    }

    /// Hidden reports included, newest first. Walks the narrowest index the filter allows and checks the rest.
    pub async fn admin_list(&self, filter: &AdminFilter, limit: u32, offset: u32) -> Result<Vec<AdminRow>> {
        let day = filter.day.as_deref().map(index::inverted_day);
        let dated = |value: &str, index: &s3db::MultiIndex| -> Result<String> {
            let base = index.members_prefix(value)?;
            Ok(day.as_ref().map_or_else(|| base.clone(), |day| format!("{base}{day}/")))
        };
        let prefix = if let Some(fp) = &filter.fingerprint {
            dated(fp, &BY_FINGERPRINT)?
        } else if let Some(title) = &filter.title {
            dated(title, &BY_TITLE)?
        } else if let Some(product) = filter.product {
            dated(product, &BY_PRODUCT)?
        } else if let Some(day) = &day {
            ALL.members_prefix(day)?
        } else {
            format!("{}/{}/", ALL.root, ALL.kind)
        };
        let keep = |report: &Report| {
            filter.fingerprint.as_ref().is_none_or(|fp| *fp == report.fingerprint)
                && filter.day.as_ref().is_none_or(|day| *day == report.received_day)
                && filter.product.is_none_or(|product| product == report.product)
                && filter
                    .title
                    .as_ref()
                    .is_none_or(|title| report.title_id.as_ref() == Some(title))
        };
        let rows = self
            .walk_reports(&prefix, keep, offset as usize, limit as usize)
            .await?;
        Ok(rows.into_iter().map(AdminRow::from).collect())
    }

    /// One page of the index: `Group` rows only, never reports.
    pub async fn groups(&self, filter: &Filter, limit: u32, offset: u32) -> Result<Vec<GroupRow>> {
        match &filter.title {
            Some(title) => self.groups_of_title(title, filter, limit, offset).await,
            None => self.groups_in_order(filter, limit, offset).await,
        }
    }

    async fn groups_in_order(&self, filter: &Filter, limit: u32, offset: u32) -> Result<Vec<GroupRow>> {
        let prefix = GROUPS.members_prefix(GROUP_ORDER)?;
        let (skip, take) = (offset as usize, limit as usize);
        let mut out = Vec::with_capacity(take.min(PAGE_KEYS));
        let mut seen = HashSet::new();
        let mut skipped = 0;
        let mut after: Option<String> = None;
        loop {
            let (members, next) = self.page(&prefix, after.as_deref(), PAGE_KEYS).await?;
            let pointers: Vec<(&str, String)> = members
                .iter()
                .filter_map(|member| member.split_once('/'))
                .filter(|(_, fp)| seen.insert((*fp).to_owned()))
                .map(|(day, fp)| (day, fp.to_owned()))
                .collect();
            let fps: Vec<String> = pointers.iter().map(|(_, fp)| fp.clone()).collect();
            for ((day, _), group) in pointers.iter().zip(self.load_groups(&fps).await?) {
                let Some(group) = group else { continue };
                if index::inverted_day(&group.last_received) != *day {
                    continue;
                }
                let Some(row) = group_row(group, filter) else { continue };
                if skipped < skip {
                    skipped += 1;
                } else {
                    out.push(row);
                    if out.len() >= take {
                        return Ok(out);
                    }
                }
            }
            match next {
                Some(next) => after = Some(next),
                None => return Ok(out),
            }
        }
    }

    async fn groups_of_title(&self, title: &str, filter: &Filter, limit: u32, offset: u32) -> Result<Vec<GroupRow>> {
        let prefix = GROUP_TITLES.members_prefix(title)?;
        let mut fps = Vec::new();
        let mut after: Option<String> = None;
        while fps.len() < MAX_TITLE_GROUPS {
            let (members, next) = self.page(&prefix, after.as_deref(), PAGE_KEYS).await?;
            fps.extend(members);
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let mut groups: Vec<Group> = self.load_groups(&fps).await?.into_iter().flatten().collect();
        groups.sort_by(|a, b| {
            b.last_received
                .cmp(&a.last_received)
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        });
        Ok(groups
            .into_iter()
            .filter_map(|group| group_row(group, filter))
            .skip(offset as usize)
            .take(limit as usize)
            .collect())
    }

    #[cfg(test)]
    pub async fn group(&self, fingerprint: &str) -> Result<Option<Group>> {
        Ok(Group::get(self.backend(), &self.cipher, fingerprint).await?.0)
    }
}
