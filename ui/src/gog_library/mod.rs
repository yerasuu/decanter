//! Keeps one `Library.WatchGames`-replacement stream alive per GOG account
//! in the active profile, owned at the app root instead of by the
//! `Library` view — so leaving and coming back to Biblioteca doesn't throw
//! the fetched library away, and switching profiles swaps every account's
//! stream at once instead of the view having to notice each one.
//!
//! Still seeds from the local cache on startup (see `stream`'s own doc for
//! why), but doesn't trust it to paint the screen on its own any more:
//! every activation starts `Loading`, and `Library` shows a loading
//! screen instead of the real one until this session's first full sync
//! actually lands — Steam's own shape, not the previous "flash the old
//! cache, then silently rewrite it underneath you" one.

use std::collections::HashMap;

use decanter_core::InstallState;
use decanter_gog::LibraryEntry;
use dioxus::core::Task;
use dioxus::prelude::*;

mod cache;
mod stream;

/// Whether this game is installed, from this account's appmanifest. The
/// one part of a `Game` that isn't catalog data, and the only part that
/// changes without a trip to GOG.
pub(crate) fn local_install_state(account_id: &str, product_id: &str) -> Option<InstallState> {
    let Some(installed) = crate::core::installed_game(account_id, product_id) else {
        return None;
    };
    Some(InstallState {
        installed: true,
        installed_version: Some(installed.build_id),
        install_size_bytes: installed.size_on_disk,
        ..Default::default()
    })
}

#[derive(Clone, PartialEq, Eq)]
pub enum LibraryState {
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

// `PartialEq` so it can be passed as a component prop; the signals it
// holds are handles, so comparing them compares identity, not contents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GogLibrary {
    /// Every account's games, merged and deduped — see [`merge`]. What the
    /// Biblioteca view reads.
    pub games: Signal<Vec<LibraryEntry>>,
    pub state: Signal<LibraryState>,
    pub generation: Signal<u32>,
    /// "N of M enriched" while pass two runs, `None` otherwise — a rough
    /// progress hint for [`LibraryState::Loading`], not tracked per account
    /// the way `by_account` is. With more than one GOG account loading at
    /// once this just reflects whichever wrote most recently, the same
    /// simplification `state` itself already makes (one shared flag, not
    /// one per account) for "done or not".
    pub enrich_progress: Signal<Option<(usize, usize)>>,
    /// One raw, unmerged list per account — the merge input, and what
    /// [`GogLibrary::refresh_install_state`] patches before re-merging.
    by_account: Signal<HashMap<String, Signal<Vec<LibraryEntry>>>>,
    /// One fetch task per account — what [`GogLibrary::forget_account`]
    /// cancels immediately, and what [`use_gog_library`]'s own effect
    /// otherwise reconciles against `wanted` on its own schedule.
    tasks: Signal<HashMap<String, Task>>,
}

impl GogLibrary {
    /// Restarts every account's fetch immediately instead of waiting out
    /// the refresh interval — for when something we did (signing in or
    /// out) means the library just changed.
    pub fn refresh(mut self) {
        let next = self.generation.peek().wrapping_add(1);
        tracing::debug!(generation = next, "gog library refresh requested");
        self.generation.set(next);
    }

    /// Re-reads one game's local install state after an install or
    /// uninstall.
    ///
    /// Not a full [`refresh`](Self::refresh): that re-resolves every owned
    /// game's catalog data over the network and takes tens of seconds,
    /// while the only thing that changed is a local record. A server used
    /// to push this to us over `WatchGames`; nothing does now, so without
    /// this the game keeps showing its pre-install state until the next
    /// scheduled fetch.
    pub fn refresh_install_state(mut self, account_id: &str, product_id: &str) {
        let install_state = local_install_state(account_id, product_id);
        tracing::debug!(
            account_id,
            product_id,
            installed = install_state.is_some(),
            "refreshing local install state for one game"
        );

        let Some(mut account_games) = self.by_account.read().get(account_id).copied() else {
            tracing::warn!(
                account_id,
                product_id,
                "no active fetch stream for this account, install state not refreshed"
            );
            return;
        };
        if let Some(entry) = account_games
            .write()
            .iter_mut()
            .find(|entry| entry.game.id == product_id)
        {
            entry.game.install_state = install_state;
        }
        self.games.set(merge(&self.by_account.read(), &[account_id.to_owned()]));
    }

    /// Cancels this account's fetch task and drops its games right away —
    /// call this from a logout, don't wait for [`refresh`](Self::refresh)'s
    /// own reconciliation to notice.
    ///
    /// A logged-out account keeps the same id if it's ever logged back in
    /// (GOG hands back the same `user_id`), so without this a task left
    /// running past logout survives a same-session re-login too: the
    /// reconciliation in [`use_gog_library`]'s effect only cancels a task
    /// whose account has dropped out of `wanted`, and by the time it runs
    /// again after a fresh login the account is back in `wanted` — so the
    /// stale task reads as "still correctly running" and is left alone,
    /// quietly serving whatever session it had before logout instead of
    /// the one just created. This is what actually closes that window.
    pub fn forget_account(mut self, account_id: &str) {
        if let Some(task) = self.tasks.write().remove(account_id) {
            tracing::debug!(account_id, "cancelling gog library fetch task on logout");
            task.cancel();
        }
        self.by_account.write().remove(account_id);
        // Order doesn't matter for dropping one account's entries out of an
        // already-merged list — the next fetch cycle re-merges properly
        // ordered anyway. This just needs the remaining accounts included.
        let remaining: Vec<String> = self.by_account.read().keys().cloned().collect();
        self.games.set(merge(&self.by_account.read(), &remaining));
    }
}

