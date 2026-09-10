use super::{bytes, eta, rate};

#[test]
fn scales_to_a_readable_unit() {
    assert_eq!(bytes(717), "717 B");
    assert_eq!(bytes(1024), "1.0 KB");
    assert_eq!(bytes(5_566_295_753), "5.2 GB");
}

#[test]
fn a_stalled_transfer_has_no_rate_or_eta() {
    assert_eq!(rate(0.0), "—");
    assert_eq!(eta(1024, 0.0), None);
}

#[test]
fn nothing_left_to_download_has_no_eta() {
    assert_eq!(eta(0, 1024.0), None);
}

#[test]
fn formats_time_left() {
    assert_eq!(eta(1024, 1024.0).unwrap(), "1s");
    assert_eq!(eta(120 * 1024, 1024.0).unwrap(), "2m 0s");
    assert_eq!(eta(7200 * 1024, 1024.0).unwrap(), "2h 0m");
}
