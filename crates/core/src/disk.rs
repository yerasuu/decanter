//! How much room is left on the filesystem a path lives on.
//!
//! For the storage screen: knowing every game's own `size_on_disk` (from
//! its manifest) isn't enough to say whether another install will fit —
//! that also needs how much of the disk they're all sitting on is still
//! free.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::path::Path;

use crate::platform;

/// `(free bytes, total bytes)` for the filesystem containing `path`.
///
/// `None` if the path doesn't exist yet or the syscall fails — the
/// storage screen just omits the free-space figure in that case rather
/// than guessing.
#[must_use]
#[inline]
pub fn disk_usage(path: &Path) -> Option<(u64, u64)> {
    let (free_bytes, total_bytes) = match platform::disk_space(path) {
        Ok(space) => space,
        Err(err) => {
            // Previously discarded via `.ok()?`; kept as a warn rather than
            // debug since the caller silently loses the free-space figure.
            tracing::warn!(path = %path.display(), error = %err, "cannot read free space, omitting it");
            return None;
        }
    };
    tracing::debug!(path = %path.display(), free_bytes, total_bytes, "read disk usage");
    Some((free_bytes, total_bytes))
}
