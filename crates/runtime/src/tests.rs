use super::*;

// implicit_return fights needless_return (clippy::all, enforced everywhere else in this
// crate); idiomatic tail-expression return wins here.
#[expect(
    clippy::implicit_return,
    reason = "needless_return elsewhere already wants no explicit return"
)]
fn environ(entries: &[&str]) -> Vec<u8> {
    let mut blob = Vec::new();
    for entry in entries {
        blob.extend_from_slice(entry.as_bytes());
        blob.push(0);
    }
    blob
}

#[test]
fn matches_a_process_by_its_wineprefix() {
    let blob = environ(&["HOME=/home/x", "WINEPREFIX=/prefixes/game", "TERM=xterm"]);
    assert!(environ_has_prefix(&blob, Path::new("/prefixes/game")));
}

/// umu and Proton are already running before Wine exports anything, so
/// a session that is starting up still has to count as live.
#[test]
fn matches_umu_before_wine_has_started() {
    let blob = environ(&["STEAM_COMPAT_DATA_PATH=/prefixes/game"]);
    assert!(environ_has_prefix(&blob, Path::new("/prefixes/game")));
}

/// A second prefix whose path merely starts the same must not read as
/// the first one still being busy.
#[test]
fn does_not_match_a_prefix_that_is_only_a_path_prefix() {
    let blob = environ(&["WINEPREFIX=/prefixes/game2"]);
    assert!(!environ_has_prefix(&blob, Path::new("/prefixes/game")));
}

#[test]
fn ignores_an_unrelated_variable_holding_the_same_path() {
    let blob = environ(&["OLDPWD=/prefixes/game"]);
    assert!(!environ_has_prefix(&blob, Path::new("/prefixes/game")));
}

#[test]
fn resolves_a_windows_path_into_the_prefix() {
    let prefix = Prefix::new("/prefixes/game");
    assert_eq!(
        prefix.resolve_windows_path("C:\\Games\\GOG\\1234\\game.exe"),
        Some(PathBuf::from("/prefixes/game/drive_c/Games/GOG/1234/game.exe"))
    );
}

#[test]
fn refuses_a_windows_path_that_climbs_out_of_the_prefix() {
    let prefix = Prefix::new("/prefixes/game");
    assert_eq!(prefix.resolve_windows_path("C:\\..\\..\\etc\\passwd"), None);
}

#[test]
fn refuses_a_drive_other_than_c() {
    let prefix = Prefix::new("/prefixes/game");
    assert_eq!(prefix.resolve_windows_path("Z:\\home\\x\\game.exe"), None);
}

#[test]
fn sanitizing_strips_nothing_from_an_ordinary_relative_path() {
    assert_eq!(
        sanitize_relative_path("Binaries/Win32/game.exe"),
        Some(PathBuf::from("Binaries/Win32/game.exe"))
    );
}

#[test]
fn sanitizing_rejects_traversal_and_absolute_paths() {
    assert_eq!(sanitize_relative_path("../../etc/passwd"), None);
    assert_eq!(sanitize_relative_path("/etc/passwd"), None);
    assert_eq!(sanitize_relative_path(""), None);
}

#[test]
fn dll_overrides_are_unset_when_there_are_none() {
    assert_eq!(format_dll_overrides(&[]), None);
}

#[test]
fn dll_overrides_render_one_entry_each() {
    let overrides = [
        ("Galaxy64".to_owned(), "n,b".to_owned()),
        ("dxgi".to_owned(), "n".to_owned()),
    ];
    assert_eq!(format_dll_overrides(&overrides), Some("Galaxy64=n,b;dxgi=n".to_owned()));
}

#[test]
fn proton_latest_leaves_protonpath_unset() {
    assert_eq!(Proton::Latest.as_env_value(), None);
    assert_eq!(
        Proton::Named("GE-Proton".into()).as_env_value(),
        Some(OsStr::new("GE-Proton"))
    );
}

#[test]
fn drive_c_sits_directly_under_the_prefix() {
    // umu symlinks `pfx` back to the prefix itself, so there is no
    // `pfx/` level here the way a Steam compatdata layout has.
    assert_eq!(
        Prefix::new("/prefixes/game").drive_c(),
        PathBuf::from("/prefixes/game/drive_c")
    );
}
