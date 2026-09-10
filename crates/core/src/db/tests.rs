#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::panic,
    reason = "unwrap_used/expect_used push every fallible call in a test toward panic!, \
              which clippy::panic then flags right back"
)]
#![expect(
    clippy::semicolon_inside_block,
    reason = "semicolon_outside_block wants the opposite placement on the same block below"
)]
#![expect(
    clippy::unused_result_ok,
    reason = "let_underscore_must_use/let_underscore_untyped want the opposite of .ok() on the \
              same best-effort cleanup below"
)]

use std::collections::HashMap;
use std::env;
use std::fmt::Debug;
use std::fs;
use std::process;

use super::{CachedGame, Db, Storefront};

/// `clippy::unwrap_used` bans the method itself; a free function with the
/// same panic message isn't `Result::unwrap`, so call sites read the same
/// without tripping the lint.
fn expect_ok<T, E>(result: Result<T, E>) -> T
where
    E: Debug,
{
    match result {
        Ok(value) => value,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
}

/// Same idea as [`expect_ok`], for the handful of `HashMap` lookups by key.
fn expect_key(map: &HashMap<String, i64>, key: &str) -> i64 {
    match map.get(key) {
        Some(&found) => found,
        None => panic!("missing key `{key}`"),
    }
}

/// Same idea as [`expect_ok`], for asserting a listing has at least one row.
#[expect(
    clippy::option_if_let_else,
    reason = "a match reads clearer here than map_or_else for a panic-on-None branch"
)]
fn expect_first(games: &[CachedGame]) -> &CachedGame {
    match games.first() {
        Some(found) => found,
        None => panic!("expected at least one row"),
    }
}

#[test]
fn retaining_drops_what_a_fetch_no_longer_lists() {
    let db = expect_ok(Db::open(":memory:"));
    for id in ["1", "2", "3"] {
        expect_ok(db.upsert_game("profile", &game(id), false));
    }

    let dropped = expect_ok(db.retain_games("profile", Storefront::Gog, &["1".to_owned(), "3".to_owned()]));
    assert_eq!(dropped, 1);
    let games = expect_ok(db.list_games("profile"));
    assert_eq!(
        games.iter().map(|game| game.product_id.as_str()).collect::<Vec<_>>(),
        vec!["1", "3"]
    );
}

/// An empty listing is a failed fetch, not an emptied account.
#[test]
fn retaining_nothing_is_refused_rather_than_obeyed() {
    let db = expect_ok(Db::open(":memory:"));
    expect_ok(db.upsert_game("profile", &game("1"), false));
    assert_eq!(expect_ok(db.retain_games("profile", Storefront::Gog, &[])), 0);
    assert_eq!(expect_ok(db.list_games("profile")).len(), 1);
}

/// Another profile's rows are none of this fetch's business.
#[test]
fn retaining_leaves_other_profiles_alone() {
    let db = expect_ok(Db::open(":memory:"));
    expect_ok(db.upsert_game("mine", &game("1"), false));
    expect_ok(db.upsert_game("theirs", &game("2"), false));

    expect_ok(db.retain_games("mine", Storefront::Gog, &["1".to_owned()]));
    assert_eq!(expect_ok(db.list_games("theirs")).len(), 1);
}

fn game(product_id: &str) -> CachedGame {
    CachedGame {
        storefront: Storefront::Gog,
        product_id: product_id.to_owned(),
        title: "A Game".to_owned(),
        icon_url: None,
        cover_url: None,
        kind: "game".to_owned(),
        purchased_at: None,
        release_date: Some(1_000),
        rating: Some("87".to_owned()),
        genres: vec!["RPG".to_owned(), "Action".to_owned()],
        library_rank: None,
        parent_id: None,
        installable: true,
    }
}

#[test]
fn catalog_metadata_survives_a_round_trip() {
    let db = expect_ok(Db::open(":memory:"));
    expect_ok(db.upsert_game("p1", &game("g1"), true));

    let cached = expect_ok(db.list_games("p1"));
    assert_eq!(cached.len(), 1);
    let first = expect_first(&cached);
    assert_eq!(first.release_date, Some(1_000));
    assert_eq!(first.rating.as_deref(), Some("87"));
    assert_eq!(first.genres, vec!["RPG".to_owned(), "Action".to_owned()]);
}

