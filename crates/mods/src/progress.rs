//! How far an archive's reading or extraction has got, written from the import's blocking thread
//! and read by the Mods tab on its own timer, which also asks it to stop.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{Error, Result};

#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    stop: AtomicBool,
}

impl Progress {
    /// `(done, total)` bytes, or `None` before the total is known.
    #[must_use]
    #[inline]
    pub fn snapshot(&self) -> Option<(u64, u64)> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.done.load(Ordering::Relaxed).min(total), total))
    }

    /// Asks the work reporting here to stop; it then ends with [`Error::Cancelled`] and leaves
    /// nothing behind.
    #[inline]
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    #[must_use]
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(crate) fn start(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    /// Counts `bytes` more, failing once a cancel was asked for.
    pub(crate) fn advance(&self, bytes: u64) -> Result<()> {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        if self.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}
