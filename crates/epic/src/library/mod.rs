// SPDX-License-Identifier: GPL-3.0-or-later
//! The account's Epic library, read only: the asset list, the entitlements and each title's
//! catalog item, filtered and grouped by the rules of spec section 4.6.
//!
//! The asset list runs beside the rest on its own longer limit, since it is the call seen to stall
//! (spec, section 12). The entitlements and their catalog items are reported as soon as they are
//! in, so a stalled asset list still leaves the owned titles on screen; then the asset list, when
//! it answers, adds the installable titles the entitlements did not name.
//!
//! Every count of what was skipped, and of how each catalog call failed, is logged at the end of a
//! listing, with no URL and no token.

pub(crate) mod assets;
mod catalog;
mod entitlements;

use core::time::Duration;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use decanter_core::secrets::Secret;
use tokio::task::JoinSet;

use self::assets::Assets;
use self::catalog::{Meta, Seen};
use crate::error::CallError;
use crate::http;
use crate::session::{self, Live, Session};

/// The namespace of Unreal Engine marketplace content (spec, 4.6 rule 1).
const UNREAL_NAMESPACE: &str = "ue";
/// The namespace every Unreal Engine Marketplace / Fab item (plugins, asset packs, projects) of the
/// owner's account shares, as seen in its first real listing; spec 4.6 rule 1 only knew `ue`.
const MARKETPLACE_NAMESPACE: &str = "89efe5924d3d467c839449ab6ab52e7f";
/// Category paths listed in the log, most common first.
const LOGGED_CATEGORY_PATHS: usize = 30;
/// Spec 13.11 starts at four parallel catalog calls, against legendary's sixteen, since Epic's
/// rate limits are unknown.
const CATALOG_PARALLEL: usize = 4;
/// Catalog calls in one listing; the titles past it are left for a listing that is not complete.
const MAX_CATALOG_CALLS: usize = 5_000;
const MAX_GAMES: usize = 10_000;
const ID_MAX_LEN: usize = 128;

/// The `country` and `locale` the catalog is asked in (spec, 4.3), which change the titles and
/// members it answers with (spec, 13.6).
#[derive(Debug, Clone)]
pub struct CatalogRegion {
    language: String,
    country: String,
}

impl CatalogRegion {
    /// The parts that are not two or three ASCII letters fall back to legendary's `en` and `US`.
    /// Only the bare language is sent, as legendary does: whether `es-ES` is accepted is open
    /// (spec, Q11).
    #[inline]
    #[must_use]
    pub fn new(language: &str, country: &str) -> Self {
        Self {
            language: code(language).unwrap_or("en").to_ascii_lowercase(),
            country: code(country).unwrap_or("US").to_ascii_uppercase(),
        }
    }
}

fn code(value: &str) -> Option<&str> {
    ((2..=3).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphabetic())).then_some(value)
}

/// One title of the library: a game, or a DLC filed under its game.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EpicGame {
    /// The title's id in Epic's API, and so in decanter.
    pub app_name: String,
    /// Epic's sandbox id, which achievements are asked by.
    pub namespace: String,
    pub catalog_item_id: String,
    /// The catalog's title, or the `appName` when the catalog gave none.
    pub title: String,
    /// The game's `appName`, for a DLC.
    pub parent_app_name: Option<String>,
    pub icon_url: Option<String>,
    pub cover_url: Option<String>,
    pub background_url: Option<String>,
}

/// Where the asset list stands in a listing.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AssetList {
    /// Still waited on: a listing reported early, while it runs.
    Pending,
    Listed,
    /// No answer within `waited`, the longer limit of this call.
    TimedOut {
        waited: Duration,
    },
    /// Any other failure, as one message.
    Failed(String),
}

/// What a listing found.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Listing {
    pub games: Vec<EpicGame>,
    pub assets: AssetList,
    /// Whether every source answered whole, so that a title missing from `games` is no longer
    /// owned. A listing that is not complete must not prune anything.
    pub complete: bool,
}