/// Concatenates every account's games into one list, keeping the first
/// copy of a product a given account owns — two accounts with the same
/// game would otherwise show it twice. `order` decides which account's
/// copy wins a duplicate (oldest account first, same order
/// `decanter_core::Db::accounts_in_profile` already returns) and, since it's
/// also the merge's only iteration order, which accounts are actually
/// included: an account with a live signal but missing from `order` is
/// skipped, the shape `refresh_install_state` relies on to patch just one
/// account's entries in as a cheap re-merge.
fn merge(by_account: &HashMap<String, Signal<Vec<LibraryEntry>>>, order: &[String]) -> Vec<LibraryEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for account_id in order {
        let Some(account_games) = by_account.get(account_id) else {
            continue;
        };
        for entry in account_games.read().iter() {
            if seen.insert((entry.game.storefront, entry.game.id.clone())) {
                merged.push(entry.clone());
            }
        }
    }
    merged
}

/// Call once, at the app root. Restarts an account's stream only when it
/// enters or leaves the active profile's account list — not on every
/// route/component mount, and not for accounts that were already running.
/// Switching `active_profile` swaps the whole set of running streams at
/// once, since the accounts it resolves to change all together too.
pub fn use_gog_library(active_profile: Signal<Option<String>>) -> GogLibrary {
    let mut by_account = use_signal(HashMap::<String, Signal<Vec<LibraryEntry>>>::new);
    let mut tasks = use_signal(HashMap::<String, Task>::new);
    let mut games = use_signal(Vec::<LibraryEntry>::new);
    let mut state = use_signal(|| LibraryState::Idle);
    let generation = use_signal(|| 0_u32);
    let mut enrich_progress = use_signal(|| None::<(usize, usize)>);

    use_effect(move || {
        let profile_id = active_profile.read().clone();
        // Subscribes this effect to `refresh()` as well as the profile.
        let _ = generation();

        let wanted: Vec<String> = profile_id
            .and_then(|profile_id| crate::core::db().ok().map(|db| (db, profile_id)))
            .map(|(db, profile_id)| {
                db.accounts_in_profile(&profile_id)
                    .inspect_err(|err| {
                        tracing::warn!(profile_id, %err, "could not list accounts for profile, treating as none");
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|account| account.storefront == decanter_core::Storefront::Gog)
                    .map(|account| account.id)
                    .collect()
            })
            .unwrap_or_default();
        tracing::debug!(accounts = ?wanted, "resolved gog accounts for active profile");

        // Stops and drops whatever isn't wanted any more — a logged-out
        // account, or one that moved to another profile.
        tasks.write().retain(|account_id, task| {
            let keep = wanted.contains(account_id);
            if !keep {
                tracing::debug!(
                    account_id,
                    "cancelling gog library fetch task, account no longer wanted"
                );
                task.cancel();
            }
            keep
        });
        by_account.write().retain(|account_id, _| wanted.contains(account_id));

        if wanted.is_empty() {
            games.write().clear();
            state.set(LibraryState::Idle);
            enrich_progress.set(None);
            return;
        }
        state.set(LibraryState::Loading);

        for account_id in &wanted {
            if tasks.read().contains_key(account_id) {
                continue;
            }
            // Seeded from cache, but still `Loading` regardless of whether
            // that seed is empty — a real second (or reopened) session
            // shouldn't have to wait, but flashing a maybe-stale,
            // maybe-incomplete list and then visibly rewriting it as the
            // fetch lands is exactly the flicker this was rewritten to
            // avoid. `stream::watch_games` moves `state` to `Loaded` only
            // once every started account's first full fetch (list +
            // catalog enrichment) is actually done.
            tracing::debug!(account_id, "starting gog library fetch task");
            let cached = cache::load_cached_games(account_id);
            let account_games = Signal::new(cached);
            by_account.write().insert(account_id.clone(), account_games);
            games.set(merge(&by_account.read(), &wanted));

            let task = spawn({
                let account_id = account_id.clone();
                let wanted = wanted.clone();
                async move {
                    stream::watch_games(account_id.clone(), account_games, state, enrich_progress, move || {
                        games.set(merge(&by_account.read(), &wanted));
                    })
                    .await;
                }
            });
            tasks.write().insert(account_id.clone(), task);
        }
    });

    GogLibrary {
        games,
        state,
        generation,
        enrich_progress,
        by_account,
        tasks,
    }
}
