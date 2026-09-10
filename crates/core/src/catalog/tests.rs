#![expect(
    clippy::panic,
    reason = "unwrap_used/expect_used push every fallible call in a test toward panic!, \
              which clippy::panic then flags right back"
)]
#![expect(
    clippy::separated_literal_suffix,
    reason = "unseparated_literal_suffix bans the opposite form on the same integer literals below"
)]

use super::*;

/// These integers are in the database. A change here silently re-keys
/// every cached game, achievement and install record.
#[test]
fn discriminants_match_what_is_already_persisted() {
    assert_eq!(Storefront::Unspecified as i32, 0_i32);
    assert_eq!(Storefront::Steam as i32, 1_i32);
    assert_eq!(Storefront::EpicGames as i32, 2_i32);
    assert_eq!(Storefront::Gog as i32, 3_i32);
    assert_eq!(Storefront::AmazonGames as i32, 4_i32);
    assert_eq!(Storefront::EaApp as i32, 5_i32);
    assert_eq!(Storefront::UbisoftConnect as i32, 6_i32);
    assert_eq!(Storefront::BattleNet as i32, 7_i32);
}

#[test]
fn round_trips_through_its_integer_form() {
    for storefront in [
        Storefront::Unspecified,
        Storefront::Steam,
        Storefront::EpicGames,
        Storefront::Gog,
        Storefront::AmazonGames,
        Storefront::EaApp,
        Storefront::UbisoftConnect,
        Storefront::BattleNet,
    ] {
        let round_tripped = match Storefront::try_from(storefront as i32) {
            Ok(value) => value,
            Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
        };
        assert_eq!(round_tripped, storefront);
    }
}

#[test]
fn rejects_an_integer_no_storefront_uses() {
    if let Ok(value) = Storefront::try_from(99_i32) {
        panic!("expected an error, got {value:?}");
    }
    if let Ok(value) = Storefront::try_from(-1_i32) {
        panic!("expected an error, got {value:?}");
    }
}

/// Install roots on disk and keyring accounts are already spelled this
/// way; renaming one orphans everything written under the old name.
#[test]
fn names_match_the_ones_already_written_to_disk() {
    assert_eq!(Storefront::Gog.as_str_name(), "STOREFRONT_GOG");
    assert_eq!(Storefront::EpicGames.as_str_name(), "STOREFRONT_EPIC_GAMES");
}
