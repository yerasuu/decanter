/// `active()`/`cancel_requested()` are process-wide statics, so each
/// test below uses its own fake (profile, product) key -- sharing one
/// with another test would make them interfere when run in parallel.
fn fake_key(label: &str) -> super::InstallKey {
    (format!("test-profile-{label}"), format!("test-product-{label}"))
}

/// The gap this covers: the UI shows a Cancelar button the moment an
/// install is queued, before `install` has enqueued anything for
/// `cancel` to reach through `active()`. Without recording the
/// request somewhere, that click would silently do nothing and the
/// install would finish anyway.
#[tokio::test]
async fn cancel_with_nothing_active_still_records_the_request() {
    let key = fake_key("queued-cancel");
    super::cancel(&key.0, &key.1).await;

    assert!(
        super::cancel_requested().lock().await.contains(&key),
        "a cancel that found no active download should still be remembered"
    );
}

/// `install`'s own defensive self-cancel (stopping a previous run of
/// the same key before starting a new one) must not make the fresh
/// attempt it's about to start look pre-cancelled -- it clears the
/// flag right after, which this checks directly since `install`
/// itself needs a live GOG session to run at all.
#[tokio::test]
async fn clearing_after_a_defensive_cancel_leaves_no_stale_request() {
    let key = fake_key("defensive-cancel");
    super::cancel(&key.0, &key.1).await;
    assert!(super::cancel_requested().lock().await.contains(&key));

    super::cancel_requested().lock().await.remove(&key);

    assert!(!super::cancel_requested().lock().await.contains(&key));
}
