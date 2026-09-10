use super::*;

#[test]
fn builds_an_image_url_from_a_scheme_less_path() {
    assert_eq!(
        image_url("//images-1.gog-statics.com/abc123").as_deref(),
        Some("https://images-1.gog-statics.com/abc123.jpg")
    );
    assert_eq!(image_url("  "), None);
}

/// The real shape of a `description.lead`, tags and newlines included.
#[test]
fn strips_the_markup_out_of_a_real_description() {
    let html = "<b>Brigador: Deluxe DLC Upgrade includes the following bonus content:</b><br>\n- OST Volume I<br>\n- Audiobook\n<br><br>\n";
    assert_eq!(
        html_to_text(html),
        "Brigador: Deluxe DLC Upgrade includes the following bonus content: - OST Volume I - Audiobook"
    );
}

/// A tag must separate the words it sits between, not glue them.
#[test]
fn a_tag_boundary_is_a_word_boundary() {
    assert_eq!(html_to_text("one<br>two"), "one two");
    assert_eq!(html_to_text("<p>alone</p>"), "alone");
}

#[test]
fn unescapes_the_entities_gog_uses() {
    assert_eq!(
        html_to_text("Tom &amp; Jerry&#39;s &quot;best&quot;"),
        "Tom & Jerry's \"best\""
    );
}

#[test]
fn plain_text_survives_untouched() {
    assert_eq!(html_to_text("just a sentence"), "just a sentence");
    assert_eq!(html_to_text(""), "");
}
