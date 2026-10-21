use decanter_gog::LibraryEntry;
use gpui::SharedString;
use rust_i18n::t;

use super::{Capsule, Derived, Organisation, ShelfData, ShelvesKey, Slot};
use crate::views::library::filters::{belongs_in_the_library, is_installed, sort_entries};
use crate::views::library::shelves;

pub(super) fn derive(key: ShelvesKey, games: &[LibraryEntry], organisation: &Organisation, slots: &[Slot]) -> Derived {
    let Organisation {
        collections,
        favorites,
        hidden,
        stats,
    } = organisation;
    let pending_installs = &key.pending_installs;
    // Only real games: a shelf of DLC capsules is a wall of near-identical
    // art for things that can't be launched.
    let entries: Vec<LibraryEntry> = games
        .iter()
        .filter(|entry| {
            entry.kind == decanter_gog::GameKind::Game && entry.parent_id.is_none() && belongs_in_the_library(entry)
        })
        .filter(|entry| !hidden.contains(&entry.game.id))
        .filter(|entry| !key.installed_only || is_installed(&entry.game, pending_installs))
        .cloned()
        .collect();
    let built = shelves::build(&entries, collections, favorites, stats, pending_installs);
    // Built-ins first, then the user's own collections — the order the
    // design prototype settled on, so the two don't drift.
    //
    // Listed unconditionally rather than picked out of `built`, which
    // drops a shelf the moment it's empty: a slot pointed at Favoritos
    // used to lose its own option the moment the last star came off, and
    // Instalados vanished as soon as a first collection retired the
    // install-state split. Either left the shelf blank with no way back.
    let mut sources: Vec<(SharedString, SharedString)> = vec![
        ("favorites".into(), t!("library.shelves.favorites").into()),
        ("all".into(), t!("library.browser.whole_library").into()),
        ("uncategorized".into(), t!("library.shelves.uncategorized").into()),
        ("installed".into(), t!("library.shelves.installed").into()),
    ];
    sources.extend(
        built
            .iter()
            .filter(|shelf| shelf.collection_id.is_some())
            .map(|shelf| (shelf.id.clone().into(), shelf.title.clone().into())),
    );

    let shelves = slots
        .iter()
        .map(|slot| {
            let pick = |keep: &dyn Fn(&LibraryEntry) -> bool| entries.iter().filter(|e| keep(e)).cloned().collect();
            let mut items: Vec<LibraryEntry> = match slot.source.as_str() {
                "all" => entries.clone(),
                "favorites" => pick(&|e| favorites.contains(&e.game.id)),
                "uncategorized" => pick(&|e| collections.is_uncategorized(&e.game.id)),
                "installed" => pick(&|e| is_installed(&e.game, pending_installs)),
                id => built
                    .iter()
                    .find(|shelf| shelf.id == id)
                    .map(|shelf| shelf.entries.clone())
                    .unwrap_or_default(),
            };
            sort_entries(&mut items, slot.sort, stats);
            let capsules: Vec<Capsule> = items
                .into_iter()
                .map(|entry| {
                    let game = entry.game;
                    Capsule {
                        meta: capsule_meta(stats.seconds(&game.id), stats.completion(&game.id)).into(),
                        installed: is_installed(&game, pending_installs),
                        favorite: favorites.contains(&game.id),
                        cover: game.cover_url.map(SharedString::from),
                        title: game.title.into(),
                        id: game.id.into(),
                    }
                })
                .collect();
            ShelfData {
                key: slot.key,
                source: slot.source.clone().into(),
                sort: slot.sort,
                installed: capsules.iter().filter(|capsule| capsule.installed).count(),
                capsules,
                dynamic: built.iter().any(|shelf| shelf.id == slot.source && shelf.dynamic),
            }
        })
        .collect();

    Derived {
        key,
        library_empty: entries.is_empty(),
        sources,
        shelves,
    }
}

/// The line under a capsule: time played and how much of the achievement
/// set is done. A game never launched says so rather than showing `0.0 h`,
/// and one with nothing cached simply omits the percentage — it isn't 0%
/// of anything.
fn capsule_meta(seconds: i64, completion: Option<f64>) -> String {
    let played = match seconds {
        0 => t!("library.never_played").into_owned(),
        _ if seconds < 3600 => t!("library.played.minutes", minutes = (seconds / 60).max(1)).into_owned(),
        _ => t!(
            "library.played.hours",
            hours = format!("{:.1}", seconds as f64 / 3600.0)
        )
        .into_owned(),
    };
    match completion {
        Some(value) => format!("{played} · {:.0}%", value * 100.0),
        None => played,
    }
}