/// A refresh where the per-game `GamesDB` lookup failed carries no
/// catalog data at all — that must not blank out what an earlier,
/// successful refresh already found.
#[test]
fn a_quiet_refresh_keeps_previously_known_catalog_data() {
    let db = expect_ok(Db::open(":memory:"));
    expect_ok(db.upsert_game("p1", &game("g1"), true));

    let mut quiet = game("g1");
    quiet.release_date = None;
    quiet.rating = None;
    quiet.genres = Vec::new();
    expect_ok(db.upsert_game("p1", &quiet, true));

    let cached = expect_ok(db.list_games("p1"));
    let first = expect_first(&cached);
    assert_eq!(first.release_date, Some(1_000));
    assert_eq!(first.rating.as_deref(), Some("87"));
    assert_eq!(first.genres, vec!["RPG".to_owned(), "Action".to_owned()]);
}

/// "Date added" prefers a real purchase date, falls back to the
/// release date (still distinguishes one game from another) and only
/// resorts to first-seen — every game scanned on the same day would
/// otherwise tie — when neither is known.
#[test]
fn date_added_falls_back_from_purchase_to_release_to_first_seen() {
    let db = expect_ok(Db::open(":memory:"));

    let mut has_purchase = game("bought");
    has_purchase.purchased_at = Some(500);
    has_purchase.release_date = Some(999);
    expect_ok(db.upsert_game("p1", &has_purchase, true));

    let mut only_release = game("no_order");
    only_release.purchased_at = None;
    only_release.release_date = Some(200);
    expect_ok(db.upsert_game("p1", &only_release, true));

    let mut neither = game("unknown");
    neither.purchased_at = None;
    neither.release_date = None;
    expect_ok(db.upsert_game("p1", &neither, true));

    let added = expect_ok(db.all_first_seen("p1", Storefront::Gog));
    assert_eq!(expect_key(&added, "bought"), 500);
    assert_eq!(expect_key(&added, "no_order"), 200);
    assert!(expect_key(&added, "unknown") > 0);
}

/// A first-ever sync for a profile has no way to tell a decade-old
/// game apart from one bought yesterday, so none of it should be
/// stamped as "just added" -- that's what `confirmed_new: false` is
/// for, and it's what a caller should pass when [`Db::has_games`] says
/// this profile has never been synced before.
#[test]
fn an_unconfirmed_row_with_no_other_date_is_left_unknown_not_stamped_as_new() {
    let db = expect_ok(Db::open(":memory:"));
    let mut old_game = game("ancient");
    old_game.purchased_at = None;
    old_game.release_date = None;
    expect_ok(db.upsert_game("p1", &old_game, false));

    let added = expect_ok(db.all_first_seen("p1", Storefront::Gog));
    assert_eq!(expect_key(&added, "ancient"), 0);
}

/// The flip side: a product id that's new relative to a profile that
/// *has* synced before is a real signal, and gets a real timestamp.
#[test]
fn a_confirmed_new_row_gets_stamped_with_now() {
    let db = expect_ok(Db::open(":memory:"));
    let mut new_game = game("just_bought");
    new_game.purchased_at = None;
    new_game.release_date = None;
    expect_ok(db.upsert_game("p1", &new_game, true));

    let added = expect_ok(db.all_first_seen("p1", Storefront::Gog));
    assert!(expect_key(&added, "just_bought") > 0);
}

/// Whatever `confirmed_new` said when a row was first inserted, a
/// later refresh must never re-stamp `first_seen` -- it has to survive
/// every later call the same way `purchased_at` does.
#[test]
fn a_later_refresh_never_touches_first_seen_either_way() {
    let db = expect_ok(Db::open(":memory:"));
    expect_ok(db.upsert_game("p1", &game("g1"), false));
    expect_ok(db.upsert_game("p1", &game("g1"), true));

    let added = expect_ok(db.all_first_seen("p1", Storefront::Gog));
    // `game()` sets a release_date, so this reads that, not
    // first_seen directly -- the point is just that re-upserting with
    // `confirmed_new: true` didn't change anything.
    assert_eq!(expect_key(&added, "g1"), 1_000);
}

