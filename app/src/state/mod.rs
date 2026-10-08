//! App-wide state, one entity per concern so a view only re-renders for
//! what it reads: install progress ticking several times a second
//! shouldn't re-run the sidebar's sqlite reads.

pub mod background;
mod cache;
pub mod gog_library;
mod ps;

use decanter_core::ProfileKind;
use gpui::{App, AppContext as _, Entity, Global};

pub use background::{Background, InstallStatus};
pub use gog_library::{GogLibrary, LibraryState};
pub use ps::{DllOutcome, JobKey, Ps, Stage};

/// Whether storage came up, and which profile is active.
pub struct Session {
    /// Resolved once at startup; screens show the error instead of their
    /// content when the database couldn't be opened.
    pub core: Result<(), String>,
    /// The profile the topbar's switcher has active — read by everything
    /// that's per-profile instead of each screen picking its own.
    pub active_profile: Option<String>,
    /// Which sections the topbar and Ajustes show. Set together with
    /// `active_profile`; `Pc` while no profile is active.
    pub active_kind: ProfileKind,
    /// Bumped whenever the topbar's "Biblioteca" link is clicked, even
    /// while already on that page, so the library drops back to the shelf
    /// browser instead of leaving whatever game detail was open.
    pub library_home: u32,
}

impl Session {
    fn load() -> Self {
        let core = crate::core::db().map(|_| ());
        let active = crate::core::db()
            .and_then(|db| db.active_profile().map_err(|err| err.to_string()))
            .inspect_err(|err| tracing::warn!(%err, "could not read the active profile"))
            .ok()
            .flatten();
        Self {
            core,
            active_kind: active.as_ref().map_or(ProfileKind::Pc, |profile| profile.kind),
            active_profile: active.map(|profile| profile.id),
            library_home: 0,
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
        cx.set_global(Self {
            session,
            background,
            library,
        });
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}
