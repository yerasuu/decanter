#![expect(
    clippy::panic,
    reason = "unwrap_used/expect_used push every fallible call in a test toward panic!, \
              which clippy::panic then flags right back"
)]
#![expect(
    clippy::option_if_let_else,
    reason = "a match reads clearer here than map_or_else for a panic-on-None/Err branch"
)]

use std::{env, fs};

use super::*;

fn scratch(name: &str) -> Library {
    let root = env::temp_dir().join("decanter-library-tests").join(name);
    // Best-effort: a fresh scratch dir has nothing to remove yet.
    drop(fs::remove_dir_all(&root));
    match fs::create_dir_all(&root) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    Library::new(root)
}

fn game(account: &str, id: &str, install_dir: &str) -> InstalledGame {
    InstalledGame {
        account_id: account.to_owned(),
        storefront: Storefront::Gog,
        game_id: id.to_owned(),
        name: "Right and Down".to_owned(),
        install_dir: install_dir.to_owned(),
        build_id: "59540632564148293".to_owned(),
        size_on_disk: Some(5_566_295_753),
        launch_executable: Some("Binaries/Win32/Betrayer.exe".to_owned()),
        last_played: None,
        proton: None,
        branch: None,
        dlcs: BTreeMap::new(),
        cloud_save_path: None,
        cloud_save_last_sync: None,
    }
}

#[test]
fn a_manifest_round_trips_through_disk() {
    let library = scratch("round-trip");
    let mut original = game("profile", "1741027632", "Right and Down");
    original.last_played = Some(1_787_529_449);
    original.proton = Some("GE-Proton11-1".to_owned());
    original.dlcs.insert(
        "1256837418".to_owned(),
        InstalledDlc {
            title: "Phantom Liberty".to_owned(),
            build_id: "594".to_owned(),
        },
    );
    original.cloud_save_path = Some("/prefix/drive_c/users/steamuser/AppData/Roaming/Betrayer".to_owned());
    original.cloud_save_last_sync = Some(1_787_529_500);

    match library.write_manifest(&original) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    assert_eq!(
        library.read_manifest("profile", Storefront::Gog, "1741027632"),
        Some(original)
    );
}

