//! The Epic Games half of the library (kb #1546), read only: one listing loop per Epic account of
//! the profile, started with the account list and repeated on the GOG loops' interval.
//!
//! A listing can be reported twice, since Epic's asset list is the call seen to stall (spec,
//! section 12): first what the entitlements and the catalog gave, then the whole of it once the
//! asset list answers or times out. Where that listing stands is kept per account for its row in
//! Settings.

use std::collections::{HashMap, HashSet};

use decanter_core::{CachedGame, Db, Game, Storefront};
use decanter_epic::{AssetList, CatalogRegion, EpicGame, Library, Listing};
use decanter_gog::{GameKind, LibraryEntry};
use gpui::{AsyncApp, Context, WeakEntity};

use super::{GogLibrary, REFRESH_INTERVAL, merge};
use crate::state::cache::load_cached_games;

/// Where an Epic account's listing stands.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EpicPhase {
    /// A listing is running; `games` is what the last one, or the cache, holds.
    Listing,
    Listed,
    /// The entitlements were listed but the asset list did not answer within `waited_secs`: the
    /// installable titles they did not name are missing until a retry gets it.
    AssetsTimedOut {
        waited_secs: u64,
    },
    /// The asset list failed otherwise.
    AssetsFailed,
    SessionClosed,
    /// Neither the entitlements nor the asset list answered.
    Failed,
}

#[derive(Clone, Copy)]
pub struct EpicStatus {
    pub phase: EpicPhase,
    /// Games shown, DLC not counted.
    pub games: usize,
    /// Unix seconds of the last listing written to the cache.
    pub last_sync: Option<i64>,
}

/// What a listing reports, from the tokio worker to the UI thread.
enum Update {
    Listed {
        entries: Vec<LibraryEntry>,
        assets: AssetList,
        complete: bool,
        synced_at: Option<i64>,
    },
    SessionClosed,
    Failed,
}

