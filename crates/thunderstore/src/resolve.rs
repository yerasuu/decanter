// SPDX-License-Identifier: GPL-3.0-or-later
//! What installing one version takes: the package and, recursively, every `Team-Name-1.2.3` its
//! dependencies name, read from the same listing (analysis, section 2).
//!
//! A package already installed from Thunderstore (any version, by its `mod.toml` origin) is not
//! downloaded again; the plan names it, with the version asked for, so an older one is visible and
//! *Buscar actualizaciones* can bring it up. A package asked for twice gets the higher version.
//! A pinned version the listing no longer has is replaced by the latest one above it, and the log
//! says so; a package the community does not have refuses the whole plan, since the mod would not
//! work without it. Depth and count are capped and a package is expanded once per version, so a
//! cycle ends.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use decanter_mods::ModManifest;

use crate::id::{Dependency, PackageId, VersionNumber};
use crate::listing::{Listing, Package, PackageVersion};
use crate::{Error, PLATFORM, Result};

const MAX_DEPTH: usize = 10;
const MAX_PACKAGES: usize = 100;
/// Dependency strings looked at in all, which bounds the work on a listing that repeats itself.
const MAX_STEPS: usize = 2_000;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlanItem {
    pub package: PackageId,
    pub version: PackageVersion,
    /// Whether it is there because another package needs it, not because the user chose it.
    pub is_dependency: bool,
}

/// A dependency left out because a version of it is installed already.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Skipped {
    pub package: PackageId,
    pub installed: VersionNumber,
    pub wanted: VersionNumber,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Plan {
    /// Dependencies before what needs them, the chosen package last, so that it goes last in the
    /// load order and wins.
    pub items: Vec<PlanItem>,
    pub skipped: Vec<Skipped>,
}

/// The game's mods that came from Thunderstore, by package, with their highest version.
#[derive(Debug, Clone, Default)]
pub struct Installed(HashMap<PackageId, VersionNumber>);

impl Installed {
    #[must_use]
    #[inline]
    pub fn get(&self, package: &PackageId) -> Option<VersionNumber> {
        self.0.get(package).copied()
    }
}

#[must_use]
#[inline]
pub fn installed(mods: &[ModManifest]) -> Installed {
    let mut found = HashMap::new();
    for manifest in mods {
        let Some(origin) = manifest.origin.as_ref().filter(|origin| origin.platform == PLATFORM) else {
            continue;
        };
        if let (Ok(package), Ok(version)) = (
            origin.package.parse::<PackageId>(),
            origin.version.parse::<VersionNumber>(),
        ) {
            let known = found.entry(package).or_insert(version);
            *known = (*known).max(version);
        } else {
            tracing::warn!(mod_id = %manifest.id, "thunderstore origin of a mod does not parse, ignored");
        }
    }
    Installed(found)
}

/// The plan for `version` of `root`, against what `installed` already has.
///
/// # Errors
///
/// [`Error::Unresolved`] for a dependency the listing lacks (or lacks at that version or above),
/// [`Error::TooDeep`] and [`Error::TooManyDependencies`] past the caps.
#[inline]
pub fn plan(listing: &Listing, root: &PackageId, version: VersionNumber, installed: &Installed) -> Result<Plan> {
    let mut chosen: BTreeMap<PackageId, VersionNumber> = BTreeMap::new();
    let mut skipped: BTreeMap<PackageId, Skipped> = BTreeMap::new();
    let mut expanded = HashSet::new();
    let mut queue = VecDeque::from([(root.clone(), version, 0_usize)]);
    let mut steps = 0_usize;
    let mut substituted = 0_usize;
    while let Some((id, wanted, depth)) = queue.pop_front() {
        steps = steps.saturating_add(1);
        if steps > MAX_STEPS {
            return Err(Error::TooManyDependencies(MAX_PACKAGES));
        }
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep(MAX_DEPTH));
        }
        let is_root = depth == 0;
        if !is_root && id == *root {
            continue;
        }
        if !is_root && let Some(have) = installed.get(&id) {
            let entry = skipped.entry(id.clone()).or_insert(Skipped {
                package: id,
                installed: have,
                wanted,
            });
            entry.wanted = entry.wanted.max(wanted);
            continue;
        }
        let unresolved = || {
            Error::Unresolved(
                Dependency {
                    package: id.clone(),
                    version: wanted,
                }
                .to_string(),
            )
        };
        let package = listing.get(&id).ok_or_else(unresolved)?;
        let found = pick(package, wanted).ok_or_else(unresolved)?;
        if found.number != wanted {
            substituted = substituted.saturating_add(1);
            tracing::warn!(
                package = %id,
                wanted = %wanted,
                taken = %found.number,
                "pinned thunderstore version not listed, the latest one is taken"
            );
        }
        if chosen.get(&id).is_some_and(|&known| known >= found.number) {
            continue;
        }
        chosen.insert(id.clone(), found.number);
        if chosen.len() > MAX_PACKAGES {
            return Err(Error::TooManyDependencies(MAX_PACKAGES));
        }
        if expanded.insert((id, found.number)) {
            for dependency in &found.dependencies {
                queue.push_back((dependency.package.clone(), dependency.version, depth.saturating_add(1)));
            }
        }
    }
    let mut items = Vec::with_capacity(chosen.len());
    let mut seen = HashSet::new();
    order(listing, &chosen, root, &mut seen, &mut items);
    for item in &mut items {
        item.is_dependency = item.package != *root;
    }
    tracing::info!(
        package = %root,
        version = %version,
        downloads = items.len(),
        skipped_installed = skipped.len(),
        substituted,
        "thunderstore install planned"
    );
    Ok(Plan {
        items,
        skipped: skipped.into_values().collect(),
    })
}

/// `wanted` when listed, else the latest version above it.
fn pick(package: &Package, wanted: VersionNumber) -> Option<&PackageVersion> {
    package
        .version(wanted)
        .or_else(|| Some(&package.latest).filter(|latest| latest.number > wanted))
}

/// Appends `id`'s chosen dependencies before it, depth first; `seen` breaks cycles.
fn order(
    listing: &Listing,
    chosen: &BTreeMap<PackageId, VersionNumber>,
    id: &PackageId,
    seen: &mut HashSet<PackageId>,
    items: &mut Vec<PlanItem>,
) {
    if !seen.insert(id.clone()) {
        return;
    }
    let Some(&number) = chosen.get(id) else {
        return;
    };
    let Some(version) = listing.get(id).and_then(|package| package.version(number)) else {
        return;
    };
    for dependency in &version.dependencies {
        if chosen.contains_key(&dependency.package) {
            order(listing, chosen, &dependency.package, seen, items);
        }
    }
    items.push(PlanItem {
        package: id.clone(),
        version: version.clone(),
        is_dependency: true,
    });
}
