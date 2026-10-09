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

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use decanter_core::{Game, InstallState};
use decanter_gog::{Batch, LibraryEntry};
use gpui::{AsyncApp, Context, Entity, Task, WeakEntity};

use super::Session;
use super::cache::{has_synced_before, load_cached_games, save_entries};

/// Raised by `main` once GPUI has returned and before the tokio runtime is
/// dropped. Each loop drives its own `block_on` on a blocking-pool thread,
/// and dropping the runtime waits for those threads — so they have to be
/// told to end first, or the next timer they poll panics on a runtime
/// that's already shutting down.
static SHUTDOWN: std::sync::LazyLock<tokio::sync::watch::Sender<bool>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::Sender::new(false));

pub fn shut_down_fetch_loops() {
    SHUTDOWN.send_replace(true);
}

/// Long enough that a full re-resolve never overlaps itself in practice.
const REFRESH_INTERVAL: Duration = Duration::from_mins(15);

/// How often every owned game's catalog data is looked up again, rather
/// than only the ones a cycle hasn't seen before.
const FULL_ENRICH_INTERVAL: Duration = Duration::from_hours(24);

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
    pub(crate) games: Vec<LibraryEntry>,
    /// Bumped on every change to `games`, so views can cache what they
    /// derive from it instead of re-deriving on every render.
    pub(crate) revision: u64,
    pub(crate) state: LibraryState,
    /// "N of M enriched" while pass two runs, `None` otherwise — a rough
    /// progress hint for [`LibraryState::Loading`], not tracked per
    /// account. With more than one GOG account loading at once this just
    /// reflects whichever wrote most recently, the same simplification
    /// `state` itself already makes.
    pub(crate) enrich_progress: Option<(usize, usize)>,
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
            revision: 0,
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
        self.revision = self.revision.wrapping_add(1);
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
        self.revision = self.revision.wrapping_add(1);
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
            self.revision = self.revision.wrapping_add(1);
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
        self.revision = self.revision.wrapping_add(1);
        cx.notify();
    }

    /// Replaces one account's raw list and re-merges. `false` once the
    /// account is gone (logged out mid-fetch), so the loop can stop writing.
    fn store(&mut self, account_id: &str, shown: Vec<LibraryEntry>, cx: &mut Context<Self>) -> bool {
        let Some(account_games) = self.by_account.get_mut(account_id) else {
            return false;
        };
        *account_games = shown;
        self.games = merge(&self.by_account, &self.wanted);
        self.revision = self.revision.wrapping_add(1);
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

/// What a fetch loop reports back to the UI thread.
enum Update {
    /// The account's whole list as it now stands.
    Shown(Vec<LibraryEntry>),
    Progress(usize, usize),
    Failed(String),
    /// A cycle finished; `listed` says whether pass one succeeded.
    CycleDone {
        listed: bool,
    },
}

/// One account's fetch loop, split in two: the fetch itself, the cache
/// writes and the merging of each batch run on a tokio worker, so a sync
/// of hundreds of games never stalls the window; this half only applies
/// what the worker reports. Ends once the account is forgotten or the
/// library entity is dropped.
async fn watch_games(account_id: String, this: WeakEntity<GogLibrary>, cx: &mut AsyncApp) {
    let Ok(Some(seed)) = this.read_with(cx, |library, _| library.by_account.get(&account_id).cloned()) else {
        return;
    };
    let (updates, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    // `decanter_gog`'s futures aren't `Send` (their batch callbacks are
    // borrowed closures), so the loop gets a blocking-pool thread of its
    // own and drives itself there. Dropping `receiver` — this task being
    // cancelled — is what tells it to stop.
    let runtime = tokio::runtime::Handle::current();
    let worker_account = account_id.clone();
    tokio::task::spawn_blocking(move || runtime.block_on(fetch_loop(worker_account, seed, updates)));
    // A burst of batches costs one re-render, not one per batch: only the
    // newest list in each burst is applied.
    while let Some(first) = receiver.recv().await {
        let mut burst = vec![first];
        while let Ok(update) = receiver.try_recv() {
            burst.push(update);
        }
        let alive = this.update(cx, |library, cx| {
            let mut latest = None;
            for update in burst {
                match update {
                    Update::Shown(shown) => latest = Some(shown),
                    Update::Progress(done, total) => library.enrich_progress = Some((done, total)),
                    Update::Failed(err) => library.state = LibraryState::Failed(err),
                    Update::CycleDone { listed } => {
                        library.enrich_progress = None;
                        // `state` starts as `Loading` for a fresh account
                        // (see `reconcile`) and this is the one place that
                        // ever moves it to `Loaded`.
                        if listed {
                            tracing::info!(account_id, "gog library ready");
                            library.state = LibraryState::Loaded;
                        }
                    }
                }
            }
            let stored = latest.is_none_or(|shown| library.store(&account_id, shown, cx));
            cx.notify();
            stored
        });
        if !matches!(alive, Ok(true)) {
            return;
        }
    }
}

async fn fetch_loop(
    account_id: String,
    mut shown: Vec<LibraryEntry>,
    updates: tokio::sync::mpsc::UnboundedSender<Update>,
) {
    let send = |update: Update| updates.send(update).is_ok();
    let mut shutdown = SHUTDOWN.subscribe();
    // Catalog data barely moves, so after one full enrichment per session
    // a cycle only enriches what it hasn't seen yet, until the next full
    // pass is due.
    let mut enriched_at: Option<Instant> = None;
    let mut enriched: HashSet<String> = HashSet::new();
    loop {
        // The whole pass is dropped the moment the UI side goes away (the
        // account forgotten, the app quitting) instead of running on into
        // a runtime that's shutting down.
        let cycle = async {
            // Checked once per cycle, before any of this round's writes land:
            // a first-ever sync means every row is about to be a fresh INSERT,
            // and none of them carries evidence of having just been added.
            let confirmed_new = has_synced_before(&account_id);

            // Pass one merges into whatever is already on screen rather than
            // starting from empty, so a full cache isn't wiped on every launch
            // and rebuilt over the next ten seconds. Ownership can still drop —
            // a refund, a revoked key — but that's settled by pruning once the
            // fetch is whole, below.
            tracing::debug!(account_id, "listing gog games");
            // Resolved fresh each cycle, not once outside the loop: a library
            // moved to a new location (see Ajustes) should be picked up by the
            // very next refresh, not only after this whole loop restarts.
            let listed = match crate::core::library_for_account(&account_id) {
                Ok(library) => {
                    decanter_gog::list_games(&library, &account_id, &mut |batch| {
                        let changed = absorb(&account_id, &mut shown, batch, Pass::Listed);
                        if !changed.is_empty() {
                            save_entries(&account_id, &changed, confirmed_new);
                            send(Update::Shown(shown.clone()));
                        }
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
                    if prune(&account_id, &mut shown, &collected) && !send(Update::Shown(shown.clone())) {
                        return None;
                    }
                    collected
                }
                Err(err) => {
                    // Whatever the cache seeded stays, unpruned: a failed
                    // refresh shouldn't blank a library the user could still
                    // browse, and a partial listing is no evidence of a
                    // dropped entitlement.
                    tracing::warn!(account_id, %err, "gog library listing failed, keeping what was cached");
                    if !send(Update::Failed(err.to_string())) {
                        return None;
                    }
                    Vec::new()
                }
            };

            let full = enriched_at.is_none_or(|at| at.elapsed() >= FULL_ENRICH_INTERVAL);
            let to_enrich: Vec<LibraryEntry> = collected
                .into_iter()
                .filter(|entry| full || !enriched.contains(&entry.game.id))
                .collect();

            // Pass two updates entries in place. Everything it does is
            // best-effort inside, so there's nothing to handle out here.
            if !to_enrich.is_empty() {
                tracing::debug!(
                    account_id,
                    count = to_enrich.len(),
                    full,
                    "enriching gog catalog metadata"
                );
                let ids: Vec<String> = to_enrich.iter().map(|entry| entry.game.id.clone()).collect();
                // Its batches each carry an entry as far as enrichment has
                // got with it — a GamesDB batch still has pass one's
                // category, which the product batch after it corrects — so
                // they're folded into a copy and only the settled result is
                // shown and cached. A first sync has nothing better on
                // screen, so it streams them as they come.
                let mut working = shown.clone();
                decanter_gog::enrich_games(
                    &account_id,
                    to_enrich,
                    &mut |batch| {
                        absorb(&account_id, &mut working, batch, Pass::Enriched);
                        if !confirmed_new {
                            send(Update::Shown(working.clone()));
                        }
                    },
                    &mut |done, total| {
                        send(Update::Progress(done, total));
                    },
                )
                .await;
                let changed = changed_since(&shown, &working);
                tracing::debug!(account_id, changed = changed.len(), "gog catalog enrichment finished");
                if !changed.is_empty() {
                    save_entries(&account_id, &changed, confirmed_new);
                    shown = working;
                    if !send(Update::Shown(shown.clone())) {
                        return None;
                    }
                }
                enriched.extend(ids);
                if full {
                    enriched_at = Some(Instant::now());
                }
            }
            Some(listed_ok)
        };
        let listed_ok = tokio::select! {
            listed = cycle => listed,
            () = updates.closed() => return,
            _ = shutdown.wait_for(|down| *down) => return,
        };
        let Some(listed_ok) = listed_ok else {
            return;
        };
        if !send(Update::CycleDone { listed: listed_ok }) {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(REFRESH_INTERVAL) => {}
            () = updates.closed() => return,
            _ = shutdown.wait_for(|down| *down) => return,
        }
    }
}

/// Drops whatever a complete fetch didn't list, on screen and in the
/// cache alike — the only thing that ever notices an entitlement going
/// away, since a dropped id simply stops coming back. `true` if anything
/// on screen went.
fn prune(account_id: &str, shown: &mut Vec<LibraryEntry>, fetched: &[LibraryEntry]) -> bool {
    let owned: HashSet<&str> = fetched.iter().map(|entry| entry.game.id.as_str()).collect();
    let before = shown.len();
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
    shown.len() != before
}

/// Which of the two fetch passes a batch came from — they own different
/// parts of an entry. See [`absorb`].
#[derive(Clone, Copy)]
enum Pass {
    Listed,
    Enriched,
}

/// Folds a batch into `shown` and returns the entries that actually
/// changed, as they now stand — what's worth caching and re-rendering.
///
/// Position is kept for an entry that's already there, so pass two filling
/// in a cover doesn't reshuffle the list under someone mid-scroll. Sorting
/// is the UI's job, not the fetch's.
///
/// Pass one only knows GOG's bare product listing: for a game already on
/// screen it updates just install state and library rank, since its art,
/// category and title are the unenriched ones pass two had already
/// replaced — taking them flipped every cover back to GOG's landscape
/// product shot until pass two came round again. Pass two's batches do
/// replace an entry, but never blank a field it already had: a lookup
/// that failed this cycle isn't news that the data went away.
fn absorb(account_id: &str, shown: &mut Vec<LibraryEntry>, batch: Batch, pass: Pass) -> Vec<LibraryEntry> {
    let mut changed = Vec::new();
    for mut entry in batch {
        // Tagged here rather than by decanter_gog itself: this is the one
        // place that knows both the entry and which account's loop it came
        // from, and the merged multi-account view needs to tell two
        // accounts' copies of the same game apart.
        entry.game.account_id = account_id.to_owned();
        let Some(existing) = shown.iter_mut().find(|existing| existing.game.id == entry.game.id) else {
            changed.push(entry.clone());
            shown.push(entry);
            continue;
        };
        let merged = match pass {
            Pass::Listed => LibraryEntry {
                library_rank: entry.library_rank.or(existing.library_rank),
                game: Game {
                    install_state: entry.game.install_state,
                    ..existing.game.clone()
                },
                ..existing.clone()
            },
            Pass::Enriched => keep_known(entry, existing),
        };
        if merged != *existing {
            *existing = merged;
            changed.push(existing.clone());
        }
    }
    changed
}

/// `entry`, with anything it left blank taken from `previous`.
fn keep_known(mut entry: LibraryEntry, previous: &LibraryEntry) -> LibraryEntry {
    fn keep<T: Clone>(field: &mut Option<T>, previous: Option<&T>) {
        if field.is_none() {
            *field = previous.cloned();
        }
    }
    let (game, old) = (&mut entry.game, &previous.game);
    keep(&mut game.icon_url, old.icon_url.as_ref());
    keep(&mut game.cover_url, old.cover_url.as_ref());
    keep(&mut game.background_url, old.background_url.as_ref());
    keep(&mut game.description, old.description.as_ref());
    keep(&mut game.developer, old.developer.as_ref());
    keep(&mut game.publisher, old.publisher.as_ref());
    keep(&mut game.rating, old.rating.as_ref());
    keep(&mut game.release_date, old.release_date.as_ref());
    if game.genres.is_empty() {
        game.genres.clone_from(&old.genres);
    }
    keep(&mut entry.purchased_at, previous.purchased_at.as_ref());
    keep(&mut entry.library_rank, previous.library_rank.as_ref());
    keep(&mut entry.parent_id, previous.parent_id.as_ref());
    entry
}

/// The entries of `after` that are new or differ from `before`.
fn changed_since(before: &[LibraryEntry], after: &[LibraryEntry]) -> Vec<LibraryEntry> {
    let before: HashMap<&str, &LibraryEntry> = before.iter().map(|entry| (entry.game.id.as_str(), entry)).collect();
    after
        .iter()
        .filter(|entry| before.get(entry.game.id.as_str()) != Some(entry))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use decanter_core::Game;
    use decanter_gog::{GameKind, LibraryEntry};

    use super::{Pass, absorb};

    fn entry(cover: &str, kind: GameKind) -> LibraryEntry {
        LibraryEntry {
            game: Game {
                id: "1207658924".to_owned(),
                account_id: "acct".to_owned(),
                title: "Betrayer".to_owned(),
                cover_url: Some(cover.to_owned()),
                ..Default::default()
            },
            kind,
            purchased_at: None,
            library_rank: None,
            installable: true,
            parent_id: None,
        }
    }

    #[test]
    fn listing_keeps_enriched_art_and_category() {
        let mut enriched = entry("vertical.jpg?namespace=gamesdb", GameKind::Game);
        enriched.game.genres = vec!["Horror".to_owned()];
        let mut shown = vec![enriched.clone()];

        let mut listed = entry("_392.jpg", GameKind::Extra);
        listed.library_rank = Some(3);
        let changed = absorb("acct", &mut shown, vec![listed], Pass::Listed);

        assert_eq!(changed.len(), 1);
        assert_eq!(shown[0].game.cover_url, enriched.game.cover_url);
        assert_eq!(shown[0].kind, GameKind::Game);
        assert_eq!(shown[0].game.genres, enriched.game.genres);
        assert_eq!(shown[0].library_rank, Some(3));
    }

    #[test]
    fn unchanged_listing_is_not_a_change() {
        let mut shown = vec![entry("vertical.jpg", GameKind::Game)];
        let changed = absorb(
            "acct",
            &mut shown,
            vec![entry("_392.jpg", GameKind::Extra)],
            Pass::Listed,
        );
        assert!(changed.is_empty());
    }

    #[test]
    fn enrichment_never_blanks_a_known_field() {
        let mut known = entry("vertical.jpg", GameKind::Game);
        known.parent_id = Some("1".to_owned());
        known.game.background_url = Some("bg.jpg".to_owned());
        let mut shown = vec![known];

        let mut partial = entry("vertical.jpg", GameKind::Game);
        partial.game.description = Some("A ghost story.".to_owned());
        absorb("acct", &mut shown, vec![partial], Pass::Enriched);

        assert_eq!(shown[0].parent_id.as_deref(), Some("1"));
        assert_eq!(shown[0].game.background_url.as_deref(), Some("bg.jpg"));
        assert_eq!(shown[0].game.description.as_deref(), Some("A ghost story."));
    }
}
