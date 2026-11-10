// SPDX-License-Identifier: GPL-3.0-or-later
//! A community's packages: `/c/{community}/api/v1/package/`, one JSON array of every active
//! package with its versions (the README documents `/api/v1/package/`; the community-scoped form
//! and the field names are secondary, analysis section 2).
//!
//! One request answers search, browse, a package's versions and every dependency, so the listing
//! is downloaded to the library's cache and kept for six hours instead of asking per package.
//! It can be large, so it is streamed to disk and read one package at a time: each is parsed on
//! its own, and one that does not hold up is skipped and counted rather than failing the rest.
//!
//! Search, sorts and filters run over the read listing, so typing asks Thunderstore nothing. The
//! `categories` field of a package is read when present; no official page documents it
//! (design, section 13), so a listing without it simply offers no category filter.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::BufReader;
use std::path::Path;

use serde::Deserialize;
use serde::de::{IgnoredAny, SeqAccess, Visitor};

use crate::download::{Fetch, stream_to_file};
use crate::http::Client;
use crate::id::{CommunityId, DOMAIN, Dependency, PackageId, VersionNumber, checked_url, date, display_text};
use crate::{Error, Refresh, Result};

const CALL: &str = "the package listing";
const TTL: Duration = Duration::from_hours(6);
/// The largest communities' listings run to hundreds of megabytes; past this something is wrong.
const MAX_LISTING_BYTES: u64 = 1 << 30;
const MAX_PACKAGES: usize = 300_000;
const MAX_VERSIONS: usize = 2_000;
const MAX_DEPENDENCIES: usize = 200;
/// The package format's limit on `description`, with room for older packages.
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_CATEGORIES: usize = 16;
const MAX_CATEGORY_CHARS: usize = 60;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Package {
    pub id: PackageId,
    /// The latest version's, shown as plain text.
    pub description: String,
    /// The latest version's icon, an HTTPS address on a Thunderstore host.
    pub icon: Option<String>,
    /// Its page on Thunderstore, likewise checked.
    pub page: Option<String>,
    pub rating: i64,
    /// Every version's downloads summed.
    pub downloads: u64,
    /// `YYYY-MM-DD`.
    pub updated: Option<String>,
    pub is_deprecated: bool,
    pub is_pinned: bool,
    pub has_nsfw_content: bool,
    /// The community's categories the package is filed under, when the listing names them.
    pub categories: Vec<String>,
    pub latest: PackageVersion,
    /// Newest first.
    pub older: Vec<PackageVersion>,
}

impl Package {
    /// Newest first, the latest included.
    #[inline]
    pub fn versions(&self) -> impl Iterator<Item = &PackageVersion> {
        core::iter::once(&self.latest).chain(&self.older)
    }

