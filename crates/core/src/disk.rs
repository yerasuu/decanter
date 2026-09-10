//! How much room is left on the filesystem a path lives on.
//!
//! For the storage screen: knowing every game's own `size_on_disk` (from
//! its manifest) isn't enough to say whether another install will fit —
//! that also needs how much of the disk they're all sitting on is still
//! free.

// Only fires on some compilation targets (lib vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::self_named_module_files,
    reason = "convention here is `<name>.rs` plus a sibling `<name>/tests.rs`, see CLAUDE.md"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::path::Path;

use nix::sys::statvfs::statvfs;

/// `(free bytes, total bytes)` for the filesystem containing `path`.
///
/// `None` if the path doesn't exist yet or the syscall fails — the
/// storage screen just omits the free-space figure in that case rather
/// than guessing.
#[must_use]
#[inline]
pub fn disk_usage(path: &Path) -> Option<(u64, u64)> {
    let stat = statvfs(path).ok()?;
    let block = stat.fragment_size();
    Some((
        stat.blocks_available().saturating_mul(block),
        stat.blocks().saturating_mul(block),
    ))
}

#[cfg(test)]
mod tests;
