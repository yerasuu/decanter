//! Fetches one account's GOG library through `decanter-gog` and folds it
//! into that account's own `games` signal (one of these runs per account
//! in the active profile — see `super::use_gog_library`), mirroring each
//! group into the local cache as it lands.
//!
//! Two passes, for the reason `decanter_gog::library` documents: the owned
//! list takes seconds, the catalog metadata takes a minute. Both write
//! into `games` as they go regardless — the cache still gets each group as
//! soon as it lands, so the app has something to seed from on a later
//! restart. For an account with nothing cached yet, `super::use_gog_library`
//! leaves `state` at `Loading` until this first cycle has done *both*
//! passes (see the bottom of the loop below), and `Library` keeps the
//! whole view behind a loading screen until then instead of mounting it
//! against a list that's still filling in under it, the way Steam's own
//! "Syncing..." splash does. An account that already had a cache skips
//! straight to `Loaded` before this even starts — this loop's writes just
//! update what's already on screen.
//!
//! This replaced a `Library.WatchGames` stream. There's no server pushing
//! updates any more, so it re-fetches on an interval instead — the only
//! thing that changes between fetches is ownership (rare) and local
//! install state (which install and uninstall refresh themselves). A
//! later cycle never re-enters "loading": `state` only ever moves forward.

use std::time::Duration;

use decanter_gog::{Batch, LibraryEntry};
use dioxus::prelude::*;

use super::LibraryState;
use super::cache::{has_synced_before, save_entry};

/// Long enough that a full re-resolve never overlaps itself in practice.
const REFRESH_INTERVAL: Duration = Duration::from_mins(15);

/// `on_update` re-merges this account's list into `GogLibrary::games` —
/// called after every write to `games` below, same as when this owned a
/// shared signal directly, just routed through the caller's merge instead
/// of a plain `Signal::set`.
///
/// `progress` carries pass two's "N of M enriched" while it runs, and
/// `None` the rest of the time — pass one has no known total to show a
/// fraction of, and a later refresh cycle shouldn't leave a stale one on
/// screen after this account is already `Loaded`.
pub(super) async fn watch_games(
    account_id: String,
    mut games: Signal<Vec<LibraryEntry>>,
    mut state: Signal<LibraryState>,
    mut progress: Signal<Option<(usize, usize)>>,
    mut on_update: impl FnMut() + 'static,
) {
    loop {
        // Checked once per cycle, before any of this round's writes land:
        // a first-ever sync means every row is about to be a fresh INSERT,
        // and none of them carries evidence of having just been added.
        let confirmed_new = has_synced_before(&account_id);

        // Pass one merges into whatever is already on screen rather than
        // starting from empty. Starting empty meant a full cache was
        // wiped on every launch and rebuilt over the next ten seconds, so
        // a library that could have been instant flickered to nothing
        // first. Ownership can still drop — a refund, a revoked key — but
        // that's settled by pruning once the fetch is whole, below.
        let mut shown: Vec<LibraryEntry> = games.peek().clone();
        tracing::debug!(account_id, "listing gog games");
        // Resolved fresh each cycle, not once outside the loop: a library
        // moved to a new location (see Ajustes) should be picked up by the
        // very next refresh, not only after this whole watch restarts.
        let listed = match crate::core::library_for_account(&account_id) {
            Ok(library) => {
                decanter_gog::list_games(&library, &account_id, &mut |batch| {
                    absorb(&account_id, confirmed_new, &mut shown, batch);
                    games.set(shown.clone());
                    on_update();
                })
                .await
            }
            Err(err) => Err(anyhow::anyhow!(err)),
        };
        let listed_ok = listed.is_ok();

        let collected = match listed {
            Ok(collected) => {
                tracing::info!(account_id, count = collected.len(), "gog library listed");
                // A complete listing is the whole truth about what's
                // owned, so anything it didn't mention isn't any more.
                prune(&account_id, &mut shown, &collected);
                games.set(shown.clone());
                on_update();
                collected
            }
            Err(err) => {
                // Whatever the cache seeded stays, unpruned: a failed
                // refresh shouldn't blank a library the user could still
                // browse, and a partial listing is no evidence of a
                // dropped entitlement.
                tracing::warn!(account_id, %err, "gog library listing failed, keeping what was cached");
                state.set(LibraryState::Failed(err.to_string()));
                Vec::new()
            }
        };

        // Pass two updates entries in place. Everything it does is
        // best-effort inside, so there's nothing to handle out here — an
        // entry that gains nothing stays exactly as pass one left it.
        if !collected.is_empty() {
            tracing::debug!(account_id, count = collected.len(), "enriching gog catalog metadata");
            decanter_gog::enrich_games(
                &account_id,
                collected,
                &mut |batch| {
                    absorb(&account_id, confirmed_new, &mut shown, batch);
                    games.set(shown.clone());
                    on_update();
                },
                &mut |done, total| progress.set(Some((done, total))),
            )
            .await;
            progress.set(None);
            tracing::debug!(account_id, "gog catalog enrichment finished");
        }

        // Only reached once both passes are done. `state` starts this
        // session as `Loading` (see `use_gog_library`) and this is the
        // one place that ever moves it to `Loaded` — after that it's
        // already `Loaded`, so re-setting it here on every later cycle is
        // a harmless no-op, not a re-triggered loading screen.
        if listed_ok {
            tracing::info!(account_id, "gog library ready");
            state.set(LibraryState::Loaded);
        }

        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

/// Drops whatever a complete fetch didn't list, on screen and in the
/// cache alike — the only thing that ever notices an entitlement going
/// away, since a dropped id simply stops coming back.
fn prune(account_id: &str, shown: &mut Vec<LibraryEntry>, fetched: &[LibraryEntry]) {
    let owned: std::collections::HashSet<&str> = fetched.iter().map(|entry| entry.game.id.as_str()).collect();
    shown.retain(|entry| owned.contains(entry.game.id.as_str()));

    let keep: Vec<String> = fetched.iter().map(|entry| entry.game.id.clone()).collect();
    match decanter_core::Db::open(decanter_core::default_db_path()) {
        Ok(db) => match db.retain_games(account_id, decanter_core::Storefront::Gog, &keep) {
            Ok(dropped) if dropped > 0 => tracing::info!("{dropped} game(s) are no longer in this GOG account"),
            Ok(_) => {}
            Err(err) => tracing::warn!(account_id, %err, "failed to prune games no longer owned from cache"),
        },
        Err(err) => tracing::warn!(account_id, %err, "failed to open db while pruning dropped games"),
    }
}

/// Caches a group and folds it into what's on screen.
///
/// Position is kept for an entry that's already there, so pass two filling
/// in a cover doesn't reshuffle the list under someone mid-scroll. Sorting
/// is the UI's job, not the fetch's.
fn absorb(account_id: &str, confirmed_new: bool, shown: &mut Vec<LibraryEntry>, batch: Batch) {
    for mut entry in batch {
        // Tagged here rather than by decanter_gog itself: this is the one place
        // that already knows both the entry and which account's stream it
        // came from, and it's what the merged, multi-account library view
        // needs to tell two accounts' copies of the same game apart.
        entry.game.account_id = account_id.to_owned();
        save_entry(account_id, &entry, confirmed_new);
        match shown.iter_mut().find(|existing| existing.game.id == entry.game.id) {
            Some(existing) => *existing = entry,
            None => shown.push(entry),
        }
    }
}