/// What [`list_games`] found.
#[derive(Debug)]
#[non_exhaustive]
pub enum Library {
    Listed(Listing),
    /// Epic refused the session, or none is stored: only a new browser login brings it back.
    Closed,
    /// Neither the entitlements nor the asset list answered: nothing is known about the library,
    /// so what is cached should stay.
    Unreachable(String),
}

/// Lists the account's library. `early` is given the titles the entitlements named, with the
/// asset list [`AssetList::Pending`], when the asset list is still running once they are in.
///
/// # Errors
///
/// Only a local problem: the keyring, a stored session that does not parse, the HTTP client.
/// Anything Epic says or fails to say is a [`Library`].
#[inline]
pub async fn list_games<F>(account_id: &str, region: CatalogRegion, mut early: F) -> anyhow::Result<Library>
where
    F: FnMut(Listing) + Send,
{
    let mut session = match session::live(account_id).await? {
        Live::Ready(session) => session,
        Live::Closed => return Ok(Library::Closed),
        Live::Unreachable(reason) => return Ok(Library::Unreachable(reason)),
    };
    let http = http::client()?;
    let started = Instant::now();
    let mut run = Run {
        http,
        token: token_of(&session),
        region: Arc::new(region),
        found: Found::default(),
        tally: Tally::default(),
    };
    // A JoinSet so that a listing dropped half way drops the asset call with it.
    let mut asset_call = JoinSet::new();
    asset_call.spawn(assets::timed(run.http.clone(), Arc::clone(&run.token)));

    let mut renewed = false;
    let mut owned = entitlements::list(&run.http, &run.token, account_id).await;
    if matches!(owned, Err(CallError::Revoked { .. })) {
        match session::renew_refused(&session).await? {
            Live::Ready(fresh) => session = fresh,
            Live::Closed => return Ok(Library::Closed),
            Live::Unreachable(reason) => return Ok(Library::Unreachable(reason)),
        }
        renewed = true;
        run.token = token_of(&session);
        owned = entitlements::list(&run.http, &run.token, account_id).await;
    }
    let owned = match owned {
        Ok(owned) => {
            run.tally.entitlements(&owned);
            let wanted = owned
                .owned
                .into_iter()
                .map(|item| (item.namespace, item.catalog_item_id))
                .collect();
            run.describe(wanted).await;
            Ok(owned.complete)
        }
        Err(CallError::Setup(err)) => return Err(anyhow::Error::new(CallError::Setup(err))),
        Err(err) => {
            tracing::warn!(account_id, error = %err, "epic did not answer the entitlement list");
            Err(session::reason(err))
        }
    };
    let mut answered = asset_call.try_join_next();
    if owned.is_ok() && answered.is_none() {
        early(run.found.listing(AssetList::Pending, false, &mut run.tally));
    }
    if answered.is_none() {
        answered = asset_call.join_next().await;
    }

    let (mut assets, mut waited) = joined(answered);
    if matches!(assets, Err(CallError::Revoked { .. })) {
        if !renewed {
            match session::renew_refused(&session).await? {
                Live::Ready(fresh) => session = fresh,
                Live::Closed => return Ok(Library::Closed),
                Live::Unreachable(reason) => return Ok(Library::Unreachable(reason)),
            }
            run.token = token_of(&session);
        }
        (assets, waited) = assets::timed(run.http.clone(), Arc::clone(&run.token)).await;
    }
    let elapsed_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
    let asset_list = match assets {
        Ok(listed) => {
            tracing::info!(
                account_id,
                elapsed_ms,
                assets = listed.list.len(),
                "epic answered the asset list"
            );
            run.found.add_assets(&listed, &mut run.tally);
            let unasked = run.found.undescribed();
            run.describe(unasked).await;
            AssetList::Listed
        }
        Err(CallError::TimedOut { .. }) => {
            tracing::warn!(account_id, elapsed_ms, "epic did not answer the asset list in time");
            AssetList::TimedOut {
                waited: assets::TIMEOUT,
            }
        }
        Err(CallError::Setup(err)) => return Err(anyhow::Error::new(CallError::Setup(err))),
        Err(err) => {
            let reason = session::reason(err);
            tracing::warn!(account_id, elapsed_ms, reason, "epic did not answer the asset list");
            AssetList::Failed(reason)
        }
    };

    let complete = matches!(asset_list, AssetList::Listed)
        && owned == Ok(true)
        && run.tally.catalog_failed.is_empty()
        && run.tally.catalog_unasked == 0;
    if let (Err(reason), AssetList::TimedOut { .. } | AssetList::Failed(_)) = (&owned, &asset_list) {
        run.tally.log(account_id, started.elapsed());
        return Ok(Library::Unreachable(reason.clone()));
    }
    let listing = run.found.listing(asset_list, complete, &mut run.tally);
    run.tally.log(account_id, started.elapsed());
    tracing::info!(
        account_id,
        games = listing.games.len(),
        complete,
        "listed the epic library"
    );
    Ok(Library::Listed(listing))
}

