//! The storage invariants the HTTP tests cannot see: counters under racing writers on both local engines, eviction
//! without scans, orphan cleanup, failed inserts, and bulk actions spanning several transactions.

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use s3db::{Backend, Cas, DeleteOutcome, ListPage, LocalEngine};

use super::index::{self, Entry, PENDING, QUEUE};
use super::model::Report;
use super::{ATTACHMENTS, CLEAN_LOGS, Change, Insert, NewReport, Product, Store, test_store};
use crate::secrets::{random_hex, sha256};

type Test = Result<(), Box<dyn Error>>;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-store-{}", random_hex()?));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_report(fingerprint: &str, title: &str, day: &str) -> Result<NewReport, Box<dyn Error>> {
    Ok(NewReport {
        id: Product::Psport.new_id()?,
        delete_hash: sha256(b"code"),
        fingerprint: fingerprint.to_owned(),
        product: "psport",
        title_id: Some(title.to_owned()),
        game_name: Some(format!("Game {title}")),
        failure_kind: "error".to_owned(),
        template_id: "t".to_owned(),
        message: "t".to_owned(),
        created_day: day.to_owned(),
        received_day: day.to_owned(),
        report_json: "{}".to_owned(),
    })
}

async fn insert(store: &Store, report: &NewReport, max: u32) -> Result<(), Box<dyn Error>> {
    match store.insert(report, max).await? {
        Insert::Stored { .. } => Ok(()),
        Insert::Full => Err("store full".into()),
    }
}

#[allow(deprecated)]
const ENGINES: [LocalEngine; 2] = [LocalEngine::Redb, LocalEngine::Files];

/// The files engine has no native transaction, so it runs s3db's lock protocol: the same path as S3.
#[tokio::test]
async fn racing_inserts_into_one_group_keep_every_counter_exact() -> Test {
    for engine in ENGINES {
        let scratch = Scratch::new()?;
        let store = test_store(&scratch.0, engine);
        let fp = "0123456789abcdef";
        let mut reports = Vec::with_capacity(24);
        for n in 0..24 {
            reports.push(new_report(fp, &format!("CUSA{:05}", n % 3), "2026-10-27")?);
        }
        let tasks: Vec<_> = reports
            .into_iter()
            .map(|report| {
                let store = store.clone();
                tokio::spawn(async move {
                    match store.insert(&report, 1000).await {
                        Ok(Insert::Stored { .. }) => Ok(report.id),
                        Ok(Insert::Full) => Err("full".to_owned()),
                        Err(error) => Err(format!("{error:#}")),
                    }
                })
            })
            .collect();
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await??);
        }
        let resolving: Vec<_> = ids
            .chunks(4)
            .map(|chunk| {
                let store = store.clone();
                let chunk = chunk[..1].to_vec();
                tokio::spawn(async move { store.change(&chunk, &Change::Resolve("2026-10-28".to_owned())).await })
            })
            .collect();
        for task in resolving {
            task.await??;
        }
        let group = store.group(fp).await?.ok_or("group")?;
        assert_eq!((group.total, group.visible, group.resolved), (24, 24, 6), "{engine:?}");
        assert_eq!(group.titles.values().map(|title| title.count).sum::<u32>(), 24);
        assert_eq!(group.titles.values().map(|title| title.resolved).sum::<u32>(), 6);
        assert_eq!(store.count().await?, 24, "{engine:?}");
        let mut listed: Vec<String> = store
            .group_members(fp, 100, 0)
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();
        listed.sort();
        ids.sort();
        assert_eq!(listed, ids, "{engine:?}");
    }
    Ok(())
}

/// Counts what reaches the bucket, so a test can tell an index walk from a scan of every report.
#[derive(Debug)]
struct Counting {
    inner: Arc<dyn Backend>,
    gets: AtomicUsize,
    lists: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Backend for Counting {
    async fn get(&self, key: &str) -> anyhow::Result<(Vec<u8>, Option<String>)> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        self.inner.get(key).await
    }

    async fn put_cas(&self, key: &str, body: Vec<u8>, expected_etag: Option<&str>) -> anyhow::Result<Cas> {
        self.inner.put_cas(key, body, expected_etag).await
    }

    async fn delete(&self, key: &str) -> anyhow::Result<DeleteOutcome> {
        self.inner.delete(key).await
    }

    async fn delete_many(&self, keys: &[String]) -> anyhow::Result<DeleteOutcome> {
        self.inner.delete_many(keys).await
    }

    async fn list_prefix(&self, prefix: &str) -> anyhow::Result<Vec<(String, u64)>> {
        self.record(prefix);
        self.inner.list_prefix(prefix).await
    }

