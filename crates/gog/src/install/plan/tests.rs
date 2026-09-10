use super::manifest_relative_path;

#[test]
fn splits_windows_separators_into_directories() {
    let path = manifest_relative_path(r"Game_Data\sharedassets1.assets").unwrap();
    assert_eq!(
        path.components().count(),
        2,
        "a backslash must become a directory boundary, not part of the filename"
    );
    assert_eq!(path, std::path::Path::new("Game_Data/sharedassets1.assets"));
}

#[test]
fn keeps_plain_paths_intact() {
    assert_eq!(
        manifest_relative_path("UnityPlayer.dll").unwrap(),
        std::path::Path::new("UnityPlayer.dll")
    );
}

#[test]
fn rejects_traversal_written_the_windows_way() {
    assert_eq!(manifest_relative_path(r"..\..\etc\passwd"), None);
}

#[test]
fn rejects_absolute_and_empty() {
    assert_eq!(manifest_relative_path("/etc/passwd"), None);
    assert_eq!(manifest_relative_path(""), None);
}