/// Never-played must not come back as "played at the epoch".
/// A game with no DLC writes no block at all, rather than an empty
/// one — an absent DLC list and an empty DLC list mean the same thing.
#[test]
fn a_game_without_dlc_writes_no_dlc_block() {
    let library = scratch("no-dlc");
    match library.write_manifest(&game("profile", "42", "Game")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let path = match library.manifest_path("profile", Storefront::Gog, "42") {
        Some(value) => value,
        None => panic!("called `Option::unwrap()` on a `None` value"),
    };
    let content = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(!content.contains("InstalledDLC"));
    let read = match library.read_manifest("profile", Storefront::Gog, "42") {
        Some(value) => value,
        None => panic!("called `Option::unwrap()` on a `None` value"),
    };
    assert!(read.dlcs.is_empty());
}

#[test]
fn a_never_played_game_has_no_last_played() {
    let library = scratch("never-played");
    match library.write_manifest(&game("profile", "42", "Game")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let read = match library.read_manifest("profile", Storefront::Gog, "42") {
        Some(value) => value,
        None => panic!("called `Option::unwrap()` on a `None` value"),
    };
    assert_eq!(read.last_played, None);
}

#[test]
fn writing_a_manifest_announces_the_library() {
    let library = scratch("libraryfolders");
    match library.write_manifest(&game("profile", "42", "Game")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    let content = match fs::read_to_string(library.root().join("libraryfolders.vdf")) {
        Ok(value) => value,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(content.contains("libraryfolders"));
    assert_eq!(library.roots(), vec![library.root().to_path_buf()]);
}

#[test]
fn scanning_returns_only_that_accounts_games() {
    let library = scratch("scan");
    match library.write_manifest(&game("a", "1", "One")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match library.write_manifest(&game("a", "2", "Two")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match library.write_manifest(&game("b", "3", "Three")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    assert_eq!(library.scan("a").keys().collect::<Vec<_>>(), vec!["1", "2"]);
    assert_eq!(library.scan("b").keys().collect::<Vec<_>>(), vec!["3"]);
    assert_eq!(library.scan_all().len(), 3);
    assert!(library.scan("nobody").is_empty());
}

/// The whole point of a shared `common/`: uninstalling for one account
/// must not delete files another account is still using.
#[test]
fn shared_install_directories_are_detected() {
    let library = scratch("shared");
    match library.write_manifest(&game("a", "42", "Shared")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match library.write_manifest(&game("b", "42", "Shared")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    assert!(library.install_dir_is_shared("Shared", "a", "42"));

    match library.remove_manifest("b", Storefront::Gog, "42") {
        Ok(_) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    assert!(!library.install_dir_is_shared("Shared", "a", "42"));
}

#[test]
fn removing_returns_what_it_dropped_and_is_idempotent() {
    let library = scratch("remove");
    let game = game("profile", "42", "Game");
    match library.write_manifest(&game) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    match library.remove_manifest("profile", Storefront::Gog, "42") {
        Ok(dropped) => assert_eq!(dropped, Some(game)),
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    match library.remove_manifest("profile", Storefront::Gog, "42") {
        Ok(dropped) => assert_eq!(dropped, None),
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
}

#[test]
fn paths_land_where_the_layout_says() {
    let library = Library::new("/lib/steamapps");
    assert_eq!(
        library.install_dir("Right and Down"),
        Some(PathBuf::from("/lib/steamapps/common/Right and Down"))
    );
    assert_eq!(
        library.compatdata("profile", "1741027632"),
        Some(PathBuf::from("/lib/steamapps/compatdata/profile/1741027632"))
    );
    assert_eq!(
        library.manifest_path("profile", Storefront::Gog, "1741027632"),
        Some(PathBuf::from(
            "/lib/steamapps/manifests/profile/appmanifest_storefront_gog_1741027632.acf"
        ))
    );
}

/// Install directories and ids come off the network; none of them may
/// reach outside the library.
#[test]
fn refuses_names_that_would_escape_the_library() {
    let library = Library::new("/lib/steamapps");
    assert_eq!(library.install_dir(".."), None);
    assert_eq!(library.install_dir("a/b"), None);
    assert_eq!(library.install_dir(r"a\b"), None);
    assert_eq!(library.install_dir(""), None);
    assert_eq!(library.compatdata("..", "42"), None);
    assert_eq!(library.compatdata("profile", "../../etc"), None);
    assert_eq!(library.manifest_path("profile", Storefront::Gog, "../x"), None);
}

/// A file someone hand-edited into nonsense is skipped, not fatal —
/// and doesn't take the rest of the account's games with it.
#[test]
fn a_corrupt_manifest_is_skipped_not_fatal() {
    let library = scratch("corrupt");
    match library.write_manifest(&game("profile", "1", "One")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let directory = library.root().join("manifests/profile");
    match fs::write(directory.join("appmanifest_storefront_gog_2.acf"), "{ this is not vdf") {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    assert_eq!(library.scan("profile").len(), 1);
}

/// A manifest missing what makes it usable is not a half-install.
#[test]
fn a_manifest_without_an_install_dir_is_ignored() {
    let library = scratch("incomplete");
    let directory = library.root().join("manifests/account");
    match fs::create_dir_all(&directory) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    let mut root = Value::block();
    root.insert("gameid", "42");
    root.insert("storefront", "STOREFRONT_GOG");
    root.insert("accountid", "account");
    match fs::write(
        directory.join("appmanifest_storefront_gog_42.acf"),
        vdf::to_string("AppState", &root),
    ) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }

    assert!(library.scan("profile").is_empty());
}

/// Deleting a manifest by hand uninstalls the game — the property
/// that makes the files on disk the source of truth.
#[test]
fn deleting_a_manifest_by_hand_uninstalls_it() {
    let library = scratch("hand-delete");
    match library.write_manifest(&game("profile", "42", "Game")) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    assert_eq!(library.scan("profile").len(), 1);

    let path = match library.manifest_path("profile", Storefront::Gog, "42") {
        Some(value) => value,
        None => panic!("called `Option::unwrap()` on a `None` value"),
    };
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    }
    assert!(library.scan("profile").is_empty());
}
