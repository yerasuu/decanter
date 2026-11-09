//! The Amazon Games half of the library (kb #1521): one listing loop per Amazon account of the
//! profile, started with the account list and repeated on the GOG loops' interval.
//!
//! Each game shows whether it is installed from its install record, and the entitlement id its
//! download needs is kept in the cache beside it.

use std::collections::HashSet;

use decanter_amazon::{AmazonGame, Library, Listing};
use decanter_core::{CachedGame, Db, Game, Storefront};
use decanter_gog::{GameKind, LibraryEntry};
use gpui::{AsyncApp, Context, WeakEntity};

use super::{GogLibrary, REFRESH_INTERVAL, merge};
use crate::state::cache::load_cached_games;

/// What one listing cycle ends in, reported from the tokio worker to the UI thread.
enum Cycle {
    Listed(Vec<LibraryEntry>),
    /// Amazon closed the session, or none is stored: the cache stays as it was until a new login.
    SessionClosed,
    /// Amazon could not be asked or answered unusably: the cache stays as it was.
    Failed,
}

impl GogLibrary {
    /// Starts a loop for each Amazon account that has none and stops the ones whose account left
    /// the profile. The caller merges and notifies.
    pub(super) fn reconcile_amazon(&mut self, cx: &Context<Self>) {
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
                    .filter(|account| account.storefront == Storefront::AmazonGames)
                    .map(|account| account.id)
                    .collect()
            })
            .unwrap_or_default();
        tracing::debug!(accounts = ?wanted, "resolved amazon accounts for active profile");

        self.amazon_tasks.retain(|account_id, _| wanted.contains(account_id));
        self.amazon_by_account
            .retain(|account_id, _| wanted.contains(account_id));
        for account_id in &wanted {
            if self.amazon_tasks.contains_key(account_id) {
                continue;
            }
            tracing::debug!(account_id, "starting amazon library loop");
            self.amazon_by_account
                .insert(account_id.clone(), load_cached_games(account_id));
            let task = cx.spawn({
                let account_id = account_id.clone();
                async move |this, cx| watch_amazon(account_id, this, cx).await
            });
            self.amazon_tasks.insert(account_id.clone(), task);
        }
        self.amazon_wanted = wanted;
        self.amazon = flatten(&self.amazon_wanted, &self.amazon_by_account);
    }

    /// A new login for an account whose loop may be idling out its interval after a closed
    /// session: it starts over at once.
    pub fn amazon_logged_in(&mut self, account_id: &str, cx: &mut Context<Self>) {
        self.amazon_tasks.remove(account_id);
        self.refresh(cx);
    }

    /// Drops one account's loop and games right away; a re-login under the same id starts afresh.
    pub fn forget_amazon_account(&mut self, account_id: &str, cx: &mut Context<Self>) {
        self.amazon_tasks.remove(account_id);
        self.amazon_by_account.remove(account_id);
        self.amazon_wanted.retain(|wanted| wanted != account_id);
        self.amazon = flatten(&self.amazon_wanted, &self.amazon_by_account);
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

    /// [`GogLibrary::refresh_install_state`] for an Amazon account's game; `false` when the account
    /// is not one of the profile's Amazon accounts.
    pub(super) fn refresh_amazon_install_state(&mut self, account_id: &str, product_id: &str) -> bool {
        let Some(account_games) = self.amazon_by_account.get_mut(account_id) else {
            return false;
        };
        if let Some(entry) = account_games.iter_mut().find(|entry| entry.game.id == product_id) {
            entry.game.install_state = super::local_install_state(account_id, product_id);
        }
        self.amazon = flatten(&self.amazon_wanted, &self.amazon_by_account);
        true
    }

    /// [`GogLibrary::refresh_all_install_state`] for the Amazon accounts' games.
    pub(super) fn refresh_all_amazon_install_state(&mut self) {
        for (account_id, games) in &mut self.amazon_by_account {
            let library = crate::core::library_for_account(account_id).ok();
            for entry in games.iter_mut() {
                entry.game.install_state = super::local_install_state_in(library.as_ref(), account_id, &entry.game.id);
            }
        }
        self.amazon = flatten(&self.amazon_wanted, &self.amazon_by_account);
    }

    fn amazon_listed(&mut self, account_id: &str, cycle: Cycle, cx: &mut Context<Self>) -> bool {
        let Some(account_games) = self.amazon_by_account.get_mut(account_id) else {
            return false;
        };
        match cycle {
            Cycle::Listed(listed) => *account_games = listed,
            Cycle::SessionClosed | Cycle::Failed => return true,
        }
        self.amazon = flatten(&self.amazon_wanted, &self.amazon_by_account);
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
        true
    }
}

