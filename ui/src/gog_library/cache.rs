//! Converts between `achv-gog`'s live `LibraryEntry` and achv-core's
//! `CachedGame` (what we persist), and reads/writes the cache itself —
//! plain sqlite, no Dioxus, so the fetch loop can call it from a spawned
//! task without touching UI concerns.

use achv_core::{CachedGame, Db, Game};
use achv_gog::{GameKind, LibraryEntry};

/// The cache stores the kind as text so achv-core doesn't have to know
/// about anyone's enum; these two are the only place that mapping lives.
const fn kind_to_str(kind: GameKind) -> &'static str {
    match kind {
        GameKind::Game => "game",
        GameKind::Movie => "movie",
        GameKind::Extra => "extra",
    }
}

fn kind_from_str(value: &str) -> GameKind {
    match value {
        "movie" => GameKind::Movie,
        "extra" => GameKind::Extra,
        _ => GameKind::Game,
    }
}

fn to_cached(entry: &LibraryEntry) -> CachedGame {
    CachedGame {
        storefront: entry.game.storefront,
        product_id: entry.game.id.clone(),
        title: entry.game.title.clone(),
        icon_url: entry.game.icon_url.clone(),
        cover_url: entry.game.cover_url.clone(),
        kind: kind_to_str(entry.kind).to_string(),
        purchased_at: entry.purchased_at,
        release_date: entry.game.release_date,
        rating: entry.game.rating.clone(),
        genres: entry.game.genres.clone(),
        library_rank: entry.library_rank,
        parent_id: entry.parent_id.clone(),
        installable: entry.installable,
    }
}

/// Install state isn't cached — it's read live from achv's own install
/// record, so a game the cache remembers still shows as installed on the
/// very first frame instead of flipping once the network fetch lands.
fn to_entry(profile_id: &str, cached: CachedGame) -> LibraryEntry {
    LibraryEntry {
        kind: kind_from_str(&cached.kind),
        purchased_at: cached.purchased_at,
        library_rank: cached.library_rank,
        parent_id: cached.parent_id,
        installable: cached.installable,
        game: Game {
            install_state: super::local_install_state(profile_id, &cached.product_id),
            id: cached.product_id,
            title: cached.title,
            storefront: cached.storefront,
            icon_url: cached.icon_url,
            cover_url: cached.cover_url,
            release_date: cached.release_date,
            rating: cached.rating,
            genres: cached.genres,
            ..Default::default()
        },
    }
}

/// Whether this profile has ever been synced before — the coarse,
/// once-per-fetch-cycle signal [`super::stream::watch_games`] needs to
/// tell a genuine new purchase (this id is new, and the profile isn't)
/// apart from a first-ever sync, where every id is new to the cache and
/// none of that is evidence any particular game was just added. Defaults
/// to `true` (behave as if already synced) when the cache can't even be
/// opened, since in that case nothing here can be trusted anyway.
pub(super) fn has_synced_before(profile_id: &str) -> bool {
    Db::open(achv_core::default_db_path())
        .and_then(|db| db.has_games(profile_id))
        .unwrap_or(true)
}

/// Whatever was cached from the last session — shown immediately on
/// startup so the sidebar isn't empty while a fresh fetch re-scans the
/// account.
pub(super) fn load_cached_games(profile_id: &str) -> Vec<LibraryEntry> {
    let Ok(db) = Db::open(achv_core::default_db_path()) else {
        return Vec::new();
    };
    db.list_games(profile_id)
        .unwrap_or_default()
        .into_iter()
        .map(|cached| to_entry(profile_id, cached))
        .collect()
}

/// Best-effort — a cache write failing shouldn't interrupt the fetch, the
/// in-memory list is still correct either way.
///
/// `confirmed_new` is the profile-wide "have we synced this account
/// before" flag from [`super::stream`] — see [`achv_core::Db::upsert_game`]
/// for why a single game entry can't answer that for itself.
pub(super) fn save_entry(profile_id: &str, entry: &LibraryEntry, confirmed_new: bool) {
    if let Ok(db) = Db::open(achv_core::default_db_path()) {
        let _ = db.upsert_game(profile_id, &to_cached(entry), confirmed_new);
    }
}
