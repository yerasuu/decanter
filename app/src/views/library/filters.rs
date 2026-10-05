//! What narrows the library, extracted from the sidebar because a
//! dynamic collection is exactly one of these saved to disk — so the
//! shelf builder has to be able to evaluate one without pulling in the
//! whole sidebar.
//!
//! Filter semantics follow Steam's: leaving a section empty means "no
//! constraint", checks inside a section combine, and sections combine
//! with each other. Genres are alternatives, so they're OR'd; play states
//! are independent properties of one game (installed, unplayed,
//! favourite), so they're AND'd — checking two narrows rather than
//! widens.

use std::collections::{HashMap, HashSet};

use decanter_core::{Game, Storefront};
use decanter_gog::{GameKind, LibraryEntry};
use serde::{Deserialize, Serialize};

use super::data::{Collections, LibraryStats};
use crate::state::InstallStatus;

/// What slice of the library is on screen — the coarse "which shelf am I
/// looking at" choice, not a constraint layered on top of one.
#[derive(Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub(super) enum KindSelection {
    #[default]
    Games,
    GamesAndExtras,
    Extras,
    Movies,
    All,
}

impl KindSelection {
    pub fn accepts(self, kind: GameKind) -> bool {
        match self {
            KindSelection::Games => kind == GameKind::Game,
            KindSelection::GamesAndExtras => matches!(kind, GameKind::Game | GameKind::Extra),
            KindSelection::Extras => kind == GameKind::Extra,
            KindSelection::Movies => kind == GameKind::Movie,
            KindSelection::All => true,
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "games_extras" => KindSelection::GamesAndExtras,
            "extras" => KindSelection::Extras,
            "movies" => KindSelection::Movies,
            "all" => KindSelection::All,
            _ => KindSelection::Games,
        }
    }

    pub const fn value(self) -> &'static str {
        match self {
            KindSelection::Games => "games",
            KindSelection::GamesAndExtras => "games_extras",
            KindSelection::Extras => "extras",
            KindSelection::Movies => "movies",
            KindSelection::All => "all",
        }
    }
}

/// Which achievement bucket a game falls in. "Nothing cached" is its own
/// answer rather than 0% — a game with no achievement data isn't 0%
/// complete, it's unmeasured.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(super) enum AchievementState {
    Complete,
    InProgress,
    NotStarted,
    Unmeasured,
}

impl AchievementState {
    pub fn of(completion: Option<f64>) -> Self {
        match completion {
            None => AchievementState::Unmeasured,
            Some(value) if value >= 1.0 => AchievementState::Complete,
            Some(value) if value > 0.0 => AchievementState::InProgress,
            Some(_) => AchievementState::NotStarted,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(super) enum PlayState {
    Installed,
    Unplayed,
    Played,
    Favorite,
}

/// What to order the list by. Every option reads from data already held
/// locally, so switching never costs a network round trip.
#[derive(Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub(super) enum Sort {
    #[default]
    Title,
    MostPlayed,
    RecentlyPlayed,
    Completion,
    ReleaseDate,
    SizeOnDisk,
    Rating,
    DateAdded,
}

impl Sort {
    pub fn from_value(value: &str) -> Self {
        match value {
            "played" => Sort::MostPlayed,
            "recent" => Sort::RecentlyPlayed,
            "completion" => Sort::Completion,
            "release" => Sort::ReleaseDate,
            "size" => Sort::SizeOnDisk,
            "rating" => Sort::Rating,
            "added" => Sort::DateAdded,
            _ => Sort::Title,
        }
    }

    pub const fn value(self) -> &'static str {
        match self {
            Sort::Title => "title",
            Sort::MostPlayed => "played",
            Sort::RecentlyPlayed => "recent",
            Sort::Completion => "completion",
            Sort::ReleaseDate => "release",
            Sort::SizeOnDisk => "size",
            Sort::Rating => "rating",
            Sort::DateAdded => "added",
        }
    }
}

#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Filters {
    pub states: HashSet<PlayState>,
    pub achievements: HashSet<AchievementState>,
    pub genres: HashSet<String>,
    /// Narrow to one collection by id. Matches explicit membership only —
    /// a dynamic collection is itself a filter, and letting one filter by
    /// another would be a cycle waiting to happen.
    pub collection: Option<String>,
    /// Narrow to one or more storefronts — empty means every storefront
    /// the profile has an account on. Only GOG is implemented today, so
    /// in practice this either has one real option or none, but the
    /// mechanics are ready for the next storefront without touching this
    /// again.
    pub storefronts: HashSet<Storefront>,
    /// Narrow to one or more accounts by id — same "empty means no
    /// constraint, checked ones OR together" shape as `storefronts`, one
    /// level more specific: two GOG accounts in one profile both show up
    /// here even though they'd both pass a `storefronts` check alone.
    pub accounts: HashSet<String>,
    pub sort: Sort,
}

