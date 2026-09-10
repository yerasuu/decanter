//! Fetches the profile's GOG library through `achv-gog` and folds it
//! into the shared `games` signal, mirroring each group into the local
//! cache as it lands.
//!
//! Two passes, for the reason `achv_gog::library` documents: the owned
//! list takes seconds, the catalog metadata takes a minute. Both write
//! into `games` as they go regardless — the cache still gets each group as
//! soon as it lands, so the app has something to seed from on a later
//! restart — but `state` only reaches `Loaded` once this session's own
//! first cycle has done *both* passes (see the bottom of the loop below).
//! Until then `Library` keeps the whole view behind a loading screen
//! instead of mounting it against a list that's still filling in under
//! it, the way Steam's own "Syncing..." splash does.
//!
//! This replaced a `Library.WatchGames` stream. There's no server pushing
//! updates any more, so it re-fetches on an interval instead — the only
//! thing that changes between fetches is ownership (rare) and local
//! install state (which install and uninstall refresh themselves). A
//! later cycle never re-enters "loading": `state` only ever moves forward.

use std::time::Duration;

use achv_gog::{Batch, LibraryEntry};
use dioxus::prelude::*;

use super::LibraryState;
use super::cache::{has_synced_before, save_entry};

/// Long enough that a full re-resolve never overlaps itself in practice.
const REFRESH_INTERVAL: Duration = Duration::from_mins(15);

pub(super) async fn watch_games(
    profile_id: String,
    mut games: Signal<Vec<LibraryEntry>>,
    mut state: Signal<LibraryState>,
) {
    loop {
        // Checked once per cycle, before any of this round's writes land:
        // a first-ever sync means every row is about to be a fresh INSERT,
        // and none of them carries evidence of having just been added.
        let confirmed_new = has_synced_before(&profile_id);

        // Pass one merges into whatever is already on screen rather than
        // starting from empty. Starting empty meant a full cache was
        // wiped on every launch and rebuilt over the next ten seconds, so
        // a library that could have been instant flickered to nothing
        // first. Ownership can still drop — a refund, a revoked key — but
        // that's settled by pruning once the fetch is whole, below.
        let mut shown: Vec<LibraryEntry> = games.peek().clone();
        let listed = achv_gog::list_games(&profile_id, &mut |batch| {
            absorb(&profile_id, confirmed_new, &mut shown, batch);
            games.set(shown.clone());
        })
        .await;
        let listed_ok = listed.is_ok();

        let collected = match listed {
            Ok(collected) => {
                // A complete listing is the whole truth about what's
                // owned, so anything it didn't mention isn't any more.
                prune(&profile_id, &mut shown, &collected);
                games.set(shown.clone());
                collected
            }
            Err(err) => {
                // Whatever the cache seeded stays, unpruned: a failed
                // refresh shouldn't blank a library the user could still
                // browse, and a partial listing is no evidence of a
                // dropped entitlement.
                state.set(LibraryState::Failed(err.to_string()));
                Vec::new()
            }
        };

        // Pass two updates entries in place. Everything it does is
        // best-effort inside, so there's nothing to handle out here — an
        // entry that gains nothing stays exactly as pass one left it.
        if !collected.is_empty() {
            achv_gog::enrich_games(&profile_id, collected, &mut |batch| {
                absorb(&profile_id, confirmed_new, &mut shown, batch);
                games.set(shown.clone());
            })
            .await;
        }

        // Only reached once both passes are done. `state` starts this
        // session as `Loading` (see `use_gog_library`) and this is the
        // one place that ever moves it to `Loaded` — after that it's
        // already `Loaded`, so re-setting it here on every later cycle is
        // a harmless no-op, not a re-triggered loading screen.
        if listed_ok {
            state.set(LibraryState::Loaded);
        }

        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

/// Drops whatever a complete fetch didn't list, on screen and in the
/// cache alike — the only thing that ever notices an entitlement going
/// away, since a dropped id simply stops coming back.
fn prune(profile_id: &str, shown: &mut Vec<LibraryEntry>, fetched: &[LibraryEntry]) {
    let owned: std::collections::HashSet<&str> = fetched.iter().map(|entry| entry.game.id.as_str()).collect();
    shown.retain(|entry| owned.contains(entry.game.id.as_str()));

    let keep: Vec<String> = fetched.iter().map(|entry| entry.game.id.clone()).collect();
    if let Ok(db) = achv_core::Db::open(achv_core::default_db_path())
        && let Ok(dropped) = db.retain_games(profile_id, achv_core::Storefront::Gog, &keep)
        && dropped > 0
    {
        tracing::info!("{dropped} game(s) are no longer in this GOG account");
    }
}

/// Caches a group and folds it into what's on screen.
///
/// Position is kept for an entry that's already there, so pass two filling
/// in a cover doesn't reshuffle the list under someone mid-scroll. Sorting
/// is the UI's job, not the fetch's.
fn absorb(profile_id: &str, confirmed_new: bool, shown: &mut Vec<LibraryEntry>, batch: Batch) {
    for entry in batch {
        save_entry(profile_id, &entry, confirmed_new);
        match shown.iter_mut().find(|existing| existing.game.id == entry.game.id) {
            Some(existing) => *existing = entry,
            None => shown.push(entry),
        }
    }
}