    #[must_use]
    #[inline]
    pub fn version(&self, number: VersionNumber) -> Option<&PackageVersion> {
        self.versions().find(|version| version.number == number)
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PackageVersion {
    pub number: VersionNumber,
    pub dependencies: Vec<Dependency>,
    pub downloads: u64,
    /// `YYYY-MM-DD`.
    pub created: Option<String>,
    /// The zip's size when the listing gives it, which the download must then match.
    pub file_size: Option<u64>,
    /// HTTPS on a Thunderstore host.
    pub download_url: url::Url,
    /// The manifest's `website_url`, which the author may point anywhere; checked as a credit
    /// link before it is shown.
    pub website: Option<String>,
}

/// A community's packages, as read from its listing.
#[derive(Debug)]
pub struct Listing {
    community: CommunityId,
    packages: Vec<Package>,
    by_id: HashMap<PackageId, usize>,
    categories: Vec<String>,
}

impl Listing {
    #[must_use]
    #[inline]
    pub const fn community(&self) -> &CommunityId {
        &self.community
    }

    #[must_use]
    #[inline]
    pub fn get(&self, id: &PackageId) -> Option<&Package> {
        self.packages.get(*self.by_id.get(id)?)
    }

    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.packages.len()
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    #[must_use]
    #[inline]
    pub fn at(&self, index: usize) -> Option<&Package> {
        self.packages.get(index)
    }

    /// Every category a package of the listing is filed under, sorted, without repeats.
    #[must_use]
    #[inline]
    pub fn categories(&self) -> &[String] {
        &self.categories
    }

    /// The listing's indices ([`Self::at`]) of the packages matching every word of the query in
    /// their name, team or description and passing its filters, in its order. Relevance puts name
    /// matches first and then the most downloaded; with no words, pinned ones first.
    #[must_use]
    #[inline]
    pub fn find(&self, query: &Query) -> Vec<usize> {
        let words: Vec<String> = query.text.split_whitespace().map(str::to_lowercase).collect();
        let mut found: Vec<(u8, usize)> = self
            .packages
            .iter()
            .enumerate()
            .filter(|&(_, package)| {
                (!query.hide_deprecated || !package.is_deprecated)
                    && (!query.hide_nsfw || !package.has_nsfw_content)
                    && query
                        .category
                        .as_ref()
                        .is_none_or(|wanted| package.categories.contains(wanted))
            })
            .filter_map(|(index, package)| Some((relevance(package, &words)?, index)))
            .collect();
        let packages = &self.packages;
        let package = |index: usize| packages.get(index);
        found.sort_by(|a, b| {
            let (Some(first), Some(second)) = (package(a.1), package(b.1)) else {
                return core::cmp::Ordering::Equal;
            };
            let by_downloads = second.downloads.cmp(&first.downloads);
            match query.sort {
                Sort::Relevance => b.0.cmp(&a.0).then(by_downloads),
                Sort::Downloads => by_downloads,
                Sort::Rating => second.rating.cmp(&first.rating).then(by_downloads),
                Sort::Updated => second.updated.cmp(&first.updated).then(by_downloads),
                Sort::Name => first
                    .id
                    .display_name()
                    .to_lowercase()
                    .cmp(&second.id.display_name().to_lowercase()),
            }
        });
        found.into_iter().map(|(_, index)| index).collect()
    }
}

/// What a search over a listing asks for.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Query {
    pub text: String,
    pub sort: Sort,
    pub hide_deprecated: bool,
    pub hide_nsfw: bool,
    /// One of [`Listing::categories`].
    pub category: Option<String>,
}

impl Query {
    /// No words, by relevance, deprecated and NSFW packages hidden, any category.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self {
            hide_deprecated: true,
            hide_nsfw: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Sort {
    #[default]
    Relevance,
    Downloads,
    Rating,
    Updated,
    Name,
}

/// 2 when every word is in the name, 1 when one is only in the team or description, `None` when
/// one is nowhere; every package is a 1 or, pinned, a 2 for no words at all.
fn relevance(package: &Package, words: &[String]) -> Option<u8> {
    if words.is_empty() {
        return Some(u8::from(package.is_pinned).saturating_add(1));
    }
    let name = package.id.display_name().to_lowercase();
    let team = package.id.namespace().to_lowercase();
    let description = package.description.to_lowercase();
    let mut score = 2_u8;
    for word in words {
        if name.contains(word.as_str()) {
            continue;
        }
        if team.contains(word.as_str()) || description.contains(word.as_str()) {
            score = 1;
        } else {
            return None;
        }
    }
    Some(score)
}

/// `community`'s listing, from the cache while it is fresh.
///
/// # Errors
///
/// As a request, [`Error::TooLarge`] past 1 `GiB`, [`Error::Malformed`] when it is not an array or no
/// package in it holds up, or [`Error::Io`] when the cache can't be written or read.
#[inline]
pub async fn listing(client: &Client, cache: &Path, community: &CommunityId, refresh: Refresh) -> Result<Listing> {
    let dir = cache.join("listings");
    let path = dir.join(format!("{community}.json"));
    let is_fresh = fs::metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age <= TTL);
    if refresh == Refresh::Now || !is_fresh {
        let url = checked_url(&format!("https://{DOMAIN}/c/{community}/api/v1/package/"))
            .ok_or_else(|| Error::InvalidCommunity(community.to_string()))?;
        fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
        let partial = dir.join(format!("{community}.json.{}.part", std::process::id()));
        let fetch = Fetch {
            call: CALL,
            limit: MAX_LISTING_BYTES,
            expected: None,
            transfer: None,
        };
        stream_to_file(client, url, &partial, &fetch).await?;
        fs::rename(&partial, &path).map_err(Error::io(&path))?;
    }
    let owned = community.clone();
    let read = tokio::task::spawn_blocking(move || read_listing(&path, owned)).await;
    match read {
        Ok(listing) => listing,
        Err(err) => Err(Error::malformed(CALL, format!("reading it stopped: {err}"))),
    }
}

fn remove_quietly(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %err, "could not remove a partial download");
    }
}