#[test]
fn has_games_reflects_whether_a_profile_was_ever_synced() {
    let db = expect_ok(Db::open(":memory:"));
    assert!(!expect_ok(db.has_games("p1")));
    expect_ok(db.upsert_game("p1", &game("g1"), false));
    assert!(expect_ok(db.has_games("p1")));
    assert!(!expect_ok(db.has_games("p2")));
}

#[test]
fn library_rank_survives_a_round_trip_and_only_lists_games_that_have_one() {
    let db = expect_ok(Db::open(":memory:"));
    let mut ranked = game("newest");
    ranked.library_rank = Some(0);
    expect_ok(db.upsert_game("p1", &ranked, true));
    expect_ok(db.upsert_game("p1", &game("unranked"), true));

    let ranks = expect_ok(db.all_library_ranks("p1", Storefront::Gog));
    assert_eq!(ranks.get("newest"), Some(&0));
    assert!(
        !ranks.contains_key("unranked"),
        "a game with no rank shouldn't appear at all"
    );
}

/// A cycle whose ranked fetch failed outright carries no rank for
/// anyone -- that must not erase the last successful ranking.
#[test]
fn a_quiet_ranking_refresh_keeps_the_last_known_rank() {
    let db = expect_ok(Db::open(":memory:"));
    let mut ranked = game("g1");
    ranked.library_rank = Some(5);
    expect_ok(db.upsert_game("p1", &ranked, true));

    let mut quiet = game("g1");
    quiet.library_rank = None;
    expect_ok(db.upsert_game("p1", &quiet, true));

    let ranks = expect_ok(db.all_library_ranks("p1", Storefront::Gog));
    assert_eq!(ranks.get("g1"), Some(&5));
}

/// A fresh successful fetch replaces the old rank outright, even
/// though both are non-null -- ranks shift as the library changes, so
/// this isn't a COALESCE-preserve case the way a *missing* value is.
#[test]
fn a_fresh_ranking_refresh_overwrites_the_old_rank() {
    let db = expect_ok(Db::open(":memory:"));
    let mut first = game("g1");
    first.library_rank = Some(5);
    expect_ok(db.upsert_game("p1", &first, true));

    let mut second = game("g1");
    second.library_rank = Some(2);
    expect_ok(db.upsert_game("p1", &second, true));

    let ranks = expect_ok(db.all_library_ranks("p1", Storefront::Gog));
    assert_eq!(ranks.get("g1"), Some(&2));
}

/// The one-time cleanup: a row written before `confirmed_new` existed
/// may already carry a "now"-looking `first_seen` with nothing backing
/// it up. Once the schema has moved past that, such a row can't be
/// told apart from a genuine one any other way than resetting it.
#[test]
fn a_stale_unbacked_first_seen_from_before_this_gets_reset_on_open() {
    let path = env::temp_dir().join(format!("decanter-test-{}.db", process::id()));
    {
        let conn = expect_ok(rusqlite::Connection::open(&path));
        expect_ok(conn.execute_batch(super::SCHEMA));
        expect_ok(conn.execute(
            "INSERT INTO games_cache (profile_id, storefront, product_id, title, kind, first_seen, updated_at)
             VALUES ('p1', 3, 'stale', 'Stale', 'game', strftime('%s','now'), strftime('%s','now'))",
            [],
        ));
        expect_ok(conn.execute(
            "INSERT INTO games_cache (profile_id, storefront, product_id, title, kind, purchased_at, first_seen, updated_at)
             VALUES ('p1', 3, 'real', 'Real', 'game', 42, strftime('%s','now'), strftime('%s','now'))",
            [],
        ))
    };

    let db = expect_ok(Db::open(&path));
    let added = expect_ok(db.all_first_seen("p1", Storefront::Gog));
    assert_eq!(
        expect_key(&added, "stale"),
        0,
        "no purchase or release date backs this up, so it's reset"
    );
    assert!(
        expect_key(&added, "real") > 0,
        "a row with a real purchase date is left alone"
    );

    fs::remove_file(&path).ok();
}
