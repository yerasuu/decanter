//! Every screen, and the main window that hosts them.

mod chrome;
mod downloads;
mod dropped;
pub mod external;
mod game_tools;
mod gog_login;
pub mod library;
mod profile_form;
mod ps;
mod raven;
pub mod session_renewal;
mod settings;
mod shell;
mod storage;
mod unavailable;

use std::cell::Cell;
use std::rc::Rc;

use decanter_core::capabilities::Feature;

use gpui::{
    App, AppContext as _, Context, Entity, Global, Subscription, Window, WindowHandle, div, prelude::*, px, rgb,
};

use crate::capabilities::Caps;
use crate::state::AppState;
use crate::theme;
use profile_form::ProfileForm;
use shell::Shell;
use unavailable::Unavailable;

/// The main window's root: the first-run onboarding until a profile
/// exists, the app shell after, or a way out when the library isn't there.
pub struct Root {
    shell: Option<Entity<Shell>>,
    onboarding: Option<(Entity<ProfileForm>, Subscription)>,
    unavailable: Option<Entity<Unavailable>>,
}

impl Root {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = AppState::get(cx).session.clone();
        cx.observe_in(&session, window, |this, _, window, cx| this.route(window, cx))
            .detach();
        let mut this = Self {
            shell: None,
            onboarding: None,
            unavailable: None,
        };
        this.route(window, cx);
        this
    }

    /// Re-checked whenever the session changes, which covers both a
    /// profile getting created here and one picked from the switcher.
    fn route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = decanter_core::bootstrap::unavailable_root() {
            self.onboarding = None;
            if self.unavailable.is_none() {
                self.unavailable = Some(cx.new(|_| Unavailable::new(root)));
            }
            cx.notify();
            return;
        }
        self.unavailable = None;
        let ready = AppState::get(cx).session.read(cx).core.is_ok();
        // Storage that didn't open falls through to the shell, which
        // already shows that error on every screen, rather than onboarding
        // into a database that can't be written.
        let needs_onboarding = decanter_core::bootstrap::location() == decanter_core::bootstrap::Location::Unconfigured
            || ready
                && crate::core::db()
                    .map(|db| db.list_profiles().unwrap_or_default().is_empty())
                    .unwrap_or(false);
        if needs_onboarding {
            if self.onboarding.is_none() {
                let form = cx.new(|cx| ProfileForm::new(profile_form::Layout::Onboarding, window, cx));
                let subscription = cx.subscribe_in(&form, window, |this, _, _: &profile_form::Done, window, cx| {
                    this.route(window, cx);
                });
                self.onboarding = Some((form, subscription));
            }
        } else {
            self.onboarding = None;
            if self.shell.is_none() {
                let on_close: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(close_main_window);
                self.shell = Some(cx.new(|cx| Shell::new(on_close, window, cx)));
            }
        }
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(theme::BG_DEEP))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .font_family(theme::SANS)
            .text_size(px(14.))
            .line_height(gpui::relative(1.5))
            .map(|root| match (&self.unavailable, &self.onboarding, &self.shell) {
                (Some(unavailable), _, _) => root.child(unavailable.clone()),
                (None, Some((form, _)), _) => root.child(form.clone()),
                (None, None, Some(shell)) => root.child(shell.clone()),
                (None, None, None) => root,
            })
    }
}

/// How the process ends: read once at startup from the database, since
/// toggling it in Ajustes only takes effect on the next launch.
pub struct Lifecycle {
    pub(crate) close_to_tray: bool,
    main: Option<WindowHandle<Root>>,
    /// Set while the close button is stopping games, so the window going
    /// away first doesn't quit before they're stopped.
    stopping: Rc<Cell<bool>>,
}

impl Global for Lifecycle {}

impl Lifecycle {
    /// The setting, as long as a tray is there to close into: without a
    /// `StatusNotifier` host, found only after startup, the window would
    /// close into nothing.
    fn closes_to_tray(&self, cx: &App) -> bool {
        self.close_to_tray && Caps::get(cx).is_offered(Feature::CloseToTray)
    }

    pub fn init(close_to_tray: bool, cx: &mut App) {
        cx.set_global(Self {
            close_to_tray,
            main: None,
            stopping: Rc::new(Cell::new(false)),
        });
        // A window closed some other way (the compositor's own close,
        // Alt+F4) just ends decanter — games outlive it by design, the
        // same "just exit" the webview build did — unless decanter lives
        // on in the tray.
        cx.on_window_closed(|cx, closed| {
            let lifecycle = cx.global::<Self>();
            let was_main = lifecycle.main.is_some_and(|main| main.window_id() == closed);
            if !was_main {
                return;
            }
            let quit = !lifecycle.closes_to_tray(cx) && !lifecycle.stopping.get();
            cx.global_mut::<Self>().main = None;
            if quit {
                cx.quit();
            }
        })
        .detach();
    }
}

/// Opens the main window, or brings it back to the front if it's open.
pub fn show_main_window(cx: &mut App) {
    if let Some(main) = cx.global::<Lifecycle>().main
        && main.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let options = chrome::window_options("Decanter", 1280., 820., 900., 600., cx);
    match cx.open_window(options, |window, cx| cx.new(|cx| Root::new(window, cx))) {
        Ok(handle) => cx.global_mut::<Lifecycle>().main = Some(handle),
        Err(err) => {
            tracing::error!(%err, "cannot open the main window");
            cx.quit();
        }
    }
}

/// The ✕ in the topbar: stops every running game, then closes — or, with
/// "cerrar a la bandeja" on, stops them and leaves decanter in the tray.
fn close_main_window(window: &mut Window, cx: &mut App) {
    let lifecycle = cx.global::<Lifecycle>();
    let close_to_tray = lifecycle.closes_to_tray(cx);
    let stopping = Rc::clone(&lifecycle.stopping);
    stopping.set(true);
    // Gone first so closing feels immediate: stopping a game means asking
    // it to exit and waiting for it, and a window sitting there
    // unresponsive meanwhile reads as a hang.
    window.remove_window();
    cx.spawn(async move |cx| {
        crate::launch::stop_everything().await;
        stopping.set(false);
        if !close_to_tray {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}