impl Filters {
    /// How many constraints the user applied, for the badge on the
    /// toggle. Sort isn't one — it reorders, it never hides anything.
    pub fn active_count(&self) -> usize {
        self.states.len()
            + self.achievements.len()
            + self.genres.len()
            + usize::from(self.collection.is_some())
            + self.storefronts.len()
            + self.accounts.len()
    }

    pub fn is_default(&self) -> bool {
        self.active_count() == 0
    }

    pub fn accepts(
        &self,
        entry: &LibraryEntry,
        favorites: &HashSet<String>,
        stats: &LibraryStats,
        collections: &Collections,
        pending_installs: &HashSet<String>,
    ) -> bool {
        if let Some(collection_id) = &self.collection
            && !collections.contains(collection_id, &entry.game.id)
        {
            return false;
        }
        if !self.genres.is_empty() && !entry.game.genres.iter().any(|genre| self.genres.contains(genre)) {
            return false;
        }
        if !self.storefronts.is_empty() && !self.storefronts.contains(&entry.game.storefront) {
            return false;
        }
        if !self.accounts.is_empty() && !self.accounts.contains(&entry.game.account_id) {
            return false;
        }
        // Buckets are alternatives, so they widen the result the way
        // genres do rather than narrowing it.
        if !self.achievements.is_empty()
            && !self
                .achievements
                .contains(&AchievementState::of(stats.completion(&entry.game.id)))
        {
            return false;
        }
        self.states.iter().all(|state| match state {
            PlayState::Installed => is_installed(&entry.game, pending_installs),
            PlayState::Unplayed => stats.seconds(&entry.game.id) == 0,
            PlayState::Played => stats.seconds(&entry.game.id) > 0,
            PlayState::Favorite => favorites.contains(&entry.game.id),
        })
    }
}

/// A whole filter state, as a dynamic collection stores it. Versioned by
/// `#[serde(default)]` on the parts rather than a version field: a filter
/// saved before a new option existed should still load, just without it.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct SavedFilter {
    pub kind: KindSelection,
    pub filters: Filters,
}

/// Whether an entry is worth a row of its own.
///
/// A GOG account accumulates two kinds of thing that aren't really library
/// entries, and both are noise in a list of games:
///
/// - **Nothing resolved.** Delisted products — withdrawn goodies
///   collections, expired promo grants. Neither `GamesDB` nor GOG's own
///   product API answers for them, so they arrive as a numeric id and
///   nothing else: not launchable, not installable, not even nameable.
///   A game is kept regardless, because a title that failed to resolve is
///   a gap to fix rather than a reason to hide something playable.
///
/// - **Nothing to install.** Superseded editions and bundle listings that
///   GOG keeps in an account for the entitlement's sake but serves no
///   download for (`is_installable`). What they cover is owned separately
///   and installable on its own — `Ultima Underworld 1+2` beside the two
///   games it granted — so the container is a duplicate row for something
///   already in the list.
pub(super) fn belongs_in_the_library(entry: &LibraryEntry) -> bool {
    if !entry.installable {
        return false;
    }
    entry.kind == GameKind::Game || entry.game.title != entry.game.id
}

impl SavedFilter {
    pub fn accepts(
        &self,
        entry: &LibraryEntry,
        favorites: &HashSet<String>,
        stats: &LibraryStats,
        collections: &Collections,
        pending_installs: &HashSet<String>,
    ) -> bool {
        // A DLC belongs under its game, not beside it — it shows in that
        // game's detail pane. Hidden here rather than in `KindSelection`
        // so no saved filter can bring either it or the entries below
        // back into the flat list.
        entry.parent_id.is_none()
            && belongs_in_the_library(entry)
            && self.kind.accepts(entry.kind)
            && self
                .filters
                .accepts(entry, favorites, stats, collections, pending_installs)
    }

