//! Grouping the filtered library into the shelves the sidebar draws.
//!
//! Kept apart from rendering because the rules are the interesting part:
//! a game can sit on several shelves at once (collections are labels, not
//! containers), and what counts as a shelf changes entirely depending on
//! whether the user has made any collections yet.

use std::collections::HashSet;

use achv_gog::LibraryEntry;

use super::data::{Collections, LibraryStats};
use super::filters::{SavedFilter, is_installed};

pub(super) struct Shelf {
    /// Stable across renders so a collapsed shelf stays collapsed.
    pub id: String,
    /// Written as prose, not shouted. The headers uppercase it in CSS, but
    /// a `<select>` shows its options untransformed — pre-shouting the data
    /// makes every dropdown a wall of capitals.
    pub title: String,
    pub entries: Vec<LibraryEntry>,
    /// A collection's shelf can be removed or renamed; the built-in ones
    /// can't.
    pub collection_id: Option<String>,
    /// A dynamic collection's contents follow its saved filter, so games
    /// can't be added or removed from it by hand.
    pub dynamic: bool,
}

impl Shelf {
    pub fn installed_count(&self, pending_installs: &HashSet<String>) -> usize {
        self.entries
            .iter()
            .filter(|entry| is_installed(&entry.game, pending_installs))
            .count()
    }
}

/// Splits `entries` into shelves.
///
/// Favourites come first as a built-in shelf — they're the one grouping
/// that exists before anyone makes a collection, and Steam puts them at
/// the top for the same reason. A favourite still appears in whatever
/// collections it belongs to; being starred isn't a collection, it's a
/// mark on top of one.
///
/// With no collections defined, falls back to the install-state split —
/// otherwise a fresh library would show as one undifferentiated pile.
/// With collections, every one gets a shelf even when empty (you just
/// made it; it should appear), and whatever belongs to none of them lands
/// in a trailing bucket.
pub(super) fn build(
    entries: &[LibraryEntry],
    collections: &Collections,
    favorites: &std::collections::HashSet<String>,
    stats: &LibraryStats,
    pending_installs: &HashSet<String>,
) -> Vec<Shelf> {
    let favorite_entries: Vec<LibraryEntry> = entries
        .iter()
        .filter(|entry| favorites.contains(&entry.game.id))
        .cloned()
        .collect();
    let favorites_shelf = (!favorite_entries.is_empty()).then(|| Shelf {
        id: "favorites".to_string(),
        title: "Favoritos".to_string(),
        entries: favorite_entries,
        collection_id: None,
        dynamic: false,
    });

    if collections.list.is_empty() {
        let (installed, rest): (Vec<_>, Vec<_>) = entries
            .iter()
            .cloned()
            .partition(|entry| is_installed(&entry.game, pending_installs));
        return favorites_shelf
            .into_iter()
            .chain([
                Shelf {
                    id: "installed".to_string(),
                    title: "Instalados".to_string(),
                    entries: installed,
                    collection_id: None,
                    dynamic: false,
                },
                Shelf {
                    id: "all".to_string(),
                    title: "Biblioteca GOG".to_string(),
                    entries: rest,
                    collection_id: None,
                    dynamic: false,
                },
            ])
            .filter(|shelf| !shelf.entries.is_empty())
            .collect();
    }

    let mut shelves: Vec<Shelf> = favorites_shelf
        .into_iter()
        .chain(collections.list.iter().map(|collection| {
            Shelf {
                id: collection.id.clone(),
                title: collection.name.clone(),
                entries: match collection.filter.as_deref().map(SavedFilter::decode) {
                    // Dynamic: membership is whatever the saved filter matches
                    // right now, recomputed every render.
                    Some(Some(saved)) => entries
                        .iter()
                        .filter(|entry| saved.accepts(entry, favorites, stats, collections, pending_installs))
                        .cloned()
                        .collect(),
                    // A filter that no longer parses matches nothing, rather
                    // than silently behaving like an unfiltered shelf.
                    Some(None) => Vec::new(),
                    None => entries
                        .iter()
                        .filter(|entry| collections.contains(&collection.id, &entry.game.id))
                        .cloned()
                        .collect(),
                },
                dynamic: collection.filter.is_some(),
                collection_id: Some(collection.id.clone()),
            }
        }))
        .collect();

    let uncategorized: Vec<LibraryEntry> = entries
        .iter()
        .filter(|entry| collections.is_uncategorized(&entry.game.id))
        .cloned()
        .collect();
    if !uncategorized.is_empty() {
        shelves.push(Shelf {
            id: "uncategorized".to_string(),
            title: "Sin coleccion".to_string(),
            entries: uncategorized,
            collection_id: None,
            dynamic: false,
        });
    }

    shelves
}