/// The state one listing carries between its steps.
struct Run {
    http: reqwest::Client,
    token: Arc<Secret>,
    region: Arc<CatalogRegion>,
    found: Found,
    tally: Tally,
}

impl Run {
    /// Asks the catalog for each item, [`CATALOG_PARALLEL`] at a time.
    async fn describe(&mut self, wanted: Vec<(String, String)>) {
        let wanted: Vec<_> = wanted
            .into_iter()
            .filter(|(namespace, catalog_item_id)| self.found.want(namespace, catalog_item_id))
            .collect();
        let mut queue = wanted.into_iter();
        let mut calls = JoinSet::new();
        loop {
            while calls.len() < CATALOG_PARALLEL {
                let Some((namespace, catalog_item_id)) = queue.next() else {
                    break;
                };
                if self.tally.catalog_calls >= MAX_CATALOG_CALLS {
                    self.tally.catalog_unasked = self.tally.catalog_unasked.saturating_add(1);
                    continue;
                }
                self.tally.catalog_calls = self.tally.catalog_calls.saturating_add(1);
                calls.spawn(catalog::item(
                    self.http.clone(),
                    Arc::clone(&self.token),
                    Arc::clone(&self.region),
                    namespace,
                    catalog_item_id,
                ));
            }
            let Some(joined) = calls.join_next().await else {
                break;
            };
            match joined {
                Ok((catalog_item_id, Ok(Some(raw)))) => {
                    let meta = catalog::meta(&raw, &mut self.tally.seen);
                    self.found.metas.insert(catalog_item_id, meta);
                }
                Ok((_, Ok(None))) => self.tally.catalog_absent = self.tally.catalog_absent.saturating_add(1),
                Ok((_, Err(err))) => self.tally.catalog_failed(failure_class(&err)),
                Err(err) => {
                    tracing::warn!(error = %err, "an epic catalog call task failed");
                    self.tally.catalog_failed("task");
                }
            }
        }
    }
}

/// One owned catalog item: its namespace, and its `appName` when the asset list named it.
struct Candidate {
    namespace: String,
    app_name: Option<String>,
}

#[derive(Default)]
struct Found {
    /// By catalog item id, ordered so that a listing comes out the same each time.
    candidates: BTreeMap<String, Candidate>,
    metas: HashMap<String, Meta>,
    /// Catalog items already asked for, whatever the answer.
    asked: HashSet<String>,
}

impl Found {
    /// Records an owned item and whether its catalog item is still to be asked for.
    fn want(&mut self, namespace: &str, catalog_item_id: &str) -> bool {
        self.candidates
            .entry(catalog_item_id.to_owned())
            .or_insert_with(|| Candidate {
                namespace: namespace.to_owned(),
                app_name: None,
            });
        self.asked.insert(catalog_item_id.to_owned())
    }

