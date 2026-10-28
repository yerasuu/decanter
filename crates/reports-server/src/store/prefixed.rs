//! Keeps every object of the server under one key prefix of a shared bucket, so the bucket's user can be restricted
//! to that prefix. s3db has no prefix of its own (its bookkeeping lives at fixed keys such as `.conf/`), hence this
//! layer. Only for the S3 drivers: they have no native transaction or range capability to forward, and a local
//! directory needs no prefix.

use std::sync::Arc;

use anyhow::Result;
use s3db::{Backend, Cas, DeleteOutcome, ListPage};

#[derive(Debug)]
pub struct Prefixed {
    inner: Arc<dyn Backend>,
    /// Ends with `/`.
    prefix: String,
}

impl Prefixed {
    /// `prefix` must already be validated: non-empty segments without `.` or `..`, no leading or trailing slash.
    pub fn new(inner: Arc<dyn Backend>, prefix: &str) -> Self {
        Self {
            inner,
            prefix: format!("{prefix}/"),
        }
    }

    fn full(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    fn strip(&self, key: &str) -> Option<String> {
        key.strip_prefix(&self.prefix).map(str::to_owned)
    }
}

#[async_trait::async_trait]
impl Backend for Prefixed {
    async fn get(&self, key: &str) -> Result<(Vec<u8>, Option<String>)> {
        self.inner.get(&self.full(key)).await
    }

    async fn put_cas(&self, key: &str, body: Vec<u8>, expected_etag: Option<&str>) -> Result<Cas> {
        self.inner.put_cas(&self.full(key), body, expected_etag).await
    }

    async fn delete(&self, key: &str) -> Result<DeleteOutcome> {
        self.inner.delete(&self.full(key)).await
    }

    async fn delete_many(&self, keys: &[String]) -> Result<DeleteOutcome> {
        let keys: Vec<String> = keys.iter().map(|key| self.full(key)).collect();
        self.inner.delete_many(&keys).await
    }

    async fn list_prefix(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
        Ok(self
            .inner
            .list_prefix(&self.full(prefix))
            .await?
            .into_iter()
            .filter_map(|(key, size)| self.strip(&key).map(|key| (key, size)))
            .collect())
    }

    async fn list_page(&self, prefix: &str, after: Option<&str>, limit: usize) -> Result<ListPage> {
        let after = after.map(|key| self.full(key));
        let page = self
            .inner
            .list_page(&self.full(prefix), after.as_deref(), limit)
            .await?;
        Ok(ListPage {
            entries: page
                .entries
                .into_iter()
                .filter_map(|(key, size)| self.strip(&key).map(|key| (key, size)))
                .collect(),
            next_after: page.next_after.and_then(|key| self.strip(&key)),
        })
    }

    async fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.inner.get_range(&self.full(key), offset, len).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keys_live_under_the_prefix_and_list_without_it() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-prefix-{}", crate::secrets::random_hex()?));
        let raw: Arc<dyn Backend> = Arc::new(s3db::LocalBackend::new(&s3db::LocalParams {
            bucket: dir.to_string_lossy().into_owned(),
            engine: s3db::LocalEngine::Redb,
        }));
        let prefixed = Prefixed::new(Arc::clone(&raw), "reports/prod");
        assert_eq!(prefixed.put_cas("a/b.json", b"x".to_vec(), None).await?, Cas::Ok);
        assert!(raw.get("reports/prod/a/b.json").await?.1.is_some());
        let page = prefixed.list_page("a/", None, 10).await?;
        assert_eq!(page.entries.first().map(|(key, _)| key.as_str()), Some("a/b.json"));
        drop((prefixed, raw));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