impl GogLibrary {
    /// Starts a loop for each Epic account that has none and stops the ones whose account left
    /// the profile. The caller merges and notifies.
    pub(super) fn reconcile_epic(&mut self, cx: &Context<Self>) {
        let wanted: Vec<String> = self
            .profile
            .as_deref()
            .and_then(|profile_id| crate::core::db().ok().map(|db| (db, profile_id)))
            .map(|(db, profile_id)| {
                db.accounts_in_profile(profile_id)
                    .inspect_err(|err| {
                        tracing::warn!(profile_id, %err, "could not list accounts for profile, treating as none");
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|account| account.storefront == Storefront::EpicGames)
                    .map(|account| account.id)
                    .collect()
            })
            .unwrap_or_default();
        tracing::debug!(accounts = ?wanted, "resolved epic accounts for active profile");

        self.epic_tasks.retain(|account_id, _| wanted.contains(account_id));
        self.epic_by_account.retain(|account_id, _| wanted.contains(account_id));
        self.epic_status.retain(|account_id, _| wanted.contains(account_id));
        for account_id in &wanted {
            if self.epic_tasks.contains_key(account_id) {
                continue;
            }
            tracing::debug!(account_id, "starting epic library loop");
            let cached = load_cached_games(account_id);
            self.epic_status.insert(
                account_id.clone(),
                EpicStatus {
                    phase: EpicPhase::Listing,
                    games: shown_games(&cached),
                    last_sync: last_sync(account_id),
                },
            );
            self.epic_by_account.insert(account_id.clone(), cached);
            let task = cx.spawn({
                let account_id = account_id.clone();
                async move |this, cx| watch_epic(account_id, this, cx).await
            });
            self.epic_tasks.insert(account_id.clone(), task);
        }
        self.epic_wanted = wanted;
        self.epic = flatten(&self.epic_wanted, &self.epic_by_account);
    }

    /// Lists the account again now: after a new login, or when the user asks from Settings.
    pub fn refresh_epic(&mut self, account_id: &str, cx: &mut Context<Self>) {
        self.epic_tasks.remove(account_id);
        self.refresh(cx);
    }

    /// Drops one account's loop and games right away; a re-login under the same id starts afresh.
    pub fn forget_epic_account(&mut self, account_id: &str, cx: &mut Context<Self>) {
        self.epic_tasks.remove(account_id);
        self.epic_by_account.remove(account_id);
        self.epic_status.remove(account_id);
        self.epic_wanted.retain(|wanted| wanted != account_id);
        self.epic = flatten(&self.epic_wanted, &self.epic_by_account);
        self.remerge(cx);
    }

    /// [`GogLibrary::refresh_install_state`] for an Epic account's game; `false` when the account is
    /// not one of the profile's Epic accounts.
    pub(super) fn refresh_epic_install_state(&mut self, account_id: &str, product_id: &str) -> bool {
        let Some(account_games) = self.epic_by_account.get_mut(account_id) else {
            return false;
        };
        if let Some(entry) = account_games.iter_mut().find(|entry| entry.game.id == product_id) {
            entry.game.install_state = super::local_install_state(account_id, product_id);
        }
        self.epic = flatten(&self.epic_wanted, &self.epic_by_account);
        true
    }

    /// [`GogLibrary::refresh_all_install_state`] for the Epic accounts' games.
    pub(super) fn refresh_all_epic_install_state(&mut self) {
        for (account_id, games) in &mut self.epic_by_account {
            let library = crate::core::library_for_account(account_id).ok();
            for entry in games.iter_mut() {
                entry.game.install_state = super::local_install_state_in(library.as_ref(), account_id, &entry.game.id);
            }
        }
        self.epic = flatten(&self.epic_wanted, &self.epic_by_account);
    }

    pub fn epic_status(&self, account_id: &str) -> Option<EpicStatus> {
        self.epic_status.get(account_id).copied()
    }

    fn epic_listed(&mut self, account_id: &str, update: Update, cx: &mut Context<Self>) -> bool {
        let (Some(account_games), Some(status)) = (
            self.epic_by_account.get_mut(account_id),
            self.epic_status.get_mut(account_id),
        ) else {
            return false;
        };
        match update {
            Update::Listed {
                entries,
                assets,
                complete,
                synced_at,
            } => {
                if complete {
                    *account_games = entries;
                } else {
                    // A partial listing names only what it reached: the cached games it did not
                    // reach stay until a complete one says they are gone.
                    let listed: HashSet<&str> = entries.iter().map(|entry| entry.game.id.as_str()).collect();
                    account_games.retain(|entry| !listed.contains(entry.game.id.as_str()));
                    account_games.extend(entries);
                }
                status.games = shown_games(account_games);
                status.last_sync = synced_at.or(status.last_sync);
                status.phase = match assets {
                    AssetList::Pending => EpicPhase::Listing,
                    AssetList::TimedOut { waited } => EpicPhase::AssetsTimedOut {
                        waited_secs: waited.as_secs(),
                    },
                    AssetList::Failed(_) => EpicPhase::AssetsFailed,
                    // Listed, or a kind this build does not know, which says nothing went wrong.
                    _ => EpicPhase::Listed,
                };
            }
            Update::SessionClosed => status.phase = EpicPhase::SessionClosed,
            Update::Failed => status.phase = EpicPhase::Failed,
        }
        self.epic = flatten(&self.epic_wanted, &self.epic_by_account);
        self.remerge(cx);
        true
    }

    fn remerge(&mut self, cx: &mut Context<Self>) {
        self.games = merge(
            &self.by_account,
            &self.wanted,
            &self.amazon,
            &self.epic,
            &self.external,
            &self.mod_ids,
        );
        self.revision = self.revision.wrapping_add(1);
        cx.notify();
    }
}

fn shown_games(entries: &[LibraryEntry]) -> usize {
    entries.iter().filter(|entry| entry.parent_id.is_none()).count()
}

fn last_sync(account_id: &str) -> Option<i64> {
    Db::open_active()
        .and_then(|db| db.last_cached_at(account_id, Storefront::EpicGames))
        .inspect_err(|err| tracing::warn!(account_id, %err, "could not read the last epic listing time"))
        .ok()
        .flatten()
}

/// Every account's games in account order, keeping the first copy of a title two accounts own.
fn flatten(order: &[String], by_account: &HashMap<String, Vec<LibraryEntry>>) -> Vec<LibraryEntry> {
    let mut seen = HashSet::new();
    order
        .iter()
        .filter_map(|account_id| by_account.get(account_id))
        .flatten()
        .filter(|entry| seen.insert(entry.game.id.clone()))
        .cloned()
        .collect()
}

async fn watch_epic(account_id: String, this: WeakEntity<GogLibrary>, cx: &mut AsyncApp) {
    loop {
        let alive = this.update(cx, |library, cx| {
            if let Some(status) = library.epic_status.get_mut(&account_id) {
                status.phase = EpicPhase::Listing;
                cx.notify();
            }
        });
        if alive.is_err() {
            return;
        }
        let (updates, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        // A JoinSet so that this loop being dropped (a logout, a refresh) stops the listing too.
        let mut worker = tokio::task::JoinSet::new();
        worker.spawn(sync_account(account_id.clone(), updates));
        while let Some(update) = receiver.recv().await {
            let alive = this.update(cx, |library, cx| library.epic_listed(&account_id, update, cx));
            if !matches!(alive, Ok(true)) {
                return;
            }
        }
        if let Some(Err(err)) = worker.join_next().await {
            tracing::warn!(account_id, %err, "epic library task failed");
            let alive = this.update(cx, |library, cx| library.epic_listed(&account_id, Update::Failed, cx));
            if !matches!(alive, Ok(true)) {
                return;
            }
        }
        cx.background_executor().timer(REFRESH_INTERVAL).await;
    }
}

async fn sync_account(account_id: String, updates: tokio::sync::mpsc::UnboundedSender<Update>) {
    // The early listing is cached off the async threads like the final one, which waits for it so
    // that the two reach the screen in order.
    let mut early_write = None;
    let early = |listing: Listing| {
        let account = account_id.clone();
        let updates = updates.clone();
        early_write = Some(tokio::task::spawn_blocking(move || {
            let _closed = updates.send(cached(&account, listing));
        }));
    };
    let listed = decanter_epic::list_games(&account_id, region(), early).await;
    if let Some(write) = early_write.take()
        && let Err(err) = write.await
    {
        tracing::warn!(%err, "epic early cache task failed");
    }
    let update = match listed {
        Ok(Library::Listed(listing)) => {
            let account = account_id.clone();
            tokio::task::spawn_blocking(move || cached(&account, listing))
                .await
                .inspect_err(|err| tracing::warn!(%err, "epic cache task failed"))
                .unwrap_or(Update::Failed)
        }
        Ok(Library::Closed) => {
            tracing::info!(account_id, "epic session closed, keeping the cached library");
            Update::SessionClosed
        }
        Ok(Library::Unreachable(reason)) => {
            tracing::info!(
                account_id,
                reason,
                "epic library not listed, keeping the cached library"
            );
            Update::Failed
        }
        Err(err) => {
            tracing::warn!(account_id, error = %format_args!("{err:#}"), "epic library listing failed locally");
            Update::Failed
        }
        // `Library` is non-exhaustive: a kind this build does not know says nothing about the cache.
        Ok(_) => Update::Failed,
    };
    let _closed = updates.send(update);
}

/// The app's language and the system locale's country, for the catalog's titles.
fn region() -> CatalogRegion {
    let locale = rust_i18n::locale();
    let language = locale.split(['-', '_']).next().unwrap_or_default();
    let system = decanter_core::platform::system_locale().unwrap_or_default();
    let country = system
        .split(['.', '@'])
        .next()
        .and_then(|tag| tag.split(['-', '_']).nth(1))
        .unwrap_or_default();
    CatalogRegion::new(language, country)
}

fn cached(account_id: &str, listing: Listing) -> Update {
    let synced_at = cache(account_id, &listing.games, listing.complete);
    let library = crate::core::library_for_account(account_id).ok();
    Update::Listed {
        entries: listing
            .games
            .iter()
            .map(|game| entry(library.as_ref(), account_id, game))
            .collect(),
        assets: listing.assets,
        complete: listing.complete,
        synced_at,
    }
}

fn entry(library: Option<&decanter_core::Library>, account_id: &str, game: &EpicGame) -> LibraryEntry {
    LibraryEntry {
        game: Game {
            account_id: account_id.to_owned(),
            install_state: super::local_install_state_in(library, account_id, &game.app_name),
            id: game.app_name.clone(),
            title: game.title.clone(),
            storefront: Storefront::EpicGames,
            icon_url: game.icon_url.clone(),
            cover_url: game.cover_url.clone(),
            background_url: game.background_url.clone(),
            ..Default::default()
        },
        kind: GameKind::Game,
        purchased_at: None,
        library_rank: None,
        installable: true,
        parent_id: game.parent_app_name.clone(),
        is_mod: false,
    }
}

/// Best-effort, like the other stores' cache writes: the list on screen is right either way. A
/// listing that is not complete prunes nothing, since it says nothing about what is gone. The
/// time it was written, unless nothing was.
fn cache(account_id: &str, games: &[EpicGame], complete: bool) -> Option<i64> {
    let db = match Db::open_active() {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(account_id, %err, "could not open cache db to save epic games");
            return None;
        }
    };
    let confirmed_new = db.has_games(account_id).unwrap_or(true);
    let mut written = 0_usize;
    for game in games {
        let cached = CachedGame {
            storefront: Storefront::EpicGames,
            product_id: game.app_name.clone(),
            title: game.title.clone(),
            icon_url: game.icon_url.clone(),
            cover_url: game.cover_url.clone(),
            background_url: game.background_url.clone(),
            kind: "game".to_owned(),
            purchased_at: None,
            release_date: None,
            rating: None,
            genres: Vec::new(),
            library_rank: None,
            parent_id: game.parent_app_name.clone(),
            installable: true,
            has_macos_build: None,
            entitlement_id: None,
            product_sku: None,
            namespace: Some(game.namespace.clone()),
            catalog_item_id: Some(game.catalog_item_id.clone()),
            can_run_offline: game.launch.as_ref().map(|launch| launch.can_run_offline),
            ownership_token: game.launch.as_ref().map(|launch| launch.ownership_token),
            additional_command_line: game
                .launch
                .as_ref()
                .map(|launch| launch.additional_command_line.clone()),
        };
        match db.upsert_game(account_id, &cached, confirmed_new) {
            Ok(()) => written = written.saturating_add(1),
            Err(err) => tracing::warn!(account_id, product_id = %game.app_name, %err, "failed to cache epic game"),
        }
    }
    if complete {
        let keep: Vec<String> = games.iter().map(|game| game.app_name.clone()).collect();
        match db.retain_games(account_id, Storefront::EpicGames, &keep) {
            Ok(dropped) if dropped > 0 => {
                tracing::info!(account_id, dropped, "epic games no longer owned dropped from the cache");
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(account_id, %err, "failed to prune epic games from cache"),
        }
    }
    tracing::debug!(account_id, written, listed = games.len(), "cached the epic listing");
    (written > 0).then(|| chrono::Utc::now().timestamp())
}
