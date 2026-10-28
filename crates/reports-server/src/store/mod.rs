//! All state lives in an object bucket through s3db: encrypted at rest, compare-and-swap on every write, and one
//! `run_update` transaction for each change to reports together with their group and the totals, so the counters
//! never drift from the reports they count, even when requests race.

mod index;
mod logs;
mod maintain;
mod model;
mod partition;
mod prefixed;
mod read;
mod write;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use s3db::txn_update::{UpdateOp, run_update};
use s3db::{Backend, Blob, Cipher, Singleton, bootstrap};
use serde::Serialize;

pub use logs::{LogFilter, LogRow};
pub use model::Report;
#[cfg(test)]
pub use partition::test_stores;
pub use partition::{Product, Stores};
pub use prefixed::Prefixed;
pub use read::{AdminFilter, AdminRow, Filter, GroupRow};
pub use write::{Attach, Change, Insert, NewReport};

use crate::config::{Driver, Storage, StorageCredentials};
use crate::ingest::MAX_ATTACHMENT_BYTES;
use index::Entry;
use model::Totals;

const TOTALS: Singleton = Singleton::new("meta/totals.json");
const SEQUENCE: Singleton = Singleton::new("meta/sequence.json");
const LAST_SWEEP: Singleton = Singleton::new("meta/sweep.json");
const ATTACHMENTS: Blob = Blob::new("attachments").with_max_total_bytes(MAX_ATTACHMENT_BYTES);
/// The public clean logs, apart from the attachments so the sweep, the caps and the admin download of the private
/// ones never mix the two.
const CLEAN_LOGS: Blob = Blob::new("cleanlogs").with_max_total_bytes(decanter_report::MAX_CLEAN_BYTES);

/// Attempts of one transaction before the request fails. A lost attempt means another writer's transaction landed
/// on the same group or the totals, so the next read sees it; with jitter this outlasts any realistic burst.
const MAX_ATTEMPTS: u32 = 64;
/// Reports one transaction changes at most: a bulk action over a large group runs as several transactions.
const BATCH: usize = 64;
/// How long a transaction may hold its locks before recovery decides its fate (S3 drivers only).
pub const STALE_TRANSACTION: Duration = Duration::from_secs(60);

/// Transport retries for the S3 drivers; a lost compare-and-swap is not one of them (that is the transactions' job).
const BUCKET_RETRIES: u32 = 3;
const BUCKET_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// The `local` driver's directory of one partition.
pub fn partition_dir(storage: &Storage, product: Product) -> std::path::PathBuf {
    storage.dir.join(product.name())
}

/// Whether the `local` driver has created this partition's database. Opening a missing one creates it, so the
/// read-only tools look first. The file name is s3db's (private there, `s3db.redb` at the pinned tag).
pub fn local_partition_exists(storage: &Storage, product: Product) -> bool {
    partition_dir(storage, product).join("s3db.redb").is_file()
}

/// The key prefix of one partition in an S3 bucket: the optional common root, then the product.
pub fn partition_prefix(storage: &Storage, product: Product) -> String {
    if storage.prefix.is_empty() {
        product.name().to_owned()
    } else {
        format!("{}/{}", storage.prefix, product.name())
    }
}

/// The backend of `product`'s partition, built from `credentials`. The S3 drivers get the key prefix and transport
/// retries only: s3db's full resilience stack journals writes on local disk, and the server keeps no local state
/// with them.
///
/// # Errors
///
/// The driver refuses its parameters (an `http://` endpoint without `allow_insecure_http`, say).
pub fn connect(storage: &Storage, credentials: &StorageCredentials, product: Product) -> Result<Arc<dyn Backend>> {
    let (access_key_id, secret_access_key) = credentials.bucket_keys.clone().unwrap_or_default();
    let config = match storage.driver {
        Driver::Local => {
            return s3db::build_backend(&s3db::BackendConfig::Local(s3db::LocalParams {
                bucket: partition_dir(storage, product).to_string_lossy().into_owned(),
                engine: s3db::LocalEngine::Redb,
            }));
        }
        Driver::S3 => s3db::BackendConfig::S3(s3db::S3Params {
            bucket: storage.bucket.clone(),
            region: Some(storage.region.clone()).filter(|region| !region.is_empty()),
            endpoint_url: Some(storage.endpoint.clone()).filter(|endpoint| !endpoint.is_empty()),
            access_key_id,
            secret_access_key: s3db::config::Secret::new(secret_access_key),
            path_style: storage.path_style,
            allow_insecure_http: storage.allow_insecure_http,
        }),
        Driver::R2 => s3db::BackendConfig::R2(s3db::R2Params {
            bucket: storage.bucket.clone(),
            account_id: storage.account_id.clone(),
            access_key_id,
            secret_access_key: s3db::config::Secret::new(secret_access_key),
        }),
    };
    let backend = Arc::new(Prefixed::new(
        s3db::build_backend(&config)?,
        &partition_prefix(storage, product),
    ));
    Ok(Arc::new(s3db::RetryBackend::new(
        backend,
        BUCKET_RETRIES,
        BUCKET_RETRY_BACKOFF,
    )))
}

