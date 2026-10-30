//! App-wide state, one entity per concern so a view only re-renders for
//! what it reads: install progress ticking several times a second
//! shouldn't re-run the sidebar's sqlite reads.

pub mod background;
mod cache;
pub mod gog_library;
mod ps;
mod raven;

use gpui::{App, AppContext as _, Entity, Global};

use crate::i18n::Text;
use crate::views::settings::Tab as SettingsTab;

pub use background::{Background, Download, DownloadKey, DownloadKind, InstallStatus};
pub use gog_library::{GogLibrary, LibraryState, ModsState};
pub use ps::{
    Adding as PsAdding, DllOutcome, Fallback, JobKey, Offer as PsOffer, PortState, Ps, Stage, Track, failure_message,
    folder_error as ps_folder_error, job_label, job_target, online_error, per_game_error, vk_layer_unavailable_text,
};
pub(crate) use ps::{PortHost, move_checkout as move_port_checkout, update_and_prepare as update_and_prepare_port};
pub use raven::{Built as RavenBuilt, Raven};

/// Whether storage came up, and which profile is active.
pub struct Session {
    /// Resolved once at startup; screens show the error instead of their
    /// content when the database couldn't be opened.
    pub core: Result<(), Text>,
    /// The profile the topbar's switcher has active — read by everything
    /// that's per-profile instead of each screen picking its own.
    pub active_profile: Option<String>,
    /// Bumped whenever the topbar's "Biblioteca" link is clicked, even
    /// while already on that page, so the library drops back to the shelf
    /// browser instead of leaving whatever game detail was open.
    pub library_home: u32,
    /// The same for the PS section: back from a game to its covers.
    pub ps_home: u32,
    /// Bumped to ask the shell for the "Juegos externos" page — from the
    /// library's sidebar or a game's menu, neither of which owns the shell.
    pub external_request: u32,
    /// Bumped to ask the shell for Descargas, where a cancel asked for
    /// from a game's menu is confirmed.
    pub downloads_request: u32,
    /// The external game that request is to edit, if any.
    pub external_edit: Option<String>,
    /// The Ajustes tab some other view asks the shell to open; Ajustes clears
    /// it once it has switched to it.
    pub settings_request: Option<SettingsTab>,
    /// Whether the player switched the Raven section on in Ajustes; it shows
    /// only where Raven is offered too.
    pub raven: bool,
}

impl Session {
    /// After the active library changed: everything here came from the
    /// database of the one before.
    pub fn reload(&mut self) {
        let fresh = Self::load();
        self.core = fresh.core;
        self.active_profile = fresh.active_profile;
        self.raven = fresh.raven;
    }

    fn load() -> Self {
        let core = crate::core::db().map(|_| ()).map_err(Text::from);
        let active_profile = crate::core::db()
            .and_then(|db| db.active_profile().map_err(crate::core::StoreError::Query))
            .inspect_err(|err| tracing::warn!(%err, "could not read the active profile"))
            .ok()
            .flatten()
            .map(|profile| profile.id);
        let raven = crate::core::db()
            .and_then(|db| db.raven_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(|err| tracing::warn!(%err, "could not read the Raven switch, leaving the section off"))
            .unwrap_or(false);
        Self {
            core,
            active_profile,
            library_home: 0,
            ps_home: 0,
            external_request: 0,
            downloads_request: 0,
            external_edit: None,
            settings_request: None,
            raven,
        }
    }
}

/// Handles to every shared entity, reachable from any window.
#[derive(Clone)]
pub struct AppState {
    pub session: Entity<Session>,
    pub background: Entity<Background>,
    pub library: Entity<GogLibrary>,
}

impl Global for AppState {}

impl AppState {
    pub fn init(cx: &mut App) {
        let session = cx.new(|_| Session::load());
        let background = cx.new(|_| Background::default());
        let library = cx.new(|cx| GogLibrary::new(&session, cx));
        let state = Self {
            session,
            background,
            library,
        };
        let watched = state.clone();
        cx.spawn(async move |cx| crate::views::library::unfinished::watch(watched, cx).await)
            .detach();
        cx.set_global(state);
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}
