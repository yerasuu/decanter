//! Pure sqlite reads for the Library view's detail pane — achievements,
//! favorites and playtime are all keyed by `(account_id, storefront,
//! product_id)`, nothing to join against a bottle or Program for any of it.

use chrono::{DateTime, Local};
use decanter_core::{CachedAchievement, Collection, Db, Playtime, Storefront};

/// A cached achievement with its unlock time parsed once, on load, so the
/// grid doesn't re-parse the stored text every frame.
#[derive(Clone)]
pub(crate) struct Achievement {
    pub(crate) cached: CachedAchievement,
    pub(crate) unlocked_at: Option<UnlockedAt>,
}

#[derive(Clone)]
pub(crate) enum UnlockedAt {
    At(DateTime<Local>),
    /// Neither shape below; shown as stored rather than hidden.
    Raw(String),
}

impl UnlockedAt {
    /// GOG's API sends `2021-10-12T00:00:56+0000`; the hook stores RFC 3339
    /// (`+00:00`), see `decanter_gog::hook`.
    fn parse(raw: &str, achievement_key: &str) -> Self {
        DateTime::parse_from_rfc3339(raw)
            .or_else(|_| DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%z"))
            .map_or_else(
                |err| {
                    tracing::warn!(achievement_key, raw, %err, "unparsable achievement unlock time, shown as stored");
                    Self::Raw(raw.to_owned())
                },
                |at| Self::At(at.with_timezone(&Local)),
            )
    }
}

pub(super) fn load_achievements(account_id: String, product_id: String) -> anyhow::Result<Vec<Achievement>> {
    tracing::debug!(account_id = %account_id, product_id = %product_id, "loading cached achievements");
    let db = crate::achv_try!(Db::open_active().inspect_err(
        |err| tracing::error!(account_id = %account_id, product_id = %product_id, error = %err, "failed to open achievement db")
    ));
    let achievements = crate::achv_try!(db.list_achievements(&account_id, Storefront::Gog, &product_id).inspect_err(
        |err| tracing::error!(account_id = %account_id, product_id = %product_id, error = %err, "failed to read achievement cache")
    ));
    Ok(achievements
        .into_iter()
        .map(|cached| Achievement {
            unlocked_at: cached
                .unlocked_at
                .as_deref()
                .map(|raw| UnlockedAt::parse(raw, &cached.achievement_key)),
            cached,
        })
        .collect())
}

/// Result of the last live poll for the selected game — distinct from
/// "cache is empty", which by itself is ambiguous: still loading, this
/// game genuinely has no achievements, or the poll itself failed (no GOG
/// session, offline). The empty case only means something once this says
/// which one it was.
#[derive(Clone, PartialEq)]
pub(super) enum PollStatus {
    Idle,
    Polling,
    Done,
    Failed(String),
}

/// Hits the GOG API and refreshes the cache — the thing that used to
/// require running `decanter achievements` by hand.
pub(super) async fn poll_achievements(account_id: String, product_id: String) -> Result<(), String> {
    tracing::debug!(account_id = %account_id, product_id = %product_id, "polling gog for achievement updates");
    let mut db = crate::achv_try!(Db::open_active().map_err(|err| err.to_string()).inspect_err(
        |err| tracing::error!(account_id = %account_id, product_id = %product_id, error = %err, "failed to open db for achievement poll")
    ));
    crate::achv_try!(
        decanter_gog::poll_achievements(&mut db, &account_id, &product_id)
            .await
            .map_err(|err| err.to_string())
            .inspect_err(|err| tracing::error!(
                account_id = %account_id,
                product_id = %product_id,
                error = %err,
                "achievement poll failed",
            ))
    );
    Ok(())
}

/// Collections and their membership, read together — the sidebar needs
/// both on every render and they're meaningless apart.
#[derive(Default, Clone, PartialEq)]
pub(super) struct Collections {
    pub list: Vec<Collection>,
    /// Product ids per collection id.
    pub members: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl Collections {
    pub fn contains(&self, collection_id: &str, product_id: &str) -> bool {
        self.members
            .get(collection_id)
            .is_some_and(|ids| ids.contains(product_id))
    }

    /// Whether this game sits in any collection at all — what decides the
    /// "Sin coleccion" shelf.
    pub fn is_uncategorized(&self, product_id: &str) -> bool {
        !self.members.values().any(|ids| ids.contains(product_id))
    }
}

pub(super) fn load_collections(profile_id: &str) -> Collections {
    let Ok(db) = Db::open_active()
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to open db, showing no collections"))
    else {
        return Collections::default();
    };
    Collections {
        list: db
            .list_collections(profile_id)
            .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to list collections"))
            .unwrap_or_default(),
        // GOG ids and external games' uuids never collide, so one map
        // keyed by product id holds both.
        members: [Storefront::Gog, Storefront::Unspecified]
            .into_iter()
            .flat_map(|storefront| {
                db.collection_members(profile_id, storefront)
                    .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to load collection membership"))
                    .unwrap_or_default()
            })
            .fold(std::collections::HashMap::new(), |mut members, (collection_id, ids)| {
                members
                    .entry(collection_id)
                    .or_insert_with(std::collections::HashSet::new)
                    .extend(ids);
                members
            }),
    }
}

pub(super) fn create_collection(profile_id: &str, name: &str) {
    let Ok(db) = Db::open_active()
        .inspect_err(|err| tracing::warn!(profile_id, name, error = %err, "failed to open db, collection not created"))
    else {
        return;
    };
    match db.create_collection(profile_id, name) {
        Ok(collection) => tracing::info!(profile_id, name, collection_id = %collection.id, "collection created"),
        Err(err) => tracing::warn!(profile_id, name, error = %err, "failed to create collection"),
    }
}

/// Creates a collection whose membership is a saved filter rather than a
/// list of games.
pub(super) fn create_dynamic_collection(profile_id: &str, name: &str, filter: &str) {
    if let Ok(db) = Db::open_active() {
        match db.create_collection(profile_id, name) {
            Ok(created) => {
                tracing::info!(profile_id, name, filter, collection_id = %created.id, "dynamic collection created");
                if let Err(err) = db.set_collection_filter(&created.id, Some(filter)) {
                    tracing::warn!(collection_id = %created.id, filter, error = %err, "failed to set collection filter");
                }
            }
            Err(err) => tracing::warn!(profile_id, name, error = %err, "failed to create dynamic collection"),
        }
    } else {
        tracing::warn!(profile_id, name, "failed to open db, dynamic collection not created");
    }
}

/// Moves a collection by `delta` slots in the shelf order.
pub(super) fn move_collection(profile_id: &str, collection_id: &str, delta: i64) {
    if let Ok(db) = Db::open_active() {
        if let Err(err) = db.move_collection(profile_id, collection_id, delta) {
            tracing::warn!(profile_id, collection_id, delta, error = %err, "failed to move collection");
        }
    } else {
        tracing::warn!(
            profile_id,
            collection_id,
            delta,
            "failed to open db, collection not moved"
        );
    }
}

pub(super) fn hidden_ids(account_id: &str, storefront: Storefront) -> std::collections::HashSet<String> {
    Db::open_active()
        .and_then(|db| db.hidden_ids(account_id, storefront))
        .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to read hidden ids"))
        .unwrap_or_default()
}

pub(super) fn set_hidden(account_id: &str, storefront: Storefront, product_id: &str, hidden: bool) {
    if let Ok(db) = Db::open_active() {
        match db.set_hidden(account_id, storefront, product_id, hidden) {
            Ok(()) => tracing::info!(account_id, product_id, hidden, "game hidden state changed"),
            Err(err) => tracing::warn!(account_id, product_id, hidden, error = %err, "failed to set hidden state"),
        }
    } else {
        tracing::warn!(
            account_id,
            product_id,
            hidden,
            "failed to open db, hidden state not changed"
        );
    }
}

/// Which shelves the user left collapsed.
pub(super) const COLLAPSED_SHELVES: &str = "collapsed_shelves";
/// The cover browser's shelves: which source each one points at and how
/// it's sorted, in the order they were dragged into.
pub(super) const BROWSER_SHELVES: &str = "browser_shelves";
/// How the sidebar is narrowed, encoded as a `SavedFilter` — the same
/// format a dynamic collection stores, so the two can't drift apart.
pub(super) const SIDEBAR_FILTERS: &str = "sidebar_filters";
/// Whether the filter panel was left open.
pub(super) const FILTER_PANEL: &str = "filter_panel_open";

/// View state that isn't worth its own table, keyed by profile so two
/// profiles don't share one arrangement of the same library. Read through
/// the `settings` key/value table.
pub(super) fn view_state(profile_id: &str, name: &str) -> Option<String> {
    let db = Db::open_active()
        .inspect_err(|err| tracing::warn!(profile_id, name, error = %err, "failed to open db for view state read"))
        .ok()?;
    db.setting(&format!("{name}:{profile_id}"))
        .inspect_err(|err| tracing::warn!(profile_id, name, error = %err, "failed to read view state setting"))
        .ok()
        .flatten()
}

pub(super) fn save_view_state(profile_id: &str, name: &str, value: &str) {
    if let Ok(db) = Db::open_active() {
        if let Err(err) = db.set_setting(&format!("{name}:{profile_id}"), value) {
            tracing::warn!(profile_id, name, error = %err, "failed to save view state setting");
        }
    } else {
        tracing::warn!(profile_id, name, "failed to open db, view state not saved");
    }
}

/// Stored per profile so the list looks the same after a restart as it did
/// when they closed the app.
pub(super) fn collapsed_shelves(profile_id: &str) -> std::collections::HashSet<String> {
    view_state(profile_id, COLLAPSED_SHELVES)
        .map(|raw| {
            raw.split('\u{1f}')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn save_collapsed_shelves(profile_id: &str, collapsed: &std::collections::HashSet<String>) {
    // Unit separator as the delimiter: shelf ids are uuids and built-in
    // names, but nothing stops a future one containing a comma.
    let joined = collapsed.iter().cloned().collect::<Vec<_>>().join("\u{1f}");
    save_view_state(profile_id, COLLAPSED_SHELVES, &joined);
}

pub(super) fn delete_collection(collection_id: &str) {
    if let Ok(db) = Db::open_active() {
        match db.delete_collection(collection_id) {
            Ok(()) => tracing::info!(collection_id, "collection deleted"),
            Err(err) => tracing::warn!(collection_id, error = %err, "failed to delete collection"),
        }
    } else {
        tracing::warn!(collection_id, "failed to open db, collection not deleted");
    }
}

pub(super) fn rename_collection(collection_id: &str, name: &str) {
    if let Ok(db) = Db::open_active() {
        match db.rename_collection(collection_id, name) {
            Ok(()) => tracing::info!(collection_id, name, "collection renamed"),
            Err(err) => tracing::warn!(collection_id, name, error = %err, "failed to rename collection"),
        }
    } else {
        tracing::warn!(collection_id, name, "failed to open db, collection not renamed");
    }
}

pub(super) fn set_collection_member(collection_id: &str, storefront: Storefront, product_id: &str, member: bool) {
    if let Ok(db) = Db::open_active() {
        match db.set_collection_member(collection_id, storefront, product_id, member) {
            Ok(()) => tracing::info!(collection_id, product_id, member, "collection membership changed"),
            Err(err) => {
                tracing::warn!(collection_id, product_id, member, error = %err, "failed to update collection membership");
            }
        }
    } else {
        tracing::warn!(
            collection_id,
            product_id,
            member,
            "failed to open db, collection membership not changed"
        );
    }
}

pub(super) fn is_favorite(account_id: &str, storefront: Storefront, product_id: &str) -> bool {
    Db::open_active()
        .and_then(|db| db.is_favorite(account_id, storefront, product_id))
        .inspect_err(
            |err| tracing::warn!(account_id, product_id, error = %err, "failed to read favorite state, defaulting to not favorited"),
        )
        .unwrap_or(false)
}

pub(super) fn set_favorite(account_id: &str, storefront: Storefront, product_id: &str, favorite: bool) {
    if let Ok(db) = Db::open_active() {
        match db.set_favorite(account_id, storefront, product_id, favorite) {
            Ok(()) => tracing::info!(account_id, product_id, favorite, "favorite toggled"),
            Err(err) => tracing::warn!(account_id, product_id, favorite, error = %err, "failed to toggle favorite"),
        }
    } else {
        tracing::warn!(
            account_id,
            product_id,
            favorite,
            "failed to open db, favorite not toggled"
        );
    }
}

/// Every GOG account in `profile_id`, oldest first — the merge order every
/// aggregate function below uses, so a product two accounts both own
/// consistently prefers the same one's data everywhere in the UI, not just
/// in which row `gog_library` happens to show — then the profile itself,
/// which external games are filed under (see `crate::external`).
fn owners(profile_id: &str) -> Vec<(String, Storefront)> {
    gog_accounts_with_names(profile_id)
        .into_iter()
        .map(|(id, _name)| (id, Storefront::Gog))
        .chain(std::iter::once((profile_id.to_owned(), Storefront::Unspecified)))
        .collect()
}

/// `(id, name)` for every GOG account in the profile, oldest first — what
/// the sidebar's account filter lists, since a bare id means nothing in a
/// checkbox label.
pub(super) fn gog_accounts_with_names(profile_id: &str) -> Vec<(String, String)> {
    Db::open_active()
        .and_then(|db| db.accounts_in_profile(profile_id))
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to list accounts in profile"))
        .unwrap_or_default()
        .into_iter()
        .filter(|account| account.storefront == Storefront::Gog)
        .map(|account| (account.id, account.name))
        .collect()
}

/// Every favorited product id across the profile's accounts — a game
/// favorited on any of them reads as favorited here, since the merged
/// library shows one row for it regardless of which account favorited it.
pub(super) fn favorite_ids_in_profile(profile_id: &str) -> std::collections::HashSet<String> {
    let Ok(db) = Db::open_active()
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to open db, no favorites shown"))
    else {
        return std::collections::HashSet::new();
    };
    owners(profile_id)
        .iter()
        .flat_map(|(account_id, storefront)| {
            db.list_favorite_ids(account_id, *storefront)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to list favorite ids"))
                .unwrap_or_default()
        })
        .collect()
}

pub(super) fn hidden_ids_in_profile(profile_id: &str) -> std::collections::HashSet<String> {
    let Ok(db) = Db::open_active()
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to open db, no hidden ids applied"))
    else {
        return std::collections::HashSet::new();
    };
    owners(profile_id)
        .iter()
        .flat_map(|(account_id, storefront)| {
            db.hidden_ids(account_id, *storefront)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to list hidden ids"))
                .unwrap_or_default()
        })
        .collect()
}

/// Playtime and achievement counts for the whole library in two queries per
/// account, for sorting and filtering the sidebar — the per-game reads
/// below would mean hundreds of round trips on every keystroke.
///
/// When two accounts own the same product, whichever is first in
/// `owners`'s order wins that product's stats — the same account
/// `gog_library`'s merge already picked to represent it on screen, so the
/// numbers shown next to a game always describe the copy actually
/// displayed rather than a blend of two.
pub(super) fn library_stats(profile_id: &str) -> LibraryStats {
    let Ok(db) = Db::open_active()
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to open db, library stats are empty"))
    else {
        return LibraryStats::default();
    };
    let mut stats = LibraryStats::default();
    for (account_id, storefront) in owners(profile_id).iter().rev() {
        let storefront = *storefront;
        // Reversed: later inserts overwrite earlier ones in a HashMap, so
        // running oldest-account-last makes the *first* account's entry
        // the one left standing for a product both have.
        stats.playtime.extend(
            db.all_playtime(account_id, storefront)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to read playtime stats"))
                .unwrap_or_default(),
        );
        stats.achievements.extend(
            db.all_achievement_counts(account_id, storefront)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to read achievement count stats"))
                .unwrap_or_default(),
        );
        stats.added.extend(
            db.all_first_seen(account_id, storefront)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to read first-seen stats"))
                .unwrap_or_default(),
        );
        stats.added_rank.extend(
            db.all_library_ranks(account_id, storefront)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to read library rank stats"))
                .unwrap_or_default(),
        );
        // From the appmanifests, not the database: what's installed lives
        // on disk now.
        stats.sizes.extend(
            crate::core::installed(account_id)
                .into_iter()
                .filter_map(|(game_id, installed)| Some((game_id, installed.size_on_disk?))),
        );
    }
    stats
}

#[derive(Default, Clone, PartialEq)]
pub(super) struct LibraryStats {
    pub playtime: std::collections::HashMap<String, Playtime>,
    /// `(unlocked, total)` per product id.
    pub achievements: std::collections::HashMap<String, (usize, usize)>,
    /// When each game first entered the library, unix seconds — a local
    /// approximation, only consulted when `added_rank` doesn't cover a
    /// game.
    pub added: std::collections::HashMap<String, i64>,
    /// GOG's own added-to-library order, 0 = most recent — see
    /// `decanter_core::Db::all_library_ranks`. The authoritative source when
    /// present; covers gifts and dev grants that `added` above can't.
    pub added_rank: std::collections::HashMap<String, i64>,
    /// Bytes on disk, for installed games only.
    pub sizes: std::collections::HashMap<String, u64>,
}

impl LibraryStats {
    pub fn seconds(&self, product_id: &str) -> i64 {
        self.playtime.get(product_id).map_or(0, |entry| entry.seconds_total)
    }

    pub fn last_played(&self, product_id: &str) -> i64 {
        self.playtime
            .get(product_id)
            .and_then(|entry| entry.last_played_at)
            .unwrap_or(0)
    }

    /// Bytes this game occupies, or `None` when it isn't installed —
    /// which sorts apart from "installed but empty".
    pub fn size(&self, product_id: &str) -> Option<u64> {
        self.sizes.get(product_id).copied()
    }

    pub fn added(&self, product_id: &str) -> i64 {
        self.added.get(product_id).copied().unwrap_or(0)
    }

    /// GOG's added-to-library rank for this game, when known — smaller is
    /// more recent, the opposite direction from `added()`'s timestamps.
    pub fn added_rank(&self, product_id: &str) -> Option<i64> {
        self.added_rank.get(product_id).copied()
    }

    /// Completion as a fraction. A game with no achievements cached sorts
    /// last rather than as 0% complete — it isn't 0% of anything.
    pub fn completion(&self, product_id: &str) -> Option<f64> {
        match self.achievements.get(product_id) {
            Some((unlocked, total)) if *total > 0 => Some(*unlocked as f64 / *total as f64),
            _ => None,
        }
    }
}

pub(super) fn load_playtime(account_id: &str, storefront: Storefront, product_id: &str) -> Playtime {
    Db::open_active()
        .and_then(|db| db.get_playtime(account_id, storefront, product_id))
        .inspect_err(
            |err| tracing::warn!(account_id, product_id, error = %err, "failed to read playtime, defaulting to zero"),
        )
        .unwrap_or_default()
}

pub(super) fn add_playtime(account_id: &str, storefront: Storefront, product_id: &str, seconds: i64) {
    if let Ok(db) = Db::open_active() {
        if let Err(err) = db.add_playtime(account_id, storefront, product_id, seconds) {
            tracing::warn!(account_id, product_id, seconds, error = %err, "failed to record playtime");
        }
    } else {
        tracing::warn!(
            account_id,
            product_id,
            seconds,
            "failed to open db, playtime not recorded"
        );
    }
}
