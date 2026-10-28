//! The audit trail (#1154): one record per change a maintainer makes, in the accounts store under
//! `audit/<inverted minute>-<random>`, so a plain key listing reads newest first and the sweep finds old records
//! without decrypting them. A record holds who, with which role, what, on which report, group or account, how many
//! reports, and the minute in UTC: nothing finer, no address, no key, no hash, no attachment content. Records of one
//! minute list in random order.

use anyhow::Result;
use chrono::{DateTime, Utc};
use s3db::Cas;
use serde::{Deserialize, Serialize};

use super::AccountStore;
use crate::secrets::hex;

const PREFIX: &str = "audit/";
/// Minutes are written as `MINUTE_CEILING - minute`, ten digits wide, so a later minute sorts first.
const MINUTE_CEILING: u64 = 9_999_999_999;
const MINUTE_DIGITS: usize = 10;
const RANDOM_HEX: usize = 8;
/// Create-only writes that may collide on the random part within one minute before giving up.
const ATTEMPTS: u32 = 4;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AuditRecord {
    /// The account's name, or `cli` for the `user` subcommand.
    pub user: String,
    /// `admin`, `viewer` or `cli`.
    pub role: String,
    pub action: String,
    /// A report id, a group's fingerprint or an account name; empty for a login or logout.
    pub target: String,
    pub count: u32,
    /// `YYYY-MM-DDTHH:MMZ`.
    pub at: String,
}

/// What `--check-config` says about the trail: how many records and the day of the oldest.
pub struct AuditSummary {
    pub records: usize,
    pub oldest_day: Option<String>,
}

#[must_use]
pub fn current_minute() -> u64 {
    u64::try_from(Utc::now().timestamp()).unwrap_or_default() / 60
}

fn time_of(minute: u64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(i64::try_from(minute.checked_mul(60)?).ok()?, 0)
}

fn minute_text(minute: u64) -> String {
    time_of(minute).map_or_else(String::new, |time| time.format("%Y-%m-%dT%H:%MZ").to_string())
}

fn day_of(minute: u64) -> Option<String> {
    time_of(minute).map(|time| time.format("%Y-%m-%d").to_string())
}

/// Whether `text` is a record id as the listing hands it out, which is the only cursor the audit page accepts.
#[must_use]
pub fn is_audit_id(text: &str) -> bool {
    text.split_once('-').is_some_and(|(minute, random)| {
        minute.len() == MINUTE_DIGITS
            && minute.bytes().all(|b| b.is_ascii_digit())
            && crate::secrets::is_hex_token(random, RANDOM_HEX)
    })
}

fn minute_of(id: &str) -> Option<u64> {
    let inverted = id.split_once('-')?.0.parse::<u64>().ok()?;
    MINUTE_CEILING.checked_sub(inverted)
}

fn new_id(minute: u64) -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; RANDOM_HEX / 2];
    getrandom::fill(&mut bytes)?;
    Ok(format!(
        "{:0MINUTE_DIGITS$}-{}",
        MINUTE_CEILING.saturating_sub(minute),
        hex(&bytes)
    ))
}

fn key_of(id: &str) -> String {
    format!("{PREFIX}{id}.json")
}

impl AccountStore {
    /// Writes `record` as made at `minute`, which also sets its `at`.
    ///
    /// # Errors
    ///
    /// The bucket fails, the random source is unavailable, or every attempt met an existing record.
    pub async fn append_audit(&self, record: &AuditRecord, minute: u64) -> Result<()> {
        let record = AuditRecord {
            at: minute_text(minute),
            ..record.clone()
        };
        for _ in 0..ATTEMPTS {
            let key = key_of(&new_id(minute)?);
            if s3db::put_json(self.store.backend(), &key, &record, None, &self.store.cipher).await? == Cas::Ok {
                return Ok(());
            }
        }
        anyhow::bail!("every audit record id tried was taken")
    }

    /// Up to `limit` records, newest first, after the record id `after`, and the id to continue after when more
    /// remain. A record the sweep deletes between the listing and its read is skipped.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed or a record cannot be read.
    pub async fn audit_page(&self, after: Option<&str>, limit: usize) -> Result<(Vec<AuditRecord>, Option<String>)> {
        let after = after.map(key_of);
        let (ids, more) = self.store.page(PREFIX, after.as_deref(), limit).await?;
        let next = more.and_then(|_| ids.last().cloned());
        let mut records = Vec::with_capacity(ids.len());
        for id in &ids {
            let (record, etag) =
                s3db::get_json::<AuditRecord>(self.store.backend(), &key_of(id), &self.store.cipher).await?;
            if etag.is_some() {
                records.push(record);
            }
        }
        Ok((records, next))
    }

    async fn audit_ids(&self) -> Result<Vec<String>> {
        self.store.members(PREFIX).await
    }

    /// Deletes the records made before `cutoff` (a minute), read from their keys alone, and answers how many.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed or the deletion fails.
    pub async fn sweep_audit(&self, cutoff: u64) -> Result<usize> {
        let old: Vec<String> = self
            .audit_ids()
            .await?
            .iter()
            .filter(|id| minute_of(id).is_some_and(|minute| minute < cutoff))
            .map(|id| key_of(id))
            .collect();
        if !old.is_empty() {
            self.store.backend().delete_many(&old).await?;
        }
        Ok(old.len())
    }

    /// A key listing only: the records are not decrypted.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed.
    pub async fn audit_summary(&self) -> Result<AuditSummary> {
        let ids = self.audit_ids().await?;
        Ok(AuditSummary {
            records: ids.len(),
            oldest_day: ids.last().and_then(|id| minute_of(id)).and_then(day_of),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_newest_first_and_carry_only_the_minute() -> Result<(), getrandom::Error> {
        let (earlier, later) = (new_id(29_000_000)?, new_id(29_000_001)?);
        assert!(later < earlier);
        assert!(is_audit_id(&earlier) && is_audit_id(&later));
        assert_eq!(minute_of(&earlier), Some(29_000_000));
        for bad in [
            "",
            "9970999999-abcd",
            "997099999-abcd0123",
            "9970999999-ABCD0123",
            "../x-abcd0123",
        ] {
            assert!(!is_audit_id(bad), "{bad}");
        }
        assert_eq!(minute_text(29_000_000), "2025-02-19T21:20Z");
        assert_eq!(day_of(29_000_000).as_deref(), Some("2025-02-19"));
        Ok(())
    }
}
