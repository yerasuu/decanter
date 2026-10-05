//! Keeps one library fetch loop alive per GOG account in the active
//! profile, owned by the app instead of by the library view — so leaving
//! and coming back to Biblioteca doesn't throw the fetched library away,
//! and switching profiles swaps every account's loop at once.
//!
//! Seeds from the local cache on startup and paints it straight away when
//! every wanted account has synced before — a GOG account decanter already
//! knows shouldn't have to wait on a network round trip just to open. The
//! library view only falls back to a loading screen for an account with
//! nothing cached yet (a fresh login), since there truly is nothing else
//! to show. Either way the refresh that follows folds in over what's
//! already on screen rather than wiping it first.
//!
//! Each loop makes two passes, for the reason `decanter_gog::library`
//! documents: the owned list takes seconds, the catalog metadata takes a
//! minute. There's no server pushing updates, so it re-fetches on an
//! interval — the only thing that changes between fetches is ownership
//! (rare) and local install state (which install and uninstall refresh
//! themselves). A later cycle never re-enters "loading": `state` only ever
//! moves forward.

use std::collections::HashMap;
use std::time::Duration;

use decanter_core::InstallState;
use decanter_gog::{Batch, LibraryEntry};
use gpui::{AsyncApp, Context, Entity, Task, WeakEntity};

use super::Session;
use super::cache::{has_synced_before, load_cached_games, save_entry};

/// Long enough that a full re-resolve never overlaps itself in practice.
const REFRESH_INTERVAL: Duration = Duration::from_mins(15);

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

pub struct GogLibrary {
    /// Every account's games, merged and deduped — see [`merge`]. What the
    /// Biblioteca view reads.
    pub games: Vec<LibraryEntry>,
    pub state: LibraryState,
    /// "N of M enriched" while pass two runs, `None` otherwise — a rough
    /// progress hint for [`LibraryState::Loading`], not tracked per
    /// account. With more than one GOG account loading at once this just
    /// reflects whichever wrote most recently, the same simplification
    /// `state` itself already makes.
    pub enrich_progress: Option<(usize, usize)>,
    profile: Option<String>,
    /// The active profile's GOG accounts, oldest first — the merge order.
    wanted: Vec<String>,
    /// One raw, unmerged list per account — the merge input, and what
    /// [`GogLibrary::refresh_install_state`] patches before re-merging.
    by_account: HashMap<String, Vec<LibraryEntry>>,
    /// One fetch loop per account. Dropping a task cancels it.
    tasks: HashMap<String, Task<()>>,
}

impl GogLibrary {
    pub fn new(session: &Entity<Session>, cx: &mut Context<Self>) -> Self {
        cx.observe(session, |this, session, cx| {
            let profile = session.read(cx).active_profile.clone();
            if profile != this.profile {
                this.profile = profile;
                this.reconcile(cx);
            }
        })
        .detach();
        let mut this = Self {
            games: Vec::new(),
            state: LibraryState::Idle,
            enrich_progress: None,
            profile: session.read(cx).active_profile.clone(),
            wanted: Vec::new(),
            by_account: HashMap::new(),
            tasks: HashMap::new(),
        };
        this.reconcile(cx);
        this
    }

