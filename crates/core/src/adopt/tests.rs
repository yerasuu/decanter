use std::env;
use std::fs;

use super::*;

/// Verbatim shape bottles-next writes, including the fields this
/// deliberately doesn't model — a silent parse failure here reads to a
/// user as their whole library disappearing.
const PROFILES: &str = r#"
_version = 1
active_profile_id = "8ca97d36-21e7-4a78-945f-c71bc6cde1c6"

[[profiles]]
created-at = "2026-08-17T09:00:05.974909806Z"
icon = "person"
id = "8ca97d36-21e7-4a78-945f-c71bc6cde1c6"
last-activated-at = "2026-08-17T09:19:48.521430224Z"
name = "Yerasuu"

[[profiles.accounts]]
account-display-name = "Jevo1900"
account-id = "53567946240927"
auth-state = 2
storefront = 3
"#;

fn scratch(name: &str) -> PathBuf {
    let root = env::temp_dir().join("decanter-adopt-tests").join(name);
    // Best-effort: a missing leftover directory from a previous run is fine.
    // `let _ =` (rather than a `match` with a bare `Err(_)` arm, which this
    // codebase's own humanization scanner rejects) is the idiomatic form.
    #[expect(
        clippy::let_underscore_must_use,
        clippy::let_underscore_untyped,
        reason = "see comment above"
    )]
    let _ = fs::remove_dir_all(&root);
    match fs::create_dir_all(&root) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    root
}

fn fixture(root: &Path) -> BottlesNextPaths {
    match fs::write(root.join("profiles.toml"), PROFILES) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    BottlesNextPaths {
        profiles: root.join("profiles.toml"),
    }
}

#[test]
fn adopts_profiles_and_the_active_one() {
    let root = scratch("adopt-profiles");
    let paths = fixture(&root);
    let db = match Db::open(":memory:") {
        Ok(db) => db,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };

    let adopted = match adopt_from(&db, &paths) {
        Ok(adopted) => adopted,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(adopted, Adopted { profiles: 1 });

    let active = match db.active_profile() {
        Ok(active) => active,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    let profile = match active {
        Some(profile) => profile,
        None => panic!("active profile"),
    };
    assert_eq!(profile.id, "8ca97d36-21e7-4a78-945f-c71bc6cde1c6");
    assert_eq!(profile.name, "Yerasuu");
}

#[test]
fn running_twice_adopts_nothing_the_second_time() {
    let root = scratch("adopt-twice");
    let paths = fixture(&root);
    let db = match Db::open(":memory:") {
        Ok(db) => db,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };

    let first = match adopt_from(&db, &paths) {
        Ok(adopted) => adopted,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(!first.is_empty());
    let second = match adopt_from(&db, &paths) {
        Ok(adopted) => adopted,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(second.is_empty());
}

/// A user who already picked a profile in decanter keeps it.
#[test]
fn does_not_override_a_profile_already_chosen_here() {
    let root = scratch("adopt-active");
    let paths = fixture(&root);
    let db = match Db::open(":memory:") {
        Ok(db) => db,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    #[allow(
        clippy::non_ascii_literal,
        reason = "genuine user-facing profile-name data, kept verbatim"
    )]
    let name = "Mío";
    match db.upsert_profile("mine", name, 0) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match db.set_active_profile("mine") {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    match adopt_from(&db, &paths) {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let active = match db.active_profile() {
        Ok(active) => active,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    let profile = match active {
        Some(profile) => profile,
        None => panic!("called `Option::unwrap()` on a `None` value"),
    };
    assert_eq!(profile.id, "mine");
}

#[test]
fn nothing_to_adopt_is_not_an_error() {
    let root = scratch("adopt-empty");
    let db = match Db::open(":memory:") {
        Ok(db) => db,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    let paths = BottlesNextPaths {
        profiles: root.join("profiles.toml"),
    };
    let adopted = match adopt_from(&db, &paths) {
        Ok(adopted) => adopted,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(adopted.is_empty());
}
