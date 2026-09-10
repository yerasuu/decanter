//! Pure sqlite reads for the Library view's detail pane — achievements,
//! favorites and playtime are all keyed by `(profile_id, storefront,
//! product_id)`, nothing to join against a bottle or Program for any of it.

use achv_core::{CachedAchievement, Collection, Db, Playtime, Storefront};

pub(super) fn load_achievements(profile_id: String, product_id: String) -> anyhow::Result<Vec<CachedAchievement>> {
    let db = crate::achv_try!(Db::open(achv_core::default_db_path()));
    let achievements = crate::achv_try!(db.list_achievements(&profile_id, Storefront::Gog, &product_id));
    Ok(achievements)
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
/// require running `achv-tray achievements` by hand.
pub(super) async fn poll_achievements(profile_id: String, product_id: String) -> Result<(), String> {
    let mut db = crate::achv_try!(Db::open(achv_core::default_db_path()).map_err(|err| err.to_string()));
    crate::achv_try!(
        achv_gog::poll_achievements(&mut db, &profile_id, &product_id)
            .await
            .map_err(|err| err.to_string())
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
    let Ok(db) = Db::open(achv_core::default_db_path()) else {
        return Collections::default();
    };
    Collections {
        list: db.list_collections(profile_id).unwrap_or_default(),
        members: db.collection_members(profile_id, Storefront::Gog).unwrap_or_default(),
    }
}

pub(super) fn create_collection(profile_id: &str, name: &str) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.create_collection(profile_id, name);
    }
}

/// Creates a collection whose membership is a saved filter rather than a
/// list of games.
pub(super) fn create_dynamic_collection(profile_id: &str, name: &str, filter: &str) {
    if let Ok(db) = Db::open(achv_core::default_db_path())
        && let Ok(created) = db.create_collection(profile_id, name)
    {
        let _ = db.set_collection_filter(&created.id, Some(filter));
    }
}

/// Moves a collection by `delta` slots in the shelf order.
pub(super) fn move_collection(profile_id: &str, collection_id: &str, delta: i64) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.move_collection(profile_id, collection_id, delta);
    }
}

pub(super) fn hidden_ids(profile_id: &str) -> std::collections::HashSet<String> {
    Db::open(achv_core::default_db_path())
        .and_then(|db| db.hidden_ids(profile_id, Storefront::Gog))
        .unwrap_or_default()
}

pub(super) fn set_hidden(profile_id: &str, product_id: &str, hidden: bool) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.set_hidden(profile_id, Storefront::Gog, product_id, hidden);
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

/// View state that isn't worth its own table, keyed by profile so two GOG
/// accounts don't share one arrangement of the same library. Read through
/// the `settings` key/value table.
pub(super) fn view_state(profile_id: &str, name: &str) -> Option<String> {
    let db = Db::open(achv_core::default_db_path()).ok()?;
    db.setting(&format!("{name}:{profile_id}")).ok().flatten()
}

pub(super) fn save_view_state(profile_id: &str, name: &str, value: &str) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.set_setting(&format!("{name}:{profile_id}"), value);
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
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.delete_collection(collection_id);
    }
}

pub(super) fn rename_collection(collection_id: &str, name: &str) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.rename_collection(collection_id, name);
    }
}

pub(super) fn set_collection_member(collection_id: &str, product_id: &str, member: bool) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.set_collection_member(collection_id, Storefront::Gog, product_id, member);
    }
}

pub(super) fn is_favorite(profile_id: &str, product_id: &str) -> bool {
    Db::open(achv_core::default_db_path())
        .and_then(|db| db.is_favorite(profile_id, Storefront::Gog, product_id))
        .unwrap_or(false)
}

pub(super) fn favorite_ids(profile_id: &str) -> std::collections::HashSet<String> {
    Db::open(achv_core::default_db_path())
        .and_then(|db| db.list_favorite_ids(profile_id, Storefront::Gog))
        .unwrap_or_default()
}

pub(super) fn set_favorite(profile_id: &str, product_id: &str, favorite: bool) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.set_favorite(profile_id, Storefront::Gog, product_id, favorite);
    }
}

/// Playtime and achievement counts for the whole library in two queries,
/// for sorting and filtering the sidebar — the per-game reads below would
/// mean hundreds of round trips on every keystroke.
pub(super) fn library_stats(profile_id: &str) -> LibraryStats {
    let Ok(db) = Db::open(achv_core::default_db_path()) else {
        return LibraryStats::default();
    };
    LibraryStats {
        playtime: db.all_playtime(profile_id, Storefront::Gog).unwrap_or_default(),
        achievements: db
            .all_achievement_counts(profile_id, Storefront::Gog)
            .unwrap_or_default(),
        added: db.all_first_seen(profile_id, Storefront::Gog).unwrap_or_default(),
        added_rank: db.all_library_ranks(profile_id, Storefront::Gog).unwrap_or_default(),
        // From the appmanifests, not the database: what's installed lives
        // on disk now.
        sizes: crate::core::installed(profile_id)
            .into_iter()
            .filter_map(|(game_id, installed)| {
                let Some(size) = installed.size_on_disk else {
                    return None;
                };
                Some((game_id, size))
            })
            .collect(),
    }
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
    /// `achv_core::Db::all_library_ranks`. The authoritative source when
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

pub(super) fn load_playtime(profile_id: &str, product_id: &str) -> Playtime {
    Db::open(achv_core::default_db_path())
        .and_then(|db| db.get_playtime(profile_id, Storefront::Gog, product_id))
        .unwrap_or_default()
}

pub(super) fn add_playtime(profile_id: &str, product_id: &str, seconds: i64) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.add_playtime(profile_id, Storefront::Gog, product_id, seconds);
    }
}
