// SPDX-License-Identifier: GPL-3.0-or-later
//! Short stand-ins for client, product and user ids in logs (plan, requirement 7).

use core::fmt;
use std::sync::LazyLock;

use sha2::{Digest as _, Sha256};

/// Drawn once per process. Client and product ids are public catalogue numbers, so an unkeyed
/// hash would be undone by hashing the catalogue; with the key, a stand-in only matches the same
/// id within one run's log.
static KEY: LazyLock<Option<[u8; 32]>> = LazyLock::new(|| {
    let mut key = [0_u8; 32];
    match getrandom::fill(&mut key) {
        Ok(()) => Some(key),
        Err(err) => {
            tracing::warn!(%err, "no random key for log identifiers, every id is logged as redacted");
            None
        }
    }
});

/// What a log line shows instead of an id: the first six bytes of SHA-256 over the per-process key
/// and the id, or `redacted` when the system gave no key.
#[derive(Clone, Copy)]
pub struct LogId(Option<[u8; 6]>);

impl fmt::Display for LogId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(short) = self.0 else {
            return f.write_str("redacted");
        };
        f.write_str("h-")?;
        short.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for LogId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[must_use]
#[inline]
pub fn log_id(raw: &str) -> LogId {
    LogId(KEY.as_ref().map(|key| {
        let digest = Sha256::new().chain_update(key).chain_update(raw.as_bytes()).finalize();
        let mut short = [0_u8; 6];
        short.iter_mut().zip(digest).for_each(|(out, byte)| *out = byte);
        short
    }))
}