    fn add_assets(&mut self, listed: &Assets, tally: &mut Tally) {
        tally.assets_skipped = listed.skipped;
        tally.assets_unreal = listed.unreal;
        for asset in &listed.list {
            let candidate = self
                .candidates
                .entry(asset.catalog_item_id.clone())
                .or_insert_with(|| Candidate {
                    namespace: asset.namespace.clone(),
                    app_name: None,
                });
            candidate.app_name = Some(asset.app_name.clone());
        }
    }

    fn undescribed(&self) -> Vec<(String, String)> {
        self.candidates
            .iter()
            .filter(|(catalog_item_id, _)| !self.asked.contains(*catalog_item_id))
            .map(|(catalog_item_id, candidate)| (candidate.namespace.clone(), catalog_item_id.clone()))
            .collect()
    }

    /// The titles as they stand, by spec 4.6: mods out, DLC under the game its `mainGameItem`
    /// names, and an item without an `appName` out, since nothing could install or launch it.
    fn listing(&self, assets: AssetList, complete: bool, tally: &mut Tally) -> Listing {
        let app_names: HashMap<&str, &str> = self
            .candidates
            .iter()
            .filter_map(|(catalog_item_id, candidate)| {
                let app_name = candidate
                    .app_name
                    .as_deref()
                    .or_else(|| self.metas.get(catalog_item_id)?.app_name.as_deref())?;
                Some((catalog_item_id.as_str(), app_name))
            })
            .collect();
        let mut counts = Counts::default();
        let mut seen = HashSet::new();
        let mut games = Vec::new();
        for (catalog_item_id, candidate) in &self.candidates {
            let meta = self.metas.get(catalog_item_id);
            if meta.is_some_and(|meta| meta.is_mod) {
                counts.mods = counts.mods.saturating_add(1);
                continue;
            }
            if candidate.namespace == MARKETPLACE_NAMESPACE {
                counts.marketplace_namespace = counts.marketplace_namespace.saturating_add(1);
                continue;
            }
            if meta.is_some_and(|meta| meta.is_non_game) {
                counts.non_game_category = counts.non_game_category.saturating_add(1);
                continue;
            }
            let Some(app_name) = app_names.get(catalog_item_id.as_str()) else {
                counts.no_app_name = counts.no_app_name.saturating_add(1);
                continue;
            };
            let parent_app_name = match meta.and_then(|meta| meta.main_game.as_ref()) {
                Some(main) => {
                    let parent = main
                        .catalog_item_id
                        .as_deref()
                        .and_then(|id| app_names.get(id).copied())
                        .or(main.app_name.as_deref());
                    let Some(parent) = parent.filter(|parent| parent != app_name) else {
                        counts.orphan_dlc = counts.orphan_dlc.saturating_add(1);
                        continue;
                    };
                    counts.dlc = counts.dlc.saturating_add(1);
                    Some((*parent).to_owned())
                }
                None => None,
            };
            if games.len() >= MAX_GAMES || !seen.insert(*app_name) {
                counts.repeated = counts.repeated.saturating_add(1);
                continue;
            }
            let art = meta.map(|meta| &meta.art);
            games.push(EpicGame {
                app_name: (*app_name).to_owned(),
                namespace: candidate.namespace.clone(),
                catalog_item_id: catalog_item_id.clone(),
                title: meta
                    .and_then(|meta| meta.title.clone())
                    .unwrap_or_else(|| (*app_name).to_owned()),
                parent_app_name,
                icon_url: art.and_then(|art| art.icon.clone()),
                cover_url: art.and_then(|art| art.cover.clone()),
                background_url: art.and_then(|art| art.background.clone()),
            });
        }
        tally.listed = counts;
        Listing {
            games,
            assets,
            complete,
        }
    }
}

/// What the last [`Found::listing`] left out, and why.
#[derive(Default, Debug)]
struct Counts {
    mods: usize,
    marketplace_namespace: usize,
    non_game_category: usize,
    dlc: usize,
    orphan_dlc: usize,
    no_app_name: usize,
    repeated: usize,
}