fn read_listing(path: &Path, community: CommunityId) -> Result<Listing> {
    let file = fs::File::open(path).map_err(Error::io(path))?;
    let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(file));
    let parsed = serde::Deserializer::deserialize_seq(&mut deserializer, PackagesVisitor)
        .and_then(|read| deserializer.end().map(|()| read));
    let read = match parsed {
        Ok(read) => read,
        Err(err) => {
            // A cache that does not parse is dropped, so that the next attempt asks again.
            remove_quietly(path);
            return Err(Error::malformed(
                CALL,
                format!(
                    "not a JSON array of packages at line {}, column {}",
                    err.line(),
                    err.column()
                ),
            ));
        }
    };
    tracing::info!(
        %community,
        packages = read.packages.len(),
        skipped_packages = read.skipped_packages,
        skipped_versions = read.skipped_versions,
        "thunderstore listing read"
    );
    if read.packages.is_empty() && read.skipped_packages > 0 {
        remove_quietly(path);
        return Err(Error::malformed(
            CALL,
            format!("none of its {} packages could be read", read.skipped_packages),
        ));
    }
    let by_id = read
        .packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.id.clone(), index))
        .collect();
    let categories = read
        .packages
        .iter()
        .flat_map(|package| package.categories.iter().map(String::as_str))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(Listing {
        community,
        packages: read.packages,
        by_id,
        categories,
    })
}

#[derive(Default)]
struct Read {
    packages: Vec<Package>,
    skipped_packages: usize,
    skipped_versions: usize,
}

struct PackagesVisitor;

#[expect(
    clippy::missing_trait_methods,
    reason = "only a sequence is a listing; serde's defaults refuse the rest"
)]
impl<'de> Visitor<'de> for PackagesVisitor {
    type Value = Read;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of packages")
    }

    fn visit_seq<A>(self, mut seq: A) -> core::result::Result<Read, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut read = Read::default();
        while let Some(value) = seq.next_element::<serde_json::Value>()? {
            if read.packages.len() >= MAX_PACKAGES {
                read.skipped_packages = read.skipped_packages.saturating_add(1);
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    read.skipped_packages = read.skipped_packages.saturating_add(1);
                }
                break;
            }
            match serde_json::from_value::<RawPackage>(value) {
                Ok(raw) => match package(raw, &mut read.skipped_versions) {
                    Some(package) => read.packages.push(package),
                    None => read.skipped_packages = read.skipped_packages.saturating_add(1),
                },
                Err(err) => {
                    read.skipped_packages = read.skipped_packages.saturating_add(1);
                    tracing::trace!(%err, "thunderstore package skipped");
                }
            }
        }
        Ok(read)
    }
}

