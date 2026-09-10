#![expect(
    clippy::panic,
    reason = "unwrap_used/expect_used push every fallible call in a test toward panic!, \
              which clippy::panic then flags right back"
)]

use super::*;

fn db() -> Db {
    match Db::open(":memory:") {
        Ok(db) => db,
        Err(err) => panic!("in-memory db: {err:?}"),
    }
}

#[test]
fn lists_profiles_in_the_order_they_were_added() {
    let db = db();
    match db.upsert_profile("b", "Segundo", 2) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_profile("a", "Primero", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let profiles = match db.list_profiles() {
        Ok(profiles) => profiles,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(
        profiles.iter().map(|profile| profile.name.as_str()).collect::<Vec<_>>(),
        vec!["Primero", "Segundo"]
    );
}

#[test]
fn re_adding_renames_without_reordering() {
    let db = db();
    match db.upsert_profile("a", "Primero", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_profile("b", "Segundo", 2) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_profile("a", "Renombrado", 99) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    let profiles = match db.list_profiles() {
        Ok(profiles) => profiles,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    let names: Vec<String> = profiles.into_iter().map(|profile| profile.name).collect();
    assert_eq!(names, vec!["Renombrado", "Segundo"]);
}

#[test]
fn activating_round_trips() {
    let db = db();
    let active_before = match db.active_profile() {
        Ok(active) => active,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(active_before, None);
    let profile = match db.upsert_profile("a", "Primero", 1) {
        Ok(profile) => profile,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    match db.set_active_profile("a") {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let active_after = match db.active_profile() {
        Ok(active) => active,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(active_after, Some(profile));
}

/// A stale pointer reads as "nobody selected" rather than handing back
/// a profile that no longer exists.
#[test]
fn an_active_profile_that_was_deleted_reads_as_none() {
    let db = db();
    match db.upsert_profile("a", "Primero", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.set_active_profile("a") {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.delete_profile("a") {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let active = match db.active_profile() {
        Ok(active) => active,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(active, None);
}
