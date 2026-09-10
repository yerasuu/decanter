use super::product_image;

#[test]
fn builds_a_sized_image_url_from_a_protocol_less_path() {
    assert_eq!(
        product_image("//images.gog.com/abc123", "_196").unwrap(),
        "https://images.gog.com/abc123_196.jpg"
    );
}

#[test]
fn tolerates_a_path_without_the_leading_slashes() {
    assert_eq!(
        product_image("images.gog.com/abc123", "_392").unwrap(),
        "https://images.gog.com/abc123_392.jpg"
    );
}

#[test]
fn has_no_url_without_an_image() {
    assert_eq!(product_image("", "_196"), None);
    assert_eq!(product_image("   ", "_196"), None);
}
