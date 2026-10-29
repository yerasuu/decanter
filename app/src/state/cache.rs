//! Converts between `decanter-gog`'s live `LibraryEntry` and decanter-core's
//! `CachedGame` (what we persist), and reads/writes the cache itself —
//! plain sqlite, no UI state, so the fetch loop can call it from a spawned
//! task without touching UI concerns.

use decanter_core::{CachedGame, Db, Game};
use decanter_gog::{GameKind, LibraryEntry};

/// The cache stores the kind as text so decanter-core doesn't have to know
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
        background_url: entry.game.background_url.clone(),
        kind: kind_to_str(entry.kind).to_string(),
        purchased_at: entry.purchased_at,
        release_date: entry.game.release_date,
        rating: entry.game.rating.clone(),
        genres: entry.game.genres.clone(),
        library_rank: entry.library_rank,
        parent_id: entry.parent_id.clone(),
        installable: entry.installable,
        has_macos_build: entry.game.has_macos_build,
    }
}

/// Install state isn't cached — it's read live from decanter's own install
/// record, so a game the cache remembers still shows as installed on the
/// very first frame instead of flipping once the network fetch lands.
fn to_entry(library: Option<&decanter_core::Library>, account_id: &str, cached: CachedGame) -> LibraryEntry {
    LibraryEntry {
        kind: kind_from_str(&cached.kind),
        purchased_at: cached.purchased_at,
        library_rank: cached.library_rank,
        parent_id: cached.parent_id,
        installable: cached.installable,
        is_mod: false,
        game: Game {
            account_id: account_id.to_owned(),
            install_state: super::gog_library::local_install_state_in(library, account_id, &cached.product_id),
            id: cached.product_id,
            title: cached.title,
            storefront: cached.storefront,
            icon_url: cached.icon_url,
            cover_url: cached.cover_url,
            background_url: cached.background_url,
            release_date: cached.release_date,
            rating: cached.rating,
            genres: cached.genres,
            has_macos_build: cached.has_macos_build,
            ..Default::default()
        },
    }
}

/// Whether this account has ever been synced before — the coarse,
/// once-per-fetch-cycle signal [`super::gog_library`]'s fetch loop needs to
/// tell a genuine new purchase (this id is new, and the account isn't)
/// apart from a first-ever sync, where every id is new to the cache and
/// none of that is evidence any particular game was just added. Defaults
/// to `true` (behave as if already synced) when the cache can't even be
/// opened, since in that case nothing here can be trusted anyway.
pub(super) fn has_synced_before(account_id: &str) -> bool {
    Db::open_active()
        .and_then(|db| db.has_games(account_id))
        .inspect_err(|err| {
            tracing::warn!(account_id, %err, "could not check prior sync state, assuming already synced");
        })
        .unwrap_or(true)
}

/// Whatever was cached from the last session — shown immediately on
/// startup so the sidebar isn't empty while a fresh fetch re-scans the
/// account.
pub(super) fn load_cached_games(account_id: &str) -> Vec<LibraryEntry> {
    let Ok(db) = Db::open_active() else {
        tracing::warn!(account_id, "could not open cache db, starting with an empty library");
        return Vec::new();
    };
    let cached = db.list_games(account_id).unwrap_or_else(|err| {
        tracing::warn!(account_id, %err, "could not read cached games, starting with an empty library");
        Vec::new()
    });
    tracing::debug!(account_id, count = cached.len(), "loaded cached games");
    let library = crate::core::library_for_account(account_id).ok();
    cached
        .into_iter()
        .map(|cached| to_entry(library.as_ref(), account_id, cached))
        .collect()
}

/// Best-effort — a cache write failing shouldn't interrupt the fetch, the
/// in-memory list is still correct either way.
///
/// `confirmed_new` is the account-wide "have we synced this account
/// before" flag from [`super::gog_library`] — see [`decanter_core::Db::upsert_game`]
/// for why a single game entry can't answer that for itself.
pub(super) fn save_entries(account_id: &str, entries: &[LibraryEntry], confirmed_new: bool) {
    let db = match Db::open_active() {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(account_id, %err, "could not open cache db to save game entries");
            return;
        }
    };
    for entry in entries {
        if let Err(err) = db.upsert_game(account_id, &to_cached(entry), confirmed_new) {
            tracing::warn!(account_id, product_id = %entry.game.id, %err, "failed to cache game entry");
        }
    }
}
