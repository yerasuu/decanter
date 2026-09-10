use super::*;

fn local(path: &str, modified_ts: i64) -> LocalFile {
    LocalFile {
        relative_path: path.to_string(),
        modified: DateTime::from_timestamp(modified_ts, 0).unwrap(),
    }
}

fn cloud(path: &str, modified_ts: i64) -> CloudFile {
    CloudFile {
        relative_path: path.to_string(),
        md5: "deadbeef".to_string(),
        last_modified: DateTime::from_timestamp(modified_ts, 0).unwrap(),
    }
}

#[test]
fn uploads_everything_when_the_cloud_is_empty() {
    let (action, plan) = classify(&[local("save.dat", 100)], &[], None, None);
    assert_eq!(action, SyncAction::Upload);
    assert_eq!(plan.upload, vec!["save.dat"]);
    assert!(plan.delete_remote.is_empty());
}

#[test]
fn downloads_everything_when_local_is_empty() {
    let (action, plan) = classify(&[], &[cloud("save.dat", 100)], None, None);
    assert_eq!(action, SyncAction::Download);
    assert_eq!(plan.download, vec!["save.dat"]);
}

#[test]
fn does_nothing_when_neither_side_changed_since_last_sync() {
    let (action, _) = classify(&[local("save.dat", 100)], &[cloud("save.dat", 100)], Some(200), None);
    assert_eq!(action, SyncAction::None);
}

#[test]
fn uploads_only_files_changed_since_last_sync() {
    let (action, plan) = classify(&[local("save.dat", 300)], &[cloud("save.dat", 100)], Some(200), None);
    assert_eq!(action, SyncAction::Upload);
    assert_eq!(plan.upload, vec!["save.dat"]);
}

#[test]
fn downloads_only_files_changed_since_last_sync() {
    let (action, plan) = classify(&[local("save.dat", 100)], &[cloud("save.dat", 300)], Some(200), None);
    assert_eq!(action, SyncAction::Download);
    assert_eq!(plan.download, vec!["save.dat"]);
}

#[test]
fn conflicts_when_both_sides_changed_since_last_sync() {
    let (action, _) = classify(&[local("save.dat", 300)], &[cloud("other.dat", 300)], Some(200), None);
    assert_eq!(action, SyncAction::Conflict);
}

#[test]
fn upload_deletes_remote_files_no_longer_present_locally() {
    let (action, plan) = classify(
        &[local("keep.dat", 300)],
        &[cloud("keep.dat", 100), cloud("gone.dat", 100)],
        Some(200),
        None,
    );
    assert_eq!(action, SyncAction::Upload);
    assert_eq!(plan.delete_remote, vec!["gone.dat"]);
}

#[test]
fn download_deletes_local_files_no_longer_present_remotely() {
    let (action, plan) = classify(
        &[local("keep.dat", 100), local("gone.dat", 100)],
        &[cloud("keep.dat", 300)],
        Some(200),
        None,
    );
    assert_eq!(action, SyncAction::Download);
    assert_eq!(plan.delete_local, vec!["gone.dat"]);
}

/// A deletion is invisible to the `updated_local`/`updated_cloud` mtime
/// filters — the deleted file simply isn't in the listing anymore, and
/// nothing else changed to otherwise trigger a sync.
#[test]
fn a_lone_local_deletion_with_nothing_else_changed_still_propagates() {
    let (action, plan) = classify(
        &[local("keep.dat", 100)],
        &[cloud("keep.dat", 100), cloud("gone.dat", 100)],
        Some(200),
        None,
    );
    assert_eq!(action, SyncAction::Upload);
    assert_eq!(plan.delete_remote, vec!["gone.dat"]);
}

#[test]
fn a_lone_remote_deletion_with_nothing_else_changed_still_propagates() {
    let (action, plan) = classify(
        &[local("keep.dat", 100), local("gone.dat", 100)],
        &[cloud("keep.dat", 100)],
        Some(200),
        None,
    );
    assert_eq!(action, SyncAction::Download);
    assert_eq!(plan.delete_local, vec!["gone.dat"]);
}

/// Each side deleted a *different* file since the last sync — neither
/// deletion should be guessed at silently.
#[test]
fn independent_deletions_on_both_sides_conflict() {
    let (action, _) = classify(
        &[local("keep.dat", 100), local("local_only.dat", 100)],
        &[cloud("keep.dat", 100), cloud("cloud_only.dat", 100)],
        Some(200),
        None,
    );
    assert_eq!(action, SyncAction::Conflict);
}

#[test]
fn force_upload_ignores_timestamps_and_sends_every_local_file() {
    let (action, plan) = classify(
        &[local("save.dat", 100)],
        &[cloud("save.dat", 999)],
        Some(200),
        Some(ForceDirection::Upload),
    );
    assert_eq!(action, SyncAction::Upload);
    assert_eq!(plan.upload, vec!["save.dat"]);
}

#[test]
fn force_download_ignores_timestamps_and_fetches_every_cloud_file() {
    let (action, plan) = classify(
        &[local("save.dat", 999)],
        &[cloud("save.dat", 100)],
        Some(200),
        Some(ForceDirection::Download),
    );
    assert_eq!(action, SyncAction::Download);
    assert_eq!(plan.download, vec!["save.dat"]);
}

#[test]
fn encodes_path_segments_but_leaves_slashes_alone() {
    assert_eq!(
        encode_relative_path("Save Games/slot 1.sav"),
        "Save%20Games/slot%201.sav"
    );
}

#[test]
fn gzip_roundtrips() {
    let data = b"hello cloud saves";
    let compressed = gzip_compress(data).unwrap();
    assert_eq!(gzip_decompress(&compressed).unwrap(), data);
}
