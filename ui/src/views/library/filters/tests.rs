use std::collections::{HashMap, HashSet};

use super::{AchievementState, Filters, KindSelection, LibraryStats, PlayState, SavedFilter, Sort, sort_entries};
use crate::background::InstallStatus;
use achv_core::Game;
use achv_gog::{GameKind, LibraryEntry};

fn entry(id: &str) -> LibraryEntry {
    LibraryEntry {
        game: Game {
            id: id.to_string(),
            title: id.to_string(),
            ..Default::default()
        },
        kind: GameKind::Game,
        parent_id: None,
        installable: true,
        purchased_at: None,
        library_rank: None,
    }
}

#[test]
fn a_saved_filter_survives_a_round_trip() {
    let mut filters = Filters::default();
    filters.states.insert(PlayState::Installed);
    filters.genres.insert("RPG".to_string());
    filters.achievements.insert(AchievementState::InProgress);
    filters.collection = Some("rpgs".to_string());
    filters.sort = Sort::MostPlayed;
    let saved = SavedFilter {
        kind: KindSelection::GamesAndExtras,
        filters,
    };

    let decoded = SavedFilter::decode(&saved.encode().unwrap()).unwrap();
    assert!(decoded == saved);
}

/// Nothing cached is unmeasured, not zero — the distinction the whole
/// bucket split rests on.
#[test]
fn missing_achievement_data_is_its_own_bucket() {
    assert!(AchievementState::of(None) == AchievementState::Unmeasured);
    assert!(AchievementState::of(Some(0.0)) == AchievementState::NotStarted);
    assert!(AchievementState::of(Some(0.5)) == AchievementState::InProgress);
    assert!(AchievementState::of(Some(1.0)) == AchievementState::Complete);
}

#[test]
fn an_unreadable_filter_decodes_to_nothing() {
    assert!(SavedFilter::decode("not json").is_none());
}

/// A filter written before an option existed should still load.
#[test]
fn missing_fields_fall_back_to_defaults() {
    let decoded = SavedFilter::decode("{}").unwrap();
    assert!(decoded == SavedFilter::default());
}

/// GOG's rank wins even against a much larger local timestamp -- it's
/// not a tiebreaker, it's the authority whenever it's present.
#[test]
fn date_added_prefers_gogs_rank_over_the_local_timestamp() {
    let mut stats = LibraryStats::default();
    stats.added_rank.insert("newer_by_rank".to_string(), 0);
    stats.added_rank.insert("older_by_rank".to_string(), 5);
    stats.added.insert("newer_by_rank".to_string(), 100);
    stats.added.insert("older_by_rank".to_string(), 999_999_999);

    let mut entries = vec![entry("older_by_rank"), entry("newer_by_rank")];
    sort_entries(&mut entries, Sort::DateAdded, &stats);

    assert_eq!(entries[0].game.id, "newer_by_rank");
    assert_eq!(entries[1].game.id, "older_by_rank");
}

/// A game GOG's ranked fetch never covered still sorts by the local
/// fallback, but always behind anything that does have a rank.
#[test]
fn date_added_falls_back_to_the_local_timestamp_when_ungranked() {
    let mut stats = LibraryStats::default();
    stats.added_rank.insert("ranked".to_string(), 3);
    stats.added.insert("ranked".to_string(), 10);
    stats.added.insert("unranked_but_recent".to_string(), 500);
    stats.added.insert("unranked_and_old".to_string(), 10);

    let mut entries = vec![entry("unranked_and_old"), entry("ranked"), entry("unranked_but_recent")];
    sort_entries(&mut entries, Sort::DateAdded, &stats);

    assert_eq!(
        entries[0].game.id, "ranked",
        "any rank beats no rank, however old the timestamp"
    );
    assert_eq!(entries[1].game.id, "unranked_but_recent");
    assert_eq!(entries[2].game.id, "unranked_and_old");
}

#[test]
fn a_queued_or_downloading_game_reads_as_installed() {
    let game = Game {
        id: "g1".to_string(),
        ..Default::default()
    };
    let mut pending = HashSet::new();
    pending.insert("g1".to_string());

    assert!(super::is_installed(&game, &pending));
}

#[test]
fn a_genuinely_uninstalled_game_with_nothing_pending_is_not_installed() {
    let game = Game {
        id: "g1".to_string(),
        ..Default::default()
    };
    assert!(!super::is_installed(&game, &HashSet::new()));
}

/// A failed install has already settled back to not-installed -- it
/// shouldn't keep reading as one just because it's still in the map
/// (e.g. until the user dismisses the error).
#[test]
fn pending_install_ids_excludes_failed_ones() {
    let mut installing = HashMap::new();
    installing.insert("queued".to_string(), InstallStatus::Queued);
    installing.insert(
        "downloading".to_string(),
        InstallStatus::Installing {
            current_file: "file.bin".to_string(),
            bytes_downloaded: 10,
            total_bytes: Some(100),
            bytes_per_second: 1.0,
        },
    );
    installing.insert("broken".to_string(), InstallStatus::Failed("oops".to_string()));

    let pending = super::pending_install_ids(&installing);

    assert!(pending.contains("queued"));
    assert!(pending.contains("downloading"));
    assert!(!pending.contains("broken"));
}
