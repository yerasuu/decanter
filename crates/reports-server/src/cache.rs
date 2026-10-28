//! A short-lived cache for the rendered index, the one public page that scans the whole table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Keys come from validated filter values only, but a title id alone has billions of forms; the cap keeps the map
/// small and a full map simply stops caching new keys until some expire.
const MAX_KEYS: usize = 128;

pub struct PageCache {
    ttl: Duration,
    entries: Mutex<HashMap<String, (Instant, Arc<str>)>>,
}

impl PageCache {
    #[must_use]
    pub fn new(ttl_secs: u32) -> Self {
        Self {
            ttl: Duration::from_secs(u64::from(ttl_secs)),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub const fn enabled(&self) -> bool {
        !self.ttl.is_zero()
    }

    pub fn get(&self, key: &str, now: Instant) -> Option<Arc<str>> {
        let found = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map(|(stored_at, page)| (*stored_at, Arc::clone(page)));
        let (stored_at, page) = found?;
        (now.duration_since(stored_at) < self.ttl).then_some(page)
    }

    pub fn put(&self, key: String, page: Arc<str>, now: Instant) {
        if !self.enabled() {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= MAX_KEYS && !entries.contains_key(&key) {
            entries.retain(|_, (stored_at, _)| now.duration_since(*stored_at) < self.ttl);
            if entries.len() >= MAX_KEYS {
                return;
            }
        }
        entries.insert(key, (now, page));
        drop(entries);
    }

    /// Called when a report leaves the public view, so a hidden or deleted report does not linger in the index.
    pub fn clear(&self) {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_serves_until_the_ttl_then_misses() {
        let cache = PageCache::new(10);
        let t0 = Instant::now();
        cache.put("a".to_owned(), Arc::from("page"), t0);
        assert_eq!(cache.get("a", t0 + Duration::from_secs(9)).as_deref(), Some("page"));
        assert!(cache.get("a", t0 + Duration::from_secs(10)).is_none());
        assert!(cache.get("b", t0).is_none());
    }

    #[test]
    fn key_set_is_bounded_and_expired_keys_make_room() {
        let cache = PageCache::new(10);
        let t0 = Instant::now();
        for i in 0..MAX_KEYS * 3 {
            cache.put(format!("k{i}"), Arc::from("p"), t0);
        }
        assert_eq!(cache.len(), MAX_KEYS);
        assert!(cache.get("k0", t0).is_some());
        assert!(cache.get(&format!("k{MAX_KEYS}"), t0).is_none());
        cache.put("late".to_owned(), Arc::from("p"), t0 + Duration::from_secs(11));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn zero_ttl_disables_it() {
        let cache = PageCache::new(0);
        cache.put("a".to_owned(), Arc::from("p"), Instant::now());
        assert_eq!(cache.len(), 0);
        assert!(!cache.enabled());
    }
}
