use super::*;

/// The account name is what makes a bottles-next session findable, so
/// it is pinned rather than left to whatever `Display` does.
#[test]
fn account_matches_the_name_bottles_next_writes() {
    assert_eq!(
        account("8ca97d36-21e7-4a78-945f-c71bc6cde1c6", Storefront::Gog),
        "8ca97d36-21e7-4a78-945f-c71bc6cde1c6/STOREFRONT_GOG"
    );
}