    /// Re-resolves the profile's accounts and starts a loop for any that
    /// doesn't have one yet — for when something we did (signing in or
    /// out) means the set of accounts just changed.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        tracing::debug!("gog library refresh requested");
        self.reconcile(cx);
    }

    /// Re-reads one game's local install state after an install or
    /// uninstall.
    ///
    /// Not a full [`refresh`](Self::refresh): that re-resolves every owned
    /// game's catalog data over the network and takes tens of seconds,
    /// while the only thing that changed is a local record. Without this
    /// the game keeps showing its pre-install state until the next
    /// scheduled fetch.
    pub fn refresh_install_state(&mut self, account_id: &str, product_id: &str, cx: &mut Context<Self>) {
        let install_state = local_install_state(account_id, product_id);
        tracing::debug!(
            account_id,
            product_id,
            installed = install_state.is_some(),
            "refreshing local install state for one game"
        );
        let Some(account_games) = self.by_account.get_mut(account_id) else {
            tracing::warn!(
                account_id,
                product_id,
                "no active fetch loop for this account, install state not refreshed"
            );
            return;
        };
        if let Some(entry) = account_games.iter_mut().find(|entry| entry.game.id == product_id) {
            entry.game.install_state = install_state;
        }
        self.games = merge(&self.by_account, &self.wanted);
        cx.notify();
    }

    /// Cancels this account's fetch loop and drops its games right away —
    /// call this from a logout, don't wait for the next reconcile.
    ///
    /// A logged-out account keeps the same id if it's ever logged back in
    /// (GOG hands back the same `user_id`), so a loop left running past
    /// logout would survive a same-session re-login too: reconciling only
    /// cancels a loop whose account dropped out of `wanted`, and after a
    /// fresh login the account is back in `wanted`, so the stale loop reads
    /// as "still correctly running" and keeps serving the old session.
    pub fn forget_account(&mut self, account_id: &str, cx: &mut Context<Self>) {
        if self.tasks.remove(account_id).is_some() {
            tracing::debug!(account_id, "cancelling gog library fetch loop on logout");
        }
        self.by_account.remove(account_id);
        self.wanted.retain(|wanted| wanted != account_id);
        self.games = merge(&self.by_account, &self.wanted);
        cx.notify();
    }

    fn reconcile(&mut self, cx: &mut Context<Self>) {
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
                    .filter(|account| account.storefront == decanter_core::Storefront::Gog)
                    .map(|account| account.id)
                    .collect()
            })
            .unwrap_or_default();
        tracing::debug!(accounts = ?wanted, "resolved gog accounts for active profile");

        // Stops and drops whatever isn't wanted any more — a logged-out
        // account, or one that moved to another profile.
        self.tasks.retain(|account_id, _| {
            let keep = wanted.contains(account_id);
            if !keep {
                tracing::debug!(
                    account_id,
                    "cancelling gog library fetch loop, account no longer wanted"
                );
            }
            keep
        });
        self.by_account.retain(|account_id, _| wanted.contains(account_id));
        self.wanted = wanted;

        if self.wanted.is_empty() {
            self.games.clear();
            self.state = LibraryState::Idle;
            self.enrich_progress = None;
            cx.notify();
            return;
        }
        // Only a genuinely empty cache (a fresh login, nothing synced yet
        // for that account) has to wait behind the loading screen —
        // re-checked on every reconcile, so a later refresh() doesn't
        // re-flash it once every wanted account has real data. The loop
        // still moves a fresh account to `Loaded` itself once its first
        // full fetch (list + catalog enrichment) actually lands.
        let all_seeded = self.wanted.iter().all(|id| has_synced_before(id));
        self.state = if all_seeded {
            LibraryState::Loaded
        } else {
            LibraryState::Loading
        };

        for account_id in self.wanted.clone() {
            if self.tasks.contains_key(&account_id) {
                continue;
            }
            tracing::debug!(account_id, "starting gog library fetch loop");
            self.by_account
                .insert(account_id.clone(), load_cached_games(&account_id));
            let task = cx.spawn({
                let account_id = account_id.clone();
                async move |this, cx| watch_games(account_id, this, cx).await
            });
            self.tasks.insert(account_id, task);
        }
        self.games = merge(&self.by_account, &self.wanted);
        cx.notify();
    }

    /// Replaces one account's raw list and re-merges. `false` once the
    /// account is gone (logged out mid-fetch), so the loop can stop writing.
    fn store(&mut self, account_id: &str, shown: &[LibraryEntry], cx: &mut Context<Self>) -> bool {
        let Some(account_games) = self.by_account.get_mut(account_id) else {
            return false;
        };
        shown.clone_into(account_games);
        self.games = merge(&self.by_account, &self.wanted);
        cx.notify();
        true
    }
}

/// Concatenates every account's games into one list, keeping the first
/// copy of a product a given account owns — two accounts with the same
/// game would otherwise show it twice. `order` decides which account's
/// copy wins a duplicate (oldest account first, the order
/// `decanter_core::Db::accounts_in_profile` returns).
fn merge(by_account: &HashMap<String, Vec<LibraryEntry>>, order: &[String]) -> Vec<LibraryEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for account_id in order {
        let Some(account_games) = by_account.get(account_id) else {
            continue;
        };
        for entry in account_games {
            if seen.insert((entry.game.storefront, entry.game.id.clone())) {
                merged.push(entry.clone());
            }
        }
    }
    merged
}

