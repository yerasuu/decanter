use super::extract_code;

#[test]
fn reads_the_code_out_of_a_pasted_redirect_url() {
    assert_eq!(
        extract_code("https://embed.gog.com/on_login_success?origin=client&code=abc123"),
        Some("abc123".to_string())
    );
}

#[test]
fn accepts_a_bare_code() {
    assert_eq!(extract_code("  abc123 "), Some("abc123".to_string()));
}

#[test]
fn rejects_a_url_with_no_code() {
    assert_eq!(
        extract_code("https://embed.gog.com/on_login_success?origin=client"),
        None
    );
}

#[test]
fn rejects_empty_input() {
    assert_eq!(extract_code("   "), None);
}