    async fn list_page(&self, prefix: &str, after: Option<&str>, limit: usize) -> anyhow::Result<ListPage> {
        self.record(prefix);
        self.inner.list_page(prefix, after, limit).await
    }

    async fn get_range(&self, key: &str, offset: u64, len: u64) -> anyhow::Result<Vec<u8>> {
        self.inner.get_range(key, offset, len).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_atomic(&self) -> Option<&dyn s3db::AtomicBackend> {
        self.inner.as_atomic()
    }
}

impl Counting {
    fn record(&self, prefix: &str) {
        self.lists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(prefix.to_owned());
    }
}

#[tokio::test]
async fn eviction_takes_the_oldest_unresolved_from_its_queue_without_scanning_reports() -> Test {
    let scratch = Scratch::new()?;
    let base = test_store(&scratch.0, LocalEngine::Redb);
    let counting = Arc::new(Counting {
        inner: Arc::clone(&base.backend),
        gets: AtomicUsize::new(0),
        lists: Mutex::new(Vec::new()),
    });
    let store = Store::with_cipher(Arc::clone(&counting) as Arc<dyn Backend>, Arc::clone(&base.cipher));
    let mut first = Vec::new();
    for n in 0..40 {
        let report = new_report(&format!("{n:016x}"), "CUSA00001", "2026-10-27")?;
        insert(&store, &report, 40).await?;
        first.push(report.id);
    }
    store
        .change(&first[..1], &Change::Resolve("2026-10-28".to_owned()))
        .await?;
    counting.gets.store(0, Ordering::Relaxed);
    counting.lists.lock().unwrap_or_else(PoisonError::into_inner).clear();

    insert(&store, &new_report("ffffffffffffffff", "CUSA00001", "2026-10-28")?, 40).await?;

    assert!(
        store.find_public(&first[0]).await?.is_some(),
        "resolved reports are never evicted"
    );
    assert!(
        store.find_public(&first[1]).await?.is_none(),
        "the oldest unresolved one goes"
    );
    assert!(store.find_public(&first[2]).await?.is_some());
    assert_eq!(store.count().await?, 40);
    let lists = counting.lists.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(
        lists
            .iter()
            .all(|prefix| !prefix.starts_with("reports/") && !prefix.starts_with("idx/all")),
        "{lists:?}"
    );
    assert!(
        counting.gets.load(Ordering::Relaxed) < 40,
        "one read per report is a scan"
    );
    Ok(())
}

#[tokio::test]
async fn sweep_removes_orphan_blobs_and_failed_inserts_but_keeps_live_ones() -> Test {
    let scratch = Scratch::new()?;
    let store = test_store(&scratch.0, LocalEngine::Redb);
    let report = new_report("0123456789abcdef", "CUSA00001", "2026-10-27")?;
    insert(&store, &report, 10).await?;
    assert!(matches!(
        store.attach(&report.id, b"log", b"{}", 10).await?,
        super::Attach::Stored { .. }
    ));
    let orphan = format!("{}-{}", random_hex()?, random_hex()?);
    ATTACHMENTS
        .put(store.backend(), &store.cipher, &orphan, b"lost", None)
        .await?;
    assert_eq!(store.blob_count().await?, 2);
    assert_eq!(
        store.remove_orphan_blobs(Duration::from_hours(1)).await?,
        0,
        "young blobs may be in flight"
    );
    let orphan_clean = format!("{}-{}", random_hex()?, random_hex()?);
    CLEAN_LOGS
        .put(store.backend(), &store.cipher, &orphan_clean, b"{}", None)
        .await?;
    assert_eq!(store.remove_orphan_blobs(Duration::ZERO).await?, 2);
    assert_eq!(store.blob_count().await?, 1);
    assert_eq!(store.clean_blob_count().await?, 1, "the live report's clean log stays");
    assert_eq!(store.attachment(&report.id).await?.as_deref(), Some(&b"log"[..]));

    let lost = Report {
        id: random_hex()?,
        seq: 0,
        fingerprint: "fedcba9876543210".to_owned(),
        product: "psport".to_owned(),
        received_day: "2026-10-27".to_owned(),
        ..Report::default()
    };
    let pending = Entry {
        index: &QUEUE,
        value: PENDING.to_owned(),
        member: index::ordered_member(lost.seq, &lost.id),
    };
    store.add_entry_with(&pending, &lost).await?;
    for entry in index::entries_of(&lost) {
        store.add_entry(&entry).await?;
    }
    store.sweep("2026-10-28", "0000-01-01", None).await?;
    let still_there = store.members(&QUEUE.members_prefix(PENDING)?).await?;
    assert_eq!(
        still_there.len(),
        1,
        "an insert newer than the previous sweep may still be running"
    );
    let swept = store.sweep("2026-10-29", "0000-01-01", None).await?;
    assert!(swept.repaired >= 1);
    let fp_entries = store
        .members(&index::BY_FINGERPRINT.members_prefix("fedcba9876543210")?)
        .await?;
    assert!(fp_entries.is_empty(), "{fp_entries:?}");
    assert!(store.members(&QUEUE.members_prefix(PENDING)?).await?.is_empty());
    assert!(store.find_public(&report.id).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn deleting_a_report_with_an_attachment_frees_its_attachment_count() -> Test {
    let scratch = Scratch::new()?;
    let store = test_store(&scratch.0, LocalEngine::Redb);
    let report = new_report("0123456789abcdef", "CUSA00001", "2026-10-27")?;
    insert(&store, &report, 1000).await?;
    store.attach(&report.id, b"log", b"{}", 10).await?;
    assert_eq!(store.totals().await?.0.attachments, 1);
    store.change(std::slice::from_ref(&report.id), &Change::Delete).await?;
    let totals = store.totals().await?.0;
    assert_eq!((totals.reports, totals.attachments), (0, 0));
    assert_eq!(store.blob_count().await?, 0);
    assert_eq!(store.clean_blob_count().await?, 0);
    Ok(())
}

#[tokio::test]
async fn clean_log_outlives_its_attachment_and_a_new_one_replaces_it() -> Test {
    let scratch = Scratch::new()?;
    let store = test_store(&scratch.0, LocalEngine::Redb);
    let report = new_report("0123456789abcdef", "CUSA00001", "2026-10-27")?;
    insert(&store, &report, 1000).await?;
    store.attach(&report.id, b"log", b"first", 10).await?;
    let id = std::slice::from_ref(&report.id);
    store.change(id, &Change::Detach).await?;
    assert_eq!((store.blob_count().await?, store.clean_blob_count().await?), (0, 1));
    store.attach(&report.id, b"log", b"second", 10).await?;
    assert_eq!(
        store.clean_blob_count().await?,
        1,
        "the evicted attachment's clean log is replaced"
    );
    let public = store.find_public(&report.id).await?.ok_or("report")?;
    assert_eq!(store.clean_log(&public).await?.as_deref(), Some(&b"second"[..]));
    store.change(id, &Change::Resolve("2026-10-28".to_owned())).await?;
    assert_eq!((store.blob_count().await?, store.clean_blob_count().await?), (0, 1));
    Ok(())
}

#[tokio::test]
async fn bulk_actions_span_several_transactions_and_keep_the_counters() -> Test {
    let scratch = Scratch::new()?;
    let store = test_store(&scratch.0, LocalEngine::Redb);
    let fp = "0123456789abcdef";
    let mut ids = Vec::new();
    for n in 0..(super::BATCH + 6) {
        let report = new_report(fp, &format!("CUSA{:05}", n % 2), "2026-10-27")?;
        insert(&store, &report, 1000).await?;
        ids.push(report.id);
    }
    insert(
        &store,
        &new_report("fedcba9876543210", "CUSA00009", "2026-10-27")?,
        1000,
    )
    .await?;
    store.attach(&ids[0], b"log", b"{}", 10).await?;
    let all = store.group_ids(fp).await?;
    assert_eq!(all.len(), ids.len());

    store.change(&all, &Change::Hide).await?;
    let group = store.group(fp).await?.ok_or("group")?;
    assert_eq!((group.visible, group.hidden), (0, 70));
    assert!(store.group_members(fp, 10, 0).await?.is_empty());
    store.change(&all, &Change::Restore).await?;
    store.change(&all, &Change::Resolve("2026-10-28".to_owned())).await?;
    let group = store.group(fp).await?.ok_or("group")?;
    assert_eq!((group.visible, group.resolved, group.hidden), (70, 70, 0));
    assert_eq!(store.blob_count().await?, 0);
    assert_eq!(store.totals().await?.0.attachments, 0);

    let removed = store.change(&all, &Change::Delete).await?;
    assert_eq!(removed.removed, 70);
    assert_eq!(store.count().await?, 1);
    assert_eq!(store.group(fp).await?.map(|group| group.total), Some(0));
    assert!(store.group_ids(fp).await?.is_empty());
    assert!(store.members(&QUEUE.members_prefix(index::GONE)?).await?.is_empty());
    let rows = store.groups(&super::Filter::default(), 10, 0).await?;
    assert_eq!(
        rows.iter().map(|row| row.fingerprint.as_str()).collect::<Vec<_>>(),
        ["fedcba9876543210"]
    );
    Ok(())
}