/// One account's fetch loop. Ends on its own once the account is forgotten
/// (its list is gone from [`GogLibrary`]) or the library entity is dropped.
async fn watch_games(account_id: String, this: WeakEntity<GogLibrary>, cx: &mut AsyncApp) {
    let publish = |shown: &[LibraryEntry], cx: &mut AsyncApp| {
        this.update(cx, |library, cx| library.store(&account_id, shown, cx))
            .unwrap_or(false)
    };
    loop {
        // Checked once per cycle, before any of this round's writes land:
        // a first-ever sync means every row is about to be a fresh INSERT,
        // and none of them carries evidence of having just been added.
        let confirmed_new = has_synced_before(&account_id);

        // Pass one merges into whatever is already on screen rather than
        // starting from empty, so a full cache isn't wiped on every launch
        // and rebuilt over the next ten seconds. Ownership can still drop —
        // a refund, a revoked key — but that's settled by pruning once the
        // fetch is whole, below.
        let Ok(Some(mut shown)) = this.read_with(cx, |library, _| library.by_account.get(&account_id).cloned()) else {
            return;
        };
        tracing::debug!(account_id, "listing gog games");
        // Resolved fresh each cycle, not once outside the loop: a library
        // moved to a new location (see Ajustes) should be picked up by the
        // very next refresh, not only after this whole loop restarts.
        let listed = match crate::core::library_for_account(&account_id) {
            Ok(library) => {
                decanter_gog::list_games(&library, &account_id, &mut |batch| {
                    absorb(&account_id, confirmed_new, &mut shown, batch);
                    publish(&shown, cx);
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
                if !publish(&shown, cx) {
                    return;
                }
                collected
            }
            Err(err) => {
                // Whatever the cache seeded stays, unpruned: a failed
                // refresh shouldn't blank a library the user could still
                // browse, and a partial listing is no evidence of a
                // dropped entitlement.
                tracing::warn!(account_id, %err, "gog library listing failed, keeping what was cached");
                let failed = this.update(cx, |library, cx| {
                    library.state = LibraryState::Failed(err.to_string());
                    cx.notify();
                });
                if failed.is_err() {
                    return;
                }
                Vec::new()
            }
        };

        // Pass two updates entries in place. Everything it does is
        // best-effort inside, so there's nothing to handle out here.
        if !collected.is_empty() {
            tracing::debug!(account_id, count = collected.len(), "enriching gog catalog metadata");
            // A second handle to the app: both callbacks need one, and they're
            // never running at the same time.
            let mut progress_cx = cx.clone();
            let progress = this.clone();
            decanter_gog::enrich_games(
                &account_id,
                collected,
                &mut |batch| {
                    absorb(&account_id, confirmed_new, &mut shown, batch);
                    publish(&shown, cx);
                },
                &mut |done, total| {
                    let _ = progress.update(&mut progress_cx, |library, cx| {
                        library.enrich_progress = Some((done, total));
                        cx.notify();
                    });
                },
            )
            .await;
            tracing::debug!(account_id, "gog catalog enrichment finished");
        }

        // `state` starts as `Loading` for a fresh account (see `reconcile`)
        // and this is the one place that ever moves it to `Loaded` — on
        // every later cycle it's already `Loaded`, so this is a no-op.
        let alive = this.update(cx, |library, cx| {
            library.enrich_progress = None;
            if listed_ok {
                tracing::info!(account_id, "gog library ready");
                library.state = LibraryState::Loaded;
            }
            cx.notify();
        });
        if alive.is_err() {
            return;
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
        // Tagged here rather than by decanter_gog itself: this is the one
        // place that knows both the entry and which account's loop it came
        // from, and the merged multi-account view needs to tell two
        // accounts' copies of the same game apart.
        entry.game.account_id = account_id.to_owned();
        save_entry(account_id, &entry, confirmed_new);
        match shown.iter_mut().find(|existing| existing.game.id == entry.game.id) {
            Some(existing) => *existing = entry,
            None => shown.push(entry),
        }
    }
}