/// Every account's games in account order, keeping the first copy of a product two accounts own.
fn flatten(order: &[String], by_account: &std::collections::HashMap<String, Vec<LibraryEntry>>) -> Vec<LibraryEntry> {
    let mut seen = HashSet::new();
    order
        .iter()
        .filter_map(|account_id| by_account.get(account_id))
        .flatten()
        .filter(|entry| seen.insert(entry.game.id.clone()))
        .cloned()
        .collect()
}

async fn watch_amazon(account_id: String, this: WeakEntity<GogLibrary>, cx: &mut AsyncApp) {
    loop {
        let cycle = tokio::spawn(sync_account(account_id.clone()))
            .await
            .inspect_err(|err| tracing::warn!(account_id, %err, "amazon library task failed"))
            .unwrap_or(Cycle::Failed);
        let alive = this.update(cx, |library, cx| library.amazon_listed(&account_id, cycle, cx));
        if !matches!(alive, Ok(true)) {
            return;
        }
        cx.background_executor().timer(REFRESH_INTERVAL).await;
    }
}

async fn sync_account(account_id: String) -> Cycle {
    match decanter_amazon::list_games(&account_id).await {
        Ok(Library::Listed(listing)) => cached(account_id, listing).await,
        Ok(Library::LoggedOut | Library::Refused) => {
            tracing::info!(account_id, "amazon session closed, keeping the cached library");
            Cycle::SessionClosed
        }
        Ok(Library::Unreachable(reason)) => {
            tracing::info!(
                account_id,
                reason,
                "amazon library not listed, keeping the cached library"
            );
            Cycle::Failed
        }
        Err(err) => {
            tracing::warn!(account_id, error = %format_args!("{err:#}"), "amazon library listing failed locally");
            Cycle::Failed
        }
        // `Library` is non-exhaustive: a kind this build does not know says nothing about the cache.
        Ok(_) => Cycle::Failed,
    }
}

async fn cached(account_id: String, listing: Listing) -> Cycle {
    tokio::task::spawn_blocking(move || {
        let library = crate::core::library_for_account(&account_id).ok();
        let entries = listing
            .games
            .iter()
            .map(|game| entry(library.as_ref(), &account_id, game))
            .collect();
        cache(&account_id, &listing.games, listing.complete);
        entries
    })
    .await
    .inspect_err(|err| tracing::warn!(%err, "amazon cache task failed"))
    .map_or(Cycle::Failed, Cycle::Listed)
}

fn entry(library: Option<&decanter_core::Library>, account_id: &str, game: &AmazonGame) -> LibraryEntry {
    LibraryEntry {
        game: Game {
            install_state: super::local_install_state_in(library, account_id, &game.product_id),
            account_id: account_id.to_owned(),
            id: game.product_id.clone(),
            title: game.title.clone(),
            storefront: Storefront::AmazonGames,
            genres: game.genres.clone(),
            icon_url: game.icon_url.clone(),
            cover_url: game.cover_url.clone(),
            background_url: game.background_url.clone(),
            ..Default::default()
        },
        kind: GameKind::Game,
        purchased_at: None,
        library_rank: None,
        installable: true,
        parent_id: None,
        is_mod: false,
    }
}

/// Best-effort, like the GOG cache writes: the list on screen is right either way. A listing that
/// did not reach its last page prunes nothing, since it says nothing about what is gone.
fn cache(account_id: &str, games: &[AmazonGame], complete: bool) {
    let db = match Db::open_active() {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(account_id, %err, "could not open cache db to save amazon games");
            return;
        }
    };
    let confirmed_new = db.has_games(account_id).unwrap_or(true);
    for game in games {
        let cached = CachedGame {
            storefront: Storefront::AmazonGames,
            product_id: game.product_id.clone(),
            title: game.title.clone(),
            icon_url: game.icon_url.clone(),
            cover_url: game.cover_url.clone(),
            background_url: game.background_url.clone(),
            kind: "game".to_owned(),
            purchased_at: None,
            release_date: None,
            rating: None,
            genres: game.genres.clone(),
            library_rank: None,
            parent_id: None,
            installable: true,
            has_macos_build: None,
            entitlement_id: Some(game.entitlement_id.clone()),
            product_sku: game.sku.clone(),
            namespace: None,
            catalog_item_id: None,
            can_run_offline: None,
            ownership_token: None,
            additional_command_line: None,
        };
        if let Err(err) = db.upsert_game(account_id, &cached, confirmed_new) {
            tracing::warn!(account_id, product_id = %game.product_id, %err, "failed to cache amazon game");
        }
    }
    if complete {
        prune(&db, account_id, games);
    }
}

fn prune(db: &Db, account_id: &str, games: &[AmazonGame]) {
    let keep: Vec<String> = games.iter().map(|game| game.product_id.clone()).collect();
    match db.retain_games(account_id, Storefront::AmazonGames, &keep) {
        Ok(dropped) if dropped > 0 => {
            tracing::info!(
                account_id,
                dropped,
                "amazon games no longer owned dropped from the cache"
            );
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(account_id, %err, "failed to prune amazon games from cache"),
    }
}
