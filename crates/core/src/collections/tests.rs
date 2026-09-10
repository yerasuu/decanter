#![expect(
    clippy::panic,
    reason = "unwrap_used/expect_used push every fallible call in a test toward panic!, \
              which clippy::panic then flags right back"
)]

use std::collections::HashSet;

use super::*;

fn db() -> Db {
    match Db::open(":memory:") {
        Ok(db) => db,
        Err(err) => panic!("in-memory db: {err:?}"),
    }
}

fn expect_ok<T, E>(result: std::result::Result<T, E>) -> T
where
    E: std::fmt::Debug,
{
    match result {
        Ok(value) => value,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
}

#[test]
fn created_collections_list_in_creation_order() {
    let db = db();
    let first = expect_ok(db.create_collection("p1", "Favoritas"));
    let second = expect_ok(db.create_collection("p1", "Para despues"));

    let listed = expect_ok(db.list_collections("p1"));
    assert_eq!(listed, vec![first, second]);
}

#[test]
fn a_fresh_collection_is_not_dynamic() {
    let db = db();
    let collection = expect_ok(db.create_collection("p1", "Favoritas"));
    assert_eq!(collection.filter, None);
}

#[test]
fn setting_a_filter_makes_a_collection_dynamic() {
    let db = db();
    let collection = expect_ok(db.create_collection("p1", "RPGs"));
    expect_ok(db.set_collection_filter(&collection.id, Some("{\"kind\":\"all\"}")));

    let listed = expect_ok(db.list_collections("p1"));
    assert_eq!(listed[0].filter.as_deref(), Some("{\"kind\":\"all\"}"));
}

#[test]
fn renaming_keeps_the_same_id_and_position() {
    let db = db();
    let collection = expect_ok(db.create_collection("p1", "Original"));
    expect_ok(db.rename_collection(&collection.id, "Renombrada"));

    let listed = expect_ok(db.list_collections("p1"));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, collection.id);
    assert_eq!(listed[0].name, "Renombrada");
}

#[test]
fn membership_round_trips_and_is_scoped_by_storefront() {
    let db = db();
    let collection = expect_ok(db.create_collection("p1", "Favoritas"));
    expect_ok(db.set_collection_member(&collection.id, Storefront::Gog, "g1", true));
    expect_ok(db.set_collection_member(&collection.id, Storefront::Gog, "g2", true));

    let members = expect_ok(db.collection_members("p1", Storefront::Gog));
    assert_eq!(
        members.get(&collection.id).cloned().unwrap_or_default(),
        HashSet::from(["g1".to_owned(), "g2".to_owned()])
    );

    expect_ok(db.set_collection_member(&collection.id, Storefront::Gog, "g1", false));
    let members = expect_ok(db.collection_members("p1", Storefront::Gog));
    assert_eq!(
        members.get(&collection.id).cloned().unwrap_or_default(),
        HashSet::from(["g2".to_owned()])
    );
}

#[test]
fn deleting_a_collection_drops_its_membership_too() {
    let db = db();
    let collection = expect_ok(db.create_collection("p1", "Temporal"));
    expect_ok(db.set_collection_member(&collection.id, Storefront::Gog, "g1", true));

    expect_ok(db.delete_collection(&collection.id));

    assert!(expect_ok(db.list_collections("p1")).is_empty());
    assert!(expect_ok(db.collection_members("p1", Storefront::Gog)).is_empty());
}

#[test]
fn moving_a_collection_later_shifts_the_ones_between() {
    let db = db();
    let a = expect_ok(db.create_collection("p1", "A"));
    let b = expect_ok(db.create_collection("p1", "B"));
    let c = expect_ok(db.create_collection("p1", "C"));

    expect_ok(db.move_collection("p1", &a.id, 1));

    let listed = expect_ok(db.list_collections("p1"));
    assert_eq!(
        listed.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        vec![b.id, a.id, c.id]
    );
}

#[test]
fn moving_past_either_end_clamps_instead_of_erroring() {
    let db = db();
    let a = expect_ok(db.create_collection("p1", "A"));
    let b = expect_ok(db.create_collection("p1", "B"));

    expect_ok(db.move_collection("p1", &b.id, -50));
    let listed = expect_ok(db.list_collections("p1"));
    assert_eq!(
        listed.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        vec![b.id.clone(), a.id.clone()]
    );

    expect_ok(db.move_collection("p1", &b.id, 50));
    let listed = expect_ok(db.list_collections("p1"));
    assert_eq!(
        listed.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
}

#[test]
fn hiding_round_trips_and_is_scoped_by_profile() {
    let db = db();
    expect_ok(db.set_hidden("p1", Storefront::Gog, "g1", true));

    assert_eq!(
        expect_ok(db.hidden_ids("p1", Storefront::Gog)),
        HashSet::from(["g1".to_owned()])
    );
    assert!(expect_ok(db.hidden_ids("p2", Storefront::Gog)).is_empty());

    expect_ok(db.set_hidden("p1", Storefront::Gog, "g1", false));
    assert!(expect_ok(db.hidden_ids("p1", Storefront::Gog)).is_empty());
}