    pub fn encode(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }

    /// A filter that no longer parses (hand-edited, or written by a much
    /// newer build) yields `None`, and the collection it belongs to falls
    /// back to matching nothing rather than everything.
    pub fn decode(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }
}

/// A queued or downloading install counts as installed everywhere this is
/// read — browsing shouldn't show a game as absent while it's actively
/// arriving, only once it's genuinely gone (nothing running, no record).
pub(super) fn is_installed(game: &Game, pending_installs: &HashSet<String>) -> bool {
    game.install_state.as_ref().is_some_and(|state| state.installed) || pending_installs.contains(&game.id)
}

/// Product ids currently queued or downloading -- a failed install isn't
/// one of them, since it settled back to genuinely not-installed rather
/// than being on its way to becoming one.
pub(super) fn pending_install_ids(installing: &HashMap<String, InstallStatus>) -> HashSet<String> {
    installing
        .iter()
        .filter(|(_, status)| !matches!(status, InstallStatus::Failed(_)))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Orders in place. Everything but `Title` falls back to title for ties,
/// so entries with no playtime (or no achievements, or no release date —
/// most of a large library) stay alphabetical among themselves instead of
/// landing in whatever order they arrived in.
pub(super) fn sort_entries(entries: &mut [LibraryEntry], sort: Sort, stats: &LibraryStats) {
    let by_title = |a: &LibraryEntry, b: &LibraryEntry| a.game.title.to_lowercase().cmp(&b.game.title.to_lowercase());
    // `None` means "nothing known", which sorts last rather than as a zero.
    let ranked = |left: Option<f64>, right: Option<f64>, a: &LibraryEntry, b: &LibraryEntry| match (left, right) {
        (Some(left), Some(right)) => right
            .partial_cmp(&left)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| by_title(a, b)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => by_title(a, b),
    };

    match sort {
        Sort::Title => entries.sort_by(by_title),
        Sort::MostPlayed => entries.sort_by(|a, b| {
            stats
                .seconds(&b.game.id)
                .cmp(&stats.seconds(&a.game.id))
                .then_with(|| by_title(a, b))
        }),
        Sort::RecentlyPlayed => entries.sort_by(|a, b| {
            stats
                .last_played(&b.game.id)
                .cmp(&stats.last_played(&a.game.id))
                .then_with(|| by_title(a, b))
        }),
        Sort::Completion => {
            entries.sort_by(|a, b| ranked(stats.completion(&a.game.id), stats.completion(&b.game.id), a, b));
        }
        Sort::ReleaseDate => entries.sort_by(|a, b| {
            let seconds = |entry: &LibraryEntry| entry.game.release_date.map(|date| date as f64);
            ranked(seconds(a), seconds(b), a, b)
        }),
        // Not installed means no size at all, which sorts apart from a
        // small install rather than below it.
        Sort::SizeOnDisk => entries.sort_by(|a, b| {
            let size = |entry: &LibraryEntry| stats.size(&entry.game.id).map(|n| n as f64);
            ranked(size(a), size(b), a, b)
        }),
        // GamesDB's aggregated score, kept as text on the wire.
        Sort::Rating => entries.sort_by(|a, b| {
            let score = |entry: &LibraryEntry| {
                let r = entry.game.rating.as_deref()?;
                r.parse::<f64>().ok()
            };
            ranked(score(a), score(b), a, b)
        }),
        // GOG's own rank (smaller = more recent) takes priority over the
        // local approximation, and beats it out for any game either one
        // covers — it's the one signal that also covers gifts and dev
        // grants, not just real orders.
        Sort::DateAdded => entries.sort_by(
            |a, b| match (stats.added_rank(&a.game.id), stats.added_rank(&b.game.id)) {
                (Some(rank_a), Some(rank_b)) => rank_a.cmp(&rank_b).then_with(|| by_title(a, b)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => stats
                    .added(&b.game.id)
                    .cmp(&stats.added(&a.game.id))
                    .then_with(|| by_title(a, b)),
            },
        ),
    }
}