#[derive(Default)]
struct Tally {
    entitlement_records: usize,
    entitlements_skipped: usize,
    entitlements_unreal: usize,
    entitlements_private: usize,
    assets_skipped: usize,
    assets_unreal: usize,
    catalog_calls: usize,
    catalog_absent: usize,
    catalog_unasked: usize,
    catalog_failed: BTreeMap<&'static str, usize>,
    listed: Counts,
    seen: Seen,
}

impl Tally {
    const fn entitlements(&mut self, owned: &entitlements::Entitlements) {
        self.entitlement_records = owned.records;
        self.entitlements_skipped = owned.skipped;
        self.entitlements_unreal = owned.unreal;
        self.entitlements_private = owned.private;
    }

    fn catalog_failed(&mut self, class: &'static str) {
        let count = self.catalog_failed.entry(class).or_default();
        *count = count.saturating_add(1);
    }

    /// The most common category paths, as `path=count`.
    fn top_category_paths(&self) -> Vec<String> {
        let mut paths: Vec<_> = self.seen.category_paths.iter().collect();
        paths.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        paths
            .into_iter()
            .take(LOGGED_CATEGORY_PATHS)
            .map(|(path, count)| format!("{path}={count}"))
            .collect()
    }

    fn log(&self, account_id: &str, elapsed: Duration) {
        tracing::info!(
            account_id,
            elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            entitlement_records = self.entitlement_records,
            entitlements_skipped = self.entitlements_skipped,
            entitlements_unreal = self.entitlements_unreal,
            entitlements_private = self.entitlements_private,
            assets_skipped = self.assets_skipped,
            assets_unreal = self.assets_unreal,
            catalog_calls = self.catalog_calls,
            catalog_absent = self.catalog_absent,
            catalog_unasked = self.catalog_unasked,
            catalog_failed = ?self.catalog_failed,
            left_out = ?self.listed,
            catalog_members = ?self.seen.members,
            image_types = ?self.seen.image_types,
            category_paths = ?self.top_category_paths(),
            "epic library listing tally"
        );
    }
}

/// How a call failed, for the counts: the error itself may differ per item and is not repeated
/// a thousand times in the log.
const fn failure_class(err: &CallError) -> &'static str {
    match err {
        CallError::Revoked { .. } => "refused",
        CallError::ActionRequired { .. } => "action_required",
        CallError::TimedOut { .. } => "timed_out",
        CallError::Unreachable { .. } => "unreachable",
        CallError::Moved { .. } => "moved",
        CallError::Unavailable { .. } => "unavailable",
        CallError::Malformed { .. } => "malformed",
        CallError::Setup(_) => "setup",
    }
}

/// The asset list's answer and how long it took.
type Timed = (Result<Assets, CallError>, Duration);

fn joined(joined: Option<Result<Timed, tokio::task::JoinError>>) -> Timed {
    match joined {
        Some(Ok(answered)) => answered,
        Some(Err(err)) => (
            Err(CallError::Malformed {
                call: "asset list",
                reason: format!("the call's task failed: {err}"),
            }),
            Duration::ZERO,
        ),
        None => (
            Err(CallError::Malformed {
                call: "asset list",
                reason: "the call's task was lost".to_owned(),
            }),
            Duration::ZERO,
        ),
    }
}

/// A copy of the access token the spawned calls can share.
fn token_of(session: &Session) -> Arc<Secret> {
    Arc::new(Secret::from(session.access_token().expose().to_owned()))
}

fn is_unreal(namespace: &str) -> bool {
    namespace == UNREAL_NAMESPACE
}

/// Ids go into request paths and become database keys, so only these characters pass, and the
/// first is a letter or digit so that no id reads as `.` or `..`.
pub(crate) fn is_id(id: &str) -> bool {
    (1..=ID_MAX_LEN).contains(&id.len())
        && id.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

/// Control characters go and the text is cut at `max_chars`; `None` when nothing is left.
fn printable(text: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).take(max_chars).collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}
