//! What the private log viewer reads: the reports that hold an attachment, taken from each partition's
//! `queue/attached` list (about `max_attachments` entries, so a scope never walks the whole report index), and the
//! attachment's size and bytes. Only visible reports count: a hidden one keeps its attachment for the download route
//! but stays out of the viewer, its search and its comparison.

use anyhow::Result;

use super::index::{self, ATTACHED, QUEUE};
use super::model::Report;
use super::read::AdminFilter;
use super::{ATTACHMENTS, Store, Stores};

/// Entries of `queue/attached` read at most; the list holds about `max_attachments` live entries plus the stale
/// ones the sweep has not removed yet.
const MAX_ATTACHED_KEYS: usize = 100_000;
/// Reports loaded per round while walking the list.
const LOAD_BATCH: usize = 64;

/// The admin list's filters plus the failure kind.
#[derive(Default)]
pub struct LogFilter {
    pub base: AdminFilter,
    pub kind: Option<String>,
}

impl LogFilter {
    fn keeps(&self, report: &Report) -> bool {
        let base = &self.base;
        base.fingerprint.as_ref().is_none_or(|fp| *fp == report.fingerprint)
            && base.day.as_ref().is_none_or(|day| *day == report.received_day)
            && base.product.is_none_or(|product| product.name() == report.product)
            && base
                .title
                .as_ref()
                .is_none_or(|title| report.title_id.as_ref() == Some(title))
            && self.kind.as_ref().is_none_or(|kind| *kind == report.failure_kind)
    }
}

pub struct LogRow {
    pub id: String,
    pub product: String,
    pub fingerprint: String,
    pub title_id: Option<String>,
    pub game_name: Option<String>,
    pub failure_kind: String,
    pub template_id: String,
    pub received_day: String,
    /// Filled by [`Stores::with_sizes`] for the rows a page shows.
    pub size: Option<u64>,
    blob: String,
}

impl LogRow {
    fn of(report: Report) -> Option<Self> {
        let blob = report.attachment?;
        Some(Self {
            id: report.id,
            product: report.product,
            fingerprint: report.fingerprint,
            title_id: report.title_id,
            game_name: report.game_name,
            failure_kind: report.failure_kind,
            template_id: report.template_id,
            received_day: report.received_day,
            size: None,
            blob,
        })
    }

    #[cfg(test)]
    pub fn for_test(id: &str, product: &str) -> Self {
        Self {
            id: id.to_owned(),
            product: product.to_owned(),
            fingerprint: "0".repeat(16),
            title_id: None,
            game_name: None,
            failure_kind: "crash".to_owned(),
            template_id: "t".to_owned(),
            received_day: "2026-10-28".to_owned(),
            size: None,
            blob: String::new(),
        }
    }
}

impl Store {
    /// `(seq, id)` of every attached report, newest first.
    async fn attached_members(&self) -> Result<Vec<(u64, String)>> {
        let prefix = QUEUE.members_prefix(ATTACHED)?;
        let mut members = Vec::new();
        let mut after: Option<String> = None;
        while members.len() < MAX_ATTACHED_KEYS {
            let (page, next) = self.page(&prefix, after.as_deref(), 1000).await?;
            members.extend(
                page.iter()
                    .filter_map(|member| index::parse_member(member, false))
                    .map(|(seq, id)| (seq, id.to_owned())),
            );
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        members.reverse();
        Ok(members)
    }

    /// The visible attached reports `filter` keeps, newest first; skips `skip` and returns at most `take`.
    async fn logs(&self, filter: &LogFilter, skip: usize, take: usize) -> Result<Vec<LogRow>> {
        let members = self.attached_members().await?;
        let mut out = Vec::new();
        let mut skipped = 0;
        for chunk in members.chunks(LOAD_BATCH) {
            let ids: Vec<&str> = chunk.iter().map(|(_, id)| id.as_str()).collect();
            for ((seq, _), found) in chunk.iter().zip(self.reports(&ids).await?) {
                let Some((report, _)) = found else { continue };
                if report.seq != *seq || report.hidden || !filter.keeps(&report) {
                    continue;
                }
                let Some(row) = LogRow::of(report) else { continue };
                if skipped < skip {
                    skipped += 1;
                    continue;
                }
                out.push(row);
                if out.len() >= take {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// The attachment's length from its manifest, without reading it.
    async fn log_size(&self, row: &LogRow) -> Result<Option<u64>> {
        let (manifest, _) = ATTACHMENTS.head(self.backend(), &self.cipher, &row.blob).await?;
        Ok(manifest.map(|manifest| manifest.total_len))
    }

    async fn log_bytes(&self, row: &LogRow) -> Result<Option<Vec<u8>>> {
        Ok(ATTACHMENTS.get(&self.backend, &self.cipher, &row.blob).await?.0)
    }

    /// A visible report's attachment; `None` for an unknown, hidden or unattached id alike.
    async fn log(&self, id: &str) -> Result<Option<(LogRow, Vec<u8>)>> {
        let Some(row) = self
            .report(id)
            .await?
            .map(|(report, _)| report)
            .filter(|report| !report.hidden)
            .and_then(LogRow::of)
        else {
            return Ok(None);
        };
        Ok(self.log_bytes(&row).await?.map(|bytes| (row, bytes)))
    }
}

impl Stores {
    /// The visible attached reports of both partitions (or the filtered one), newest received day first; within a
    /// day decanter's come first, as in the admin list.
    pub async fn logs(&self, filter: &LogFilter, skip: usize, take: usize) -> Result<Vec<LogRow>> {
        if let Some(product) = filter.base.product {
            return self.get(product).logs(filter, skip, take).await;
        }
        let wanted = skip.saturating_add(take);
        let (mut rows, psport) = tokio::try_join!(
            self.get(super::Product::Decanter).logs(filter, 0, wanted),
            self.get(super::Product::Psport).logs(filter, 0, wanted)
        )?;
        rows.extend(psport);
        rows.sort_by(|a, b| b.received_day.cmp(&a.received_day));
        Ok(rows.into_iter().skip(skip).take(take).collect())
    }

    /// Fills each row's `size`, reading only the manifests.
    pub async fn with_sizes(&self, rows: &mut [LogRow]) -> Result<()> {
        for row in rows {
            if let Some(store) = self.of_id(&row.id) {
                row.size = store.log_size(row).await?;
            }
        }
        Ok(())
    }

    pub async fn log(&self, id: &str) -> Result<Option<(LogRow, Vec<u8>)>> {
        match self.of_id(id) {
            Some(store) => store.log(id).await,
            None => Ok(None),
        }
    }
}
