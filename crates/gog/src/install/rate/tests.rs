use super::*;

#[test]
fn reports_nothing_before_the_first_interval_has_passed() {
    let mut tracker = RateTracker::new();
    assert_eq!(tracker.sample(1_000_000), 0.0);
}

#[test]
fn averages_over_the_full_tracked_interval_not_a_sub_slice_of_it() {
    let mut tracker = RateTracker::new();
    tracker.last_sample_at = Instant::now() - Duration::from_secs(1);
    tracker.last_sample_bytes = 0;

    // The upstream bug this exists to avoid divides a file's whole
    // size by a microscopic sliver of time — as if it had all
    // arrived just before completion. Dividing by the real elapsed
    // interval instead should land close to the true average, not
    // near whatever inflated number that sliver would produce.
    let rate = tracker.sample(500 * 1024 * 1024);
    assert!(
        (rate - 500.0 * 1024.0 * 1024.0).abs() < 1.0 * 1024.0 * 1024.0,
        "expected roughly 500 MiB/s over the full second, got {rate}"
    );
}
