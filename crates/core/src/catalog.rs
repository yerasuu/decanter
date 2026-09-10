//! The catalog model: a storefront, a game as its store describes it, and
//! whether achv has it installed.
//!
//! [`Storefront`]'s discriminants are the integers already written into
//! every cache row, and changing them would silently re-key the whole
//! database — they're pinned, not free to renumber.

// Only fires on some compilation targets (lib vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::self_named_module_files,
    reason = "convention here is `<name>.rs` plus a sibling `<name>/tests.rs`, see CLAUDE.md"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "Game's Debug/Clone derive over this many Option<String> fields exceeds the 512-byte threshold by design"
)]

/// Which store a game came from.
///
/// The discriminants are persisted (the `storefront` column, and the
/// install-record key) — they are not free to change.
#[expect(
    clippy::exhaustive_enums,
    reason = "crates/gog and ui both match on this exhaustively with no wildcard arm"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(i32)]
pub enum Storefront {
    #[default]
    Unspecified = 0,
    Steam = 1,
    EpicGames = 2,
    Gog = 3,
    AmazonGames = 4,
    EaApp = 5,
    UbisoftConnect = 6,
    BattleNet = 7,
}

impl Storefront {
    /// The stable text form, used wherever a storefront names a directory
    /// or a keyring entry rather than a column: install roots on disk
    /// (`Program Files/STOREFRONT_GOG/...`) and credential accounts are
    /// both already written this way.
    #[must_use]
    #[inline]
    pub const fn as_str_name(self) -> &'static str {
        match self {
            Self::Unspecified => "STOREFRONT_UNSPECIFIED",
            Self::Steam => "STOREFRONT_STEAM",
            Self::EpicGames => "STOREFRONT_EPIC_GAMES",
            Self::Gog => "STOREFRONT_GOG",
            Self::AmazonGames => "STOREFRONT_AMAZON_GAMES",
            Self::EaApp => "STOREFRONT_EA_APP",
            Self::UbisoftConnect => "STOREFRONT_UBISOFT_CONNECT",
            Self::BattleNet => "STOREFRONT_BATTLE_NET",
        }
    }
}

impl TryFrom<i32> for Storefront {
    type Error = UnknownStorefront;

    #[inline]
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unspecified),
            1 => Ok(Self::Steam),
            2 => Ok(Self::EpicGames),
            3 => Ok(Self::Gog),
            4 => Ok(Self::AmazonGames),
            5 => Ok(Self::EaApp),
            6 => Ok(Self::UbisoftConnect),
            7 => Ok(Self::BattleNet),
            _ => Err(UnknownStorefront(value)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown storefront {0}")]
#[non_exhaustive]
pub struct UnknownStorefront(pub i32);

/// A game as its storefront describes it, plus achv's own view of
/// whether it's installed.
///
/// Every field but [`Game::account_id`] and [`Game::install_state`] is
/// catalog data straight from the store — optional throughout, because no
/// storefront populates all of it and an absent field means "the store
/// didn't say", not "empty".
#[expect(
    clippy::exhaustive_structs,
    reason = "crates/gog and ui both build this with a plain struct literal from outside this crate"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Game {
    /// Which account this copy is owned by — the same game can come back
    /// twice, once per account that owns it, when a profile groups more
    /// than one (see `crate::accounts`). Not catalog data: this is local
    /// bookkeeping the UI needs to merge several accounts' libraries
    /// without losing track of which account a row's actions apply to.
    pub account_id: String,
    /// Wide hero/banner art, distinct from `cover_url`'s portrait crop.
    pub background_url: Option<String>,
    pub cover_url: Option<String>,
    pub description: Option<String>,
    pub developer: Option<String>,
    /// Empty when the store has no real genre data — never filled with a
    /// non-genre stand-in (Epic's "categories" are store taxonomy, not
    /// genres).
    pub genres: Vec<String>,
    pub icon_url: Option<String>,
    /// The storefront's own product id.
    pub id: String,
    /// `None`, not `installed: false`, when achv doesn't govern this
    /// game's installs at all.
    pub install_state: Option<InstallState>,
    pub publisher: Option<String>,
    /// Whatever the store rates it — age rating, review score — as free
    /// text, since rating systems aren't consistent across stores.
    pub rating: Option<String>,
    /// Unix seconds. Seconds, not a date type, because that is what the
    /// cache column holds and the only thing anything does with it is
    /// format it or compare it.
    pub release_date: Option<i64>,
    pub storefront: Storefront,
    pub title: String,
}

/// Local install status for a [`Game`] — not catalog data.
#[expect(
    clippy::exhaustive_structs,
    reason = "same as Game: built with a plain struct literal from crates/gog and ui"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallState {
    pub install_size_bytes: Option<u64>,
    pub installed: bool,
    pub installed_version: Option<String>,
    /// The prefix it's installed in.
    pub prefix_id: Option<String>,
}

#[cfg(test)]
mod tests;
