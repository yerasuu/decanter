//! End-to-end install against GOG's real CDN.
//!
//! Ignored by default: this needs a logged-in profile and downloads a real
//! game — 2.4 GB for the default pick. Run it deliberately:
//!
//! ```text
//! cargo test -p achv-gog -- --ignored --nocapture
//! ```
//!
//! It exists because the failure it caught can't be caught any other way.
//! A GOG build carries one depot per product — the base game and every
//! DLC of it — and taking the wrong one downloads a fraction of the game
//! and then 404s, because a depot's chunks are signed for their own
//! product. `depot::select_depots` has unit tests for the selection;
//! only a real download proves the URLs it produces are accepted.
//!
//! The default pick is the smallest game in the test account that has a
//! DLC. Override with `ACHV_TEST_GAME` / `ACHV_TEST_DLC`.

use achv_core::{Db, Library, Storefront, default_db_path};

fn ids() -> (String, String) {
    (
        std::env::var("ACHV_TEST_GAME").unwrap_or_else(|_| "1854689188".to_owned()),
        std::env::var("ACHV_TEST_DLC").unwrap_or_else(|_| "1191925155".to_owned()),
    )
}

fn library() -> Library {
    Library::new(Library::default_path().expect("data directory"))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "downloads a real game from GOG"]
async fn installs_a_game_and_then_a_dlc_into_it() {
    let profile = Db::open(default_db_path())
        .expect("database")
        .active_profile()
        .expect("profile query")
        .expect("a logged-in profile")
        .id;
    let (game_id, dlc_id) = ids();
    let library = library();

    achv_gog::install(&profile, &game_id, None, "Test Game", None, None, |_| {})
        .await
        .expect("the base game should install");

    let installed = library
        .read_manifest(&profile, Storefront::Gog, &game_id)
        .expect("a manifest for the game");
    assert!(
        installed.launch_executable.is_some(),
        "an installed game should have something to launch"
    );
    let before = installed.size_on_disk.expect("a recorded size");
    assert!(
        before > 100_000_000,
        "only a fraction of the game was downloaded ({before} bytes) — \
         the wrong depots were selected"
    );

    achv_gog::install(&profile, &dlc_id, Some(&game_id), "Test DLC", None, None, |_| {})
        .await
        .expect("the DLC should install");

    let after = library
        .read_manifest(&profile, Storefront::Gog, &game_id)
        .expect("the game's manifest after the DLC");
    let dlc = after.dlcs.get(&dlc_id).expect("the DLC on the game");
    assert_eq!(dlc.title, "Test DLC");
    assert!(
        after.size_on_disk.expect("a size") > before,
        "the DLC's bytes should have grown the game's recorded size"
    );

    // A DLC installs into the game, so it gets no install of its own.
    assert!(
        library.read_manifest(&profile, Storefront::Gog, &dlc_id).is_none(),
        "a DLC must not have a manifest of its own"
    );
    // And its files landed beside the game's, in one directory.
    let directory = library.install_dir(&after.install_dir).expect("an install directory");
    assert!(directory.join(format!("goggame-{game_id}.info")).is_file());
    assert!(directory.join(format!("goggame-{dlc_id}.info")).is_file());
}

/// A DLC's files belong in the base game's directory, so there is nowhere
/// to put them until it is installed. Refused before anything downloads.
#[tokio::test(flavor = "current_thread")]
#[ignore = "needs a logged-in GOG profile"]
async fn refuses_a_dlc_whose_game_is_not_installed() {
    let profile = Db::open(default_db_path())
        .expect("database")
        .active_profile()
        .expect("profile query")
        .expect("a logged-in profile")
        .id;
    let (_, dlc_id) = ids();

    let result = achv_gog::install(&profile, &dlc_id, Some("999999999"), "Test DLC", None, None, |_| {}).await;
    assert!(result.is_err(), "a DLC with no base game must not install");
}

/// Unticking a DLC removes what it alone shipped and leaves the game
/// alone — the property that makes the checklist safe to toggle.
///
/// Runs against whatever the test above installed, so it does not download
/// anything of its own.
#[tokio::test(flavor = "current_thread")]
#[ignore = "needs the game from the test above already installed"]
async fn removing_a_dlc_leaves_the_game_intact() {
    let profile = Db::open(default_db_path())
        .expect("database")
        .active_profile()
        .expect("profile query")
        .expect("a logged-in profile")
        .id;
    let (game_id, dlc_id) = ids();
    let library = library();

    let before = library
        .read_manifest(&profile, Storefront::Gog, &game_id)
        .expect("the game must be installed for this test");
    assert!(
        before.dlcs.contains_key(&dlc_id),
        "the DLC must be installed for this test"
    );
    let directory = library.install_dir(&before.install_dir).expect("an install directory");
    let executable = directory.join(before.launch_executable.as_deref().expect("a launch executable"));

    achv_gog::uninstall_dlc(&profile, &dlc_id, &game_id)
        .await
        .expect("the DLC should come off");

    let after = library
        .read_manifest(&profile, Storefront::Gog, &game_id)
        .expect("the game's manifest survives");
    assert!(!after.dlcs.contains_key(&dlc_id), "the DLC record should be gone");
    assert!(
        after.size_on_disk.expect("a size") < before.size_on_disk.expect("a size"),
        "the recorded size should have shrunk by the DLC's files"
    );

    // The DLC's own marker went; the game's did not, and neither did the
    // thing you launch.
    assert!(!directory.join(format!("goggame-{dlc_id}.info")).is_file());
    assert!(directory.join(format!("goggame-{game_id}.info")).is_file());
    assert!(executable.is_file(), "the game must still be launchable");
}
