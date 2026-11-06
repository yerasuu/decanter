// SPDX-License-Identifier: GPL-3.0-or-later
//! What an install or an update has to fetch and delete, by comparing the new manifest with the one
//! saved at the last install (docs/amazon-nile-integration-analysis.md, section 4.3). Files are
//! whole objects: Amazon's delta format is unknown and unused, so a changed file is fetched again.

use std::collections::{HashMap, HashSet};

use super::{Manifest, ManifestFile, ManifestPath};

/// The work between two versions of a game.
#[derive(Debug)]
#[non_exhaustive]
pub struct Plan<'manifest> {
    /// Files of the new manifest that are new, or whose hash or size changed, in manifest order.
    pub fetch: Vec<&'manifest ManifestFile>,
    /// Files of the old manifest the new one no longer lists, in the old manifest's order.
    pub remove: Vec<&'manifest ManifestPath>,
    /// The sum of the sizes in `fetch`.
    pub download_size: u64,
}

/// Plans an update from `old` to `new`, or a fresh install without `old`.
///
/// Paths are matched exactly, on a case-sensitive library: a file whose name changes only in case
/// is fetched under its new name and the old name removed, so a caller on a case-insensitive file
/// system must delete before it writes.
#[inline]
#[must_use]
pub fn plan<'manifest>(new: &'manifest Manifest, old: Option<&'manifest Manifest>) -> Plan<'manifest> {
    let previous: HashMap<_, _> = old
        .map(|manifest| manifest.files.iter().map(|file| (file.path.as_str(), file)).collect())
        .unwrap_or_default();
    let fetch: Vec<_> = new
        .files
        .iter()
        .filter(|file| {
            previous
                .get(file.path.as_str())
                .is_none_or(|before| before.sha256 != file.sha256 || before.size != file.size)
        })
        .collect();
    // `new.total_size` fit a u64 when the manifest was read, and `fetch` is part of it.
    let download_size = fetch.iter().fold(0_u64, |sum, file| sum.saturating_add(file.size));
    let current: HashSet<_> = new.files.iter().map(|file| file.path.as_str()).collect();
    let remove = old
        .map(|manifest| {
            manifest
                .files
                .iter()
                .map(|file| &file.path)
                .filter(|path| !current.contains(path.as_str()))
                .collect()
        })
        .unwrap_or_default();
    Plan {
        fetch,
        remove,
        download_size,
    }
}
