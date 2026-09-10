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
fn lists_accounts_of_a_profile_in_the_order_they_were_added() {
    let db = db();
    match db.upsert_account("2", Storefront::Gog, "p", "Segunda", 2) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_account("1", Storefront::Gog, "p", "Primera", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let accounts = match db.accounts_in_profile("p") {
        Ok(accounts) => accounts,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(
        accounts.iter().map(|account| account.name.as_str()).collect::<Vec<_>>(),
        vec!["Primera", "Segunda"]
    );
}

/// Two profiles can each hold their own accounts without seeing the other's.
#[test]
fn accounts_stay_scoped_to_their_profile() {
    let db = db();
    match db.upsert_account("1", Storefront::Gog, "p1", "Cuenta uno", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_account("2", Storefront::Gog, "p2", "Cuenta dos", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let p1_accounts = match db.accounts_in_profile("p1") {
        Ok(accounts) => accounts,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(
        p1_accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        vec!["1"]
    );
}

/// A profile can hold several accounts of the same storefront — the whole
/// point of grouping accounts under a profile instead of one-per-storefront.
#[test]
fn a_profile_can_hold_several_accounts_of_the_same_storefront() {
    let db = db();
    match db.upsert_account("1", Storefront::Gog, "p", "Primera", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_account("2", Storefront::Gog, "p", "Segunda", 2) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let accounts = match db.accounts_in_profile("p") {
        Ok(accounts) => accounts,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(accounts.len(), 2);
}

#[test]
fn re_adding_the_same_account_renames_and_can_move_it_to_another_profile() {
    let db = db();
    match db.upsert_account("1", Storefront::Gog, "p1", "Vieja", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.upsert_account("1", Storefront::Gog, "p2", "Nueva", 99) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    let p1_accounts = match db.accounts_in_profile("p1") {
        Ok(accounts) => accounts,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(p1_accounts.is_empty());

    let p2_accounts = match db.accounts_in_profile("p2") {
        Ok(accounts) => accounts,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(
        p2_accounts
            .iter()
            .map(|account| account.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Nueva"]
    );
}

#[test]
fn forgetting_an_account_removes_it_from_its_profile() {
    let db = db();
    match db.upsert_account("1", Storefront::Gog, "p", "Primera", 1) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.forget_account("1", Storefront::Gog) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let accounts = match db.accounts_in_profile("p") {
        Ok(accounts) => accounts,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(accounts.is_empty());
}
