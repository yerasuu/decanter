//! Keeps exactly one `Library.WatchGames` stream alive per active profile,
//! owned at the app root instead of by the `Library` view — so leaving and
//! coming back to Biblioteca doesn't throw the fetched library away.
//!
//! Still seeds from the local cache on startup (see `stream`'s own doc for
//! why), but doesn't trust it to paint the screen on its own any more:
//! every activation starts `Loading`, and `Library` shows a loading
//! screen instead of the real one until this session's first full sync
//! actually lands — Steam's own shape, not the previous "flash the old
//! cache, then silently rewrite it underneath you" one.

use achv_core::InstallState;
use achv_gog::LibraryEntry;
use dioxus::core::Task;
use dioxus::prelude::*;

mod cache;
mod stream;

/// Whether this game is installed, from this profile's appmanifest. The
/// one part of a `Game` that isn't catalog data, and the only part that
/// changes without a trip to GOG.
pub(crate) fn local_install_state(profile_id: &str, product_id: &str) -> Option<InstallState> {
    let Some(installed) = crate::core::installed_game(profile_id, product_id) else {
        return None;
    };
    Some(InstallState {
        installed: true,
        installed_version: Some(installed.build_id),
        install_size_bytes: installed.size_on_disk,
        ..Default::default()
    })
}

#[derive(Clone, PartialEq, Eq)]
pub enum LibraryState {
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

// `PartialEq` so it can be passed as a component prop; the signals it
// holds are handles, so comparing them compares identity, not contents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GogLibrary {
    pub games: Signal<Vec<LibraryEntry>>,
    pub state: Signal<LibraryState>,
    pub generation: Signal<u32>,
}

impl GogLibrary {
    /// Restarts the fetch immediately instead of waiting out the refresh
    /// interval — for when something we did (signing in or out) means the
    /// library just changed.
    pub fn refresh(mut self) {
        let next = self.generation.peek().wrapping_add(1);
        self.generation.set(next);
    }

    /// Re-reads one game's local install state after an install or
    /// uninstall.
    ///
    /// Not a full [`refresh`](Self::refresh): that re-resolves every owned
    /// game's catalog data over the network and takes tens of seconds,
    /// while the only thing that changed is a local record. A server used
    /// to push this to us over `WatchGames`; nothing does now, so without
    /// this the game keeps showing its pre-install state until the next
    /// scheduled fetch.
    pub fn refresh_install_state(mut self, profile_id: &str, product_id: &str) {
        let install_state = local_install_state(profile_id, product_id);

        if let Some(entry) = self.games.write().iter_mut().find(|entry| entry.game.id == product_id) {
            entry.game.install_state = install_state;
        }
    }
}

/// Call once, at the app root. Restarts the stream only when the active
/// profile actually changes — not on every route/component mount.
pub fn use_gog_library(active_profile: Signal<Option<String>>) -> GogLibrary {
    let mut games = use_signal(Vec::<LibraryEntry>::new);
    let mut state = use_signal(|| LibraryState::Idle);
    let mut current_task = use_signal(|| None::<Task>);
    let generation = use_signal(|| 0_u32);

    use_effect(move || {
        let profile_id = active_profile.read().clone();
        // Subscribes this effect to `refresh()` as well as profile changes.
        let _ = generation();

        if let Some(task) = current_task.write().take() {
            task.cancel();
        }

        match profile_id {
            None => {
                games.write().clear();
                state.set(LibraryState::Idle);
            }
            Some(profile_id) => {
                // Seeded from cache, but still `Loading` regardless of
                // whether that seed is empty — a real second (or
                // reopened) session shouldn't have to wait, but flashing
                // a maybe-stale, maybe-incomplete list and then visibly
                // rewriting it as the fetch lands is exactly the flicker
                // this was rewritten to avoid. `stream::watch_games`
                // moves this to `Loaded` only once this session's first
                // full fetch (list + catalog enrichment) is actually
                // done, Steam's own "Syncing..." shape.
                let cached = cache::load_cached_games(&profile_id);
                games.set(cached);
                state.set(LibraryState::Loading);
                let task = spawn(stream::watch_games(profile_id, games, state));
                current_task.set(Some(task));
            }
        }
    });

    GogLibrary {
        games,
        state,
        generation,
    }
}
