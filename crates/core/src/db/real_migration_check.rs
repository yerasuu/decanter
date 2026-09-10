use std::env;

use super::Db;

// `clippy::implicit_return` and `clippy::needless_return` (part of
// `clippy::all`) disagree on the closures below -- natural style wins.
/// Not run by default (`cargo test` for the crate proper doesn't
/// touch anything outside a `:memory:` db). Opt in with
/// `cargo test -p achv-core -- --ignored` to sanity-check the
/// migration against a real, pre-existing `links.db` copied in first.
#[test]
#[ignore = "requires ACHV_MIGRATION_CHECK_DB pointing at a real, pre-existing links.db"]
fn migrates_a_real_pre_release_date_db_without_losing_rows() {
    let path = match env::var("ACHV_MIGRATION_CHECK_DB") {
        Ok(value) => value,
        Err(err) => panic!("set ACHV_MIGRATION_CHECK_DB: {err:?}"),
    };
    let conn = match rusqlite::Connection::open(&path) {
        Ok(conn) => conn,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    let before: i64 = match conn.query_row("SELECT COUNT(*) FROM games_cache", [], |row| row.get(0)) {
        Ok(count) => count,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };

    let db = match Db::open(&path) {
        Ok(db) => db,
        Err(err) => panic!("migration should not fail on a real db: {err:?}"),
    };
    let after: i64 = match db
        .conn
        .query_row("SELECT COUNT(*) FROM games_cache", [], |row| row.get(0))
    {
        Ok(count) => count,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };

    assert_eq!(before, after, "row count changed across migration");
}
