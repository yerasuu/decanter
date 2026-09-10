use super::*;

/// The account name is a stable keyring lookup key, so its shape is
/// pinned rather than left to whatever `Display` does.
#[test]
fn account_name_has_the_expected_shape() {
    assert_eq!(
        account("8ca97d36-21e7-4a78-945f-c71bc6cde1c6", Storefront::Gog),
        "8ca97d36-21e7-4a78-945f-c71bc6cde1c6/STOREFRONT_GOG"
    );
}