#[derive(Deserialize)]
struct RawPackage {
    name: String,
    owner: String,
    #[serde(default)]
    package_url: Option<String>,
    #[serde(default)]
    rating_score: Option<i64>,
    #[serde(default)]
    is_pinned: Option<bool>,
    #[serde(default)]
    is_deprecated: Option<bool>,
    #[serde(default)]
    has_nsfw_content: Option<bool>,
    #[serde(default)]
    date_updated: Option<String>,
    #[serde(default)]
    categories: Vec<serde_json::Value>,
    #[serde(default)]
    versions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawVersion {
    version_number: String,
    download_url: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    date_created: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
    #[serde(default)]
    is_active: Option<bool>,
    #[serde(default)]
    website_url: Option<String>,
}

/// A package whose name and at least one version hold up; its other versions that don't are
/// counted in `skipped_versions`.
fn package(raw: RawPackage, skipped_versions: &mut usize) -> Option<Package> {
    let id = PackageId::new(&raw.owner, &raw.name).ok()?;
    let mut details = None;
    let mut versions = Vec::with_capacity(raw.versions.len().min(MAX_VERSIONS));
    for value in raw.versions.into_iter().take(MAX_VERSIONS) {
        let checked = serde_json::from_value::<RawVersion>(value)
            .ok()
            .and_then(|raw_version| version(&raw_version));
        match checked {
            Some((version, description, icon)) => {
                if details.as_ref().is_none_or(|&(number, _, _)| version.number > number) {
                    details = Some((version.number, description, icon));
                }
                versions.push(version);
            }
            None => *skipped_versions = skipped_versions.saturating_add(1),
        }
    }
    versions.sort_by_key(|version| core::cmp::Reverse(version.number));
    versions.dedup_by_key(|version| version.number);
    let mut newest_first = versions.into_iter();
    let latest = newest_first.next()?;
    let older: Vec<PackageVersion> = newest_first.collect();
    let (_, description, icon) = details?;
    Some(Package {
        id,
        description,
        icon,
        page: raw.package_url.as_deref().and_then(checked_url).map(String::from),
        rating: raw.rating_score.unwrap_or_default(),
        downloads: core::iter::once(&latest)
            .chain(&older)
            .fold(0_u64, |sum, version| sum.saturating_add(version.downloads)),
        updated: raw.date_updated.as_deref().and_then(date),
        is_deprecated: raw.is_deprecated.unwrap_or_default(),
        is_pinned: raw.is_pinned.unwrap_or_default(),
        has_nsfw_content: raw.has_nsfw_content.unwrap_or_default(),
        categories: raw
            .categories
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|text| display_text(text, MAX_CATEGORY_CHARS))
            .filter(|text| !text.is_empty())
            .take(MAX_CATEGORIES)
            .collect(),
        latest,
        older,
    })
}

/// A version that can be installed: a version number, dependency strings that parse and a
/// download address on Thunderstore. One whose dependencies can't be read is left out, since
/// installing it without them would break it.
fn version(raw: &RawVersion) -> Option<(PackageVersion, String, Option<String>)> {
    if raw.is_active == Some(false) || raw.dependencies.len() > MAX_DEPENDENCIES {
        return None;
    }
    let number = raw.version_number.parse().ok()?;
    let download_url = checked_url(&raw.download_url)?;
    let dependencies = raw
        .dependencies
        .iter()
        .map(|text| text.parse::<Dependency>())
        .collect::<Result<Vec<_>>>()
        .ok()?;
    let description = raw
        .description
        .as_deref()
        .map(|text| display_text(text, MAX_DESCRIPTION_CHARS))
        .unwrap_or_default();
    let icon = raw.icon.as_deref().and_then(checked_url).map(String::from);
    Some((
        PackageVersion {
            number,
            dependencies,
            downloads: raw.downloads.unwrap_or_default(),
            created: raw.date_created.as_deref().and_then(date),
            file_size: raw.file_size,
            download_url,
            website: raw
                .website_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
        },
        description,
        icon,
    ))
}