/// A harmless round trip for `--check-config`: reads the bucket's encryption config and, when the bucket has one,
/// checks the passphrase opens it. Writes nothing.
///
/// # Errors
///
/// The bucket cannot be reached, or the passphrase does not open it.
pub async fn probe(backend: &dyn Backend, passphrase: &str) -> Result<bool> {
    let (_, etag) = backend.get(bootstrap::ENCRYPTION_META_KEY).await?;
    if etag.is_none() {
        // A missing key and a missing bucket both read as "no object": a listing tells them apart, as the first
        // start does before it creates the encryption.
        backend
            .list_page("", None, 1)
            .await
            .context("the bucket cannot be listed (does it exist, and may this key use it?)")?;
        return Ok(false);
    }
    bootstrap::open_cipher(backend, passphrase)
        .await
        .context("the passphrase does not open this bucket")?;
    Ok(true)
}

#[derive(Clone)]
pub struct Store {
    backend: Arc<dyn Backend>,
    cipher: Arc<Cipher>,
}

impl Store {
    /// Derives the bucket's key from `passphrase` (creating the bucket's encryption config on first use) and
    /// finishes any transaction a previous run left half done.
    ///
    /// # Errors
    ///
    /// The bucket cannot be reached, or `passphrase` does not open it.
    pub async fn open(backend: Arc<dyn Backend>, passphrase: &str) -> Result<Self> {
        let cipher = bootstrap::get_or_create_cipher(backend.as_ref(), passphrase)
            .await
            .context("cannot open the bucket's encryption (wrong passphrase or unreachable bucket)")?;
        let store = Self::with_cipher(backend, Arc::new(cipher));
        store.recover().await?;
        Ok(store)
    }

    pub const fn with_cipher(backend: Arc<dyn Backend>, cipher: Arc<Cipher>) -> Self {
        Self { backend, cipher }
    }

    /// Rolls back or forward the transactions whose locks are older than `STALE_TRANSACTION`; without this a crash
    /// mid-transaction would leave the totals locked and every upload failing.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed or a record cannot be read.
    pub async fn recover(&self) -> Result<usize> {
        s3db::recover_stale_updates(self.backend.as_ref(), &self.cipher, STALE_TRANSACTION).await
    }

    fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    async fn report(&self, id: &str) -> Result<Option<(Report, String)>> {
        let (report, etag) = Report::get(self.backend(), &self.cipher, id).await?;
        Ok(report.zip(etag).filter(|(report, _)| !report.deleted))
    }

    async fn totals(&self) -> Result<(Totals, Option<String>)> {
        TOTALS.get(self.backend(), &self.cipher).await
    }

    /// Reports in the order of `ids`, `None` where one is missing or deleted.
    async fn reports(&self, ids: &[&str]) -> Result<Vec<Option<(Report, String)>>> {
        let mut set = tokio::task::JoinSet::new();
        for (position, id) in ids.iter().enumerate() {
            let store = self.clone();
            let id = (*id).to_owned();
            set.spawn(async move { (position, store.report(&id).await) });
        }
        let mut found = vec![None; ids.len()];
        while let Some(joined) = set.join_next().await {
            let (position, report) = joined.context("report fetch task failed")?;
            if let Some(slot) = found.get_mut(position) {
                *slot = report?;
            }
        }
        Ok(found)
    }

    /// `true` when every op landed; `false` when another transaction got there first and nothing was written.
    async fn commit(&self, ops: Vec<UpdateOp>) -> Result<bool> {
        Ok(run_update(self.backend(), &self.cipher, ops).await?.applied)
    }

    async fn add_entry(&self, entry: &Entry) -> Result<()> {
        self.add_entry_with(entry, &()).await
    }

    /// Create-only: an entry that already exists says the same thing (its name is its meaning), so a lost race is
    /// success. `MultiIndex::add` overwrites instead, which fails under many writers adding one group's entry.
    async fn add_entry_with<T: Serialize + Sync>(&self, entry: &Entry, payload: &T) -> Result<()> {
        let key = entry.index.entry_key(&entry.value, &entry.member)?;
        s3db::put_json(self.backend(), &key, payload, None, &self.cipher).await?;
        Ok(())
    }

    async fn remove_entry(&self, entry: &Entry) {
        entry.index.remove(self.backend(), &entry.value, &entry.member).await;
    }

    /// One page of member names under `prefix` (an index value's members, or a whole index), in key order, and the
    /// key to continue after.
    async fn page(&self, prefix: &str, after: Option<&str>, limit: usize) -> Result<(Vec<String>, Option<String>)> {
        let page = self.backend.list_page(prefix, after, limit).await?;
        let members = page
            .entries
            .into_iter()
            .filter_map(|(key, _)| {
                key.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(".json"))
                    .map(str::to_owned)
            })
            .collect();
        Ok((members, page.next_after))
    }

    /// Every member name under `prefix`, for the sweep's passes over whole indexes.
    async fn members(&self, prefix: &str) -> Result<Vec<String>> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let (members, next) = self.page(prefix, after.as_deref(), 1000).await?;
            all.extend(members);
            match next {
                Some(next) => after = Some(next),
                None => return Ok(all),
            }
        }
    }

    async fn remove_blob(&self, name: &str) {
        if let Err(error) = ATTACHMENTS.delete(self.backend(), name).await {
            tracing::warn!(target: "decanter_reports::store", %error, "attachment blob not removed");
        }
    }

    async fn remove_clean_blob(&self, name: &str) {
        if let Err(error) = CLEAN_LOGS.delete(self.backend(), name).await {
            tracing::warn!(target: "decanter_reports::store", %error, "clean log blob not removed");
        }
    }

    #[cfg(test)]
    pub async fn count(&self) -> Result<u32> {
        Ok(self.totals().await?.0.reports)
    }

    #[cfg(test)]
    pub async fn blob_count(&self) -> Result<usize> {
        Ok(ATTACHMENTS.list_names(self.backend()).await?.len())
    }

    #[cfg(test)]
    pub async fn clean_blob_count(&self) -> Result<usize> {
        Ok(CLEAN_LOGS.list_names(self.backend()).await?.len())
    }
}

/// A store over a fresh local directory with a fixed key, so tests skip the passphrase's key derivation.
#[cfg(test)]
pub fn test_store(dir: &std::path::Path, engine: s3db::LocalEngine) -> Store {
    static CIPHER: std::sync::OnceLock<Option<Arc<Cipher>>> = std::sync::OnceLock::new();
    let backend: Arc<dyn Backend> = Arc::new(s3db::LocalBackend::new(&s3db::LocalParams {
        bucket: dir.to_string_lossy().into_owned(),
        engine,
    }));
    let cipher = CIPHER.get_or_init(|| {
        Cipher::derive("test passphrase", &[3; s3db::cipher::SALT_LEN])
            .ok()
            .map(Arc::new)
    });
    let cipher = cipher.clone().unwrap_or_else(|| Arc::new(Cipher::disabled()));
    Store::with_cipher(backend, cipher)
}

/// Exponential backoff with full jitter, so writers that lost the same race do not collide again in lockstep.
async fn backoff(attempt: u32) {
    let ceiling = 10_u64.saturating_mul(1 << attempt.min(6));
    let mut random = [0_u8; 8];
    let jitter = match getrandom::fill(&mut random) {
        Ok(()) => u64::from_le_bytes(random) % ceiling.max(1),
        Err(_) => ceiling / 2,
    };
    tokio::time::sleep(Duration::from_millis(jitter + 1)).await;
}

/// Rows freed at once when a cap is hit, so the next uploads do not each pay for an eviction.
const fn eviction_batch(max: u32) -> u32 {
    let batch = max / 100;
    if batch == 0 { 1 } else { batch }
}

#[cfg(test)]
mod tests;
