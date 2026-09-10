#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
// Only fires on some compilation targets (bin vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::large_stack_frames,
    reason = "dioxus's rsx! expansion and event-handler closures cost more than the 512-byte \
              threshold by design"
)]
#![expect(
    clippy::absolute_paths,
    reason = "one-off calls read clearer qualified at the call site than imported for a single use"
)]
#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "items are ordered by what makes sense to read together, not alphabetically"
)]
#![expect(
    clippy::non_ascii_literal,
    reason = "this app's UI text is Spanish; accented characters are content, not a lint target"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private items get a doc comment when the WHY isn't obvious from the signature, \
              not as a blanket requirement — see CLAUDE.md's comment philosophy"
)]
#![expect(
    clippy::str_to_string,
    reason = "to_string() on &str values from props/signals reads the same as to_owned() here"
)]
#![expect(
    clippy::single_call_fn,
    reason = "dioxus components are one function each by the framework's own convention, not \
              because they're only called once"
)]
#![expect(
    clippy::shadow_reuse,
    reason = "rebinding a clone under the same name before moving it into a closure is the \
              established pattern in this UI's event handlers, not accidental shadowing"
)]
#![expect(
    clippy::pub_with_shorthand,
    reason = "this crate uses the pub(crate)/pub(super) shorthand everywhere, not pub(in path)"
)]
#![expect(
    clippy::impl_trait_in_params,
    reason = "dioxus component props use impl Into<T> by convention (e.g. Prefix::new-style \
              builders); a generic type param would add nothing a caller needs to name"
)]
#![expect(
    clippy::same_name_method,
    reason = "dioxus's #[component] macro generates an inherent method with the same name as a \
              trait method on the props type; not a naming collision to fix"
)]
#![expect(
    clippy::pattern_type_mismatch,
    reason = "matching by-value on Copy signal reads (Option<T>, enums) throughout the UI reads \
              clearer than threading extra references through every match arm"
)]
#![expect(
    clippy::use_self,
    reason = "spelling out the type name in impls reads clearer than Self here, especially in \
              generated dioxus component code"
)]
#![expect(
    clippy::let_underscore_untyped,
    reason = "the dropped value's type is obvious from the right-hand side at every one of these \
              call sites"
)]
#![expect(
    clippy::as_conversions,
    reason = "numeric casts here are widening/lossless UI measurements (f64 pixel math), not the \
              truncating kind this lint is meant to catch"
)]
#![expect(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "desktop app, no no_std/no-alloc target exists to justify core::/alloc:: here"
)]
#![expect(
    clippy::missing_trait_methods,
    reason = "ksni::Tray's un-overridden methods use its own sensible defaults (see tray_linux.rs)"
)]
#![expect(
    clippy::pub_use,
    clippy::field_scoped_visibility_modifiers,
    clippy::redundant_pub_crate,
    reason = "this crate uses pub(crate)/pub(super) shorthand and re-exports throughout, not \
              pub(in path) or fully-qualified visibility"
)]
#![expect(
    clippy::default_numeric_fallback,
    clippy::float_arithmetic,
    reason = "UI layout/measurement code is f64 pixel math throughout by design"
)]

use clap::Parser as _;
use dioxus::prelude::*;

use views::{Downloads, Library, Navbar, Onboarding, Settings, Storage};

/// Runs a fallible call and returns from the enclosing function with the
/// error instead of using `?` — `clippy::restriction` (`question_mark_used`)
/// forbids the operator project-wide, so this is the pre-`?` `try!` idiom
/// brought back as one macro rather than pasting the same match at every
/// call site. Same shape as `decanter_gog::achv_try`.
#[macro_export]
macro_rules! achv_try {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return Err(err.into()),
        }
    };
}

mod background;
mod cli;
mod core;
mod format;
mod gog_library;
mod launch;
#[cfg(target_os = "linux")]
mod tray_linux;
mod views;

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(Navbar)]
        #[route("/")]
        Library {},
        #[route("/almacenamiento")]
        Storage {},
        #[route("/descargas")]
        Downloads {},
        #[route("/ajustes")]
        Settings {},
}

const FAVICON: Asset = asset!("/assets/favicon.ico");
pub(crate) const MAIN_CSS: Asset = asset!("/assets/styling/main.css");
pub(crate) const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");
/// Shared with any secondary window (see `views::library::properties`) --
/// each is its own document and needs its own copy of these links, since a
/// second `VirtualDom` doesn't inherit anything `App`'s render tree put in
/// this one's `<head>`.
pub(crate) const GOOGLE_FONTS_HREF: &str = "https://fonts.googleapis.com/css2?family=Oswald:wght@400;500;600;700&family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;600;700&display=swap";

fn main() {
    // Before either path below: the CLI subcommands used to be the only
    // one that installed this, which left the ordinary GUI launch with no
    // subscriber at all — every `tracing::info!`/`warn!` in that process
    // silently went nowhere.
    tracing_subscriber::fmt::init();

    // A recognized subcommand runs headless and exits before any of the
    // GUI setup below ever runs — see `cli`. No subcommand (the ordinary
    // double-click launch) falls through to the GUI unchanged.
    if let Some(cmd) = cli::Cli::parse().cmd {
        cli::run(cmd);
    }

    // No native title bar: `Navbar`'s own topbar draws the drag region
    // and the minimize/maximize/close controls instead, so there's one
    // bar at the top of the window rather than the OS's plus ours
    // stacked on each other. Turning decorations off also takes the
    // resize frame with them, so `Navbar` draws its own edge strips —
    // `with_resizable` is what makes those strips do anything.
    #[cfg(feature = "desktop")]
    {
        let mut config = dioxus::desktop::Config::new()
            .with_menu(None)
            .with_data_directory(webview_data_dir())
            .with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_decorations(false)
                    .with_resizable(true)
                    .with_min_inner_size(dioxus::desktop::LogicalSize::new(900.0, 600.0)),
            );
        if close_to_tray() {
            config = config.with_close_behaviour(dioxus::desktop::WindowCloseBehaviour::WindowHides);
        }
        dioxus::LaunchBuilder::new().with_cfg(config).launch(App);
    }
    #[cfg(not(feature = "desktop"))]
    dioxus::launch(App);
}

/// Whether closing the window should hide it behind a tray icon instead of
/// quitting — read once, before the window exists, since `Config` has to
/// carry the answer in from the start rather than switch it mid-flight.
/// Toggling the setting in Ajustes only takes effect on the next launch.
#[cfg(feature = "desktop")]
fn close_to_tray() -> bool {
    let value = core::db()
        .and_then(|db| db.close_to_tray().map_err(|err| err.to_string()))
        .unwrap_or_else(|err| {
            tracing::warn!("could not read close_to_tray setting: {err}");
            false
        });
    tracing::debug!(close_to_tray = value, "resolved close-to-tray setting");
    value
}

/// Where the webview keeps its own state — HSTS cache, media keys, the
/// storage salt.
///
/// Set explicitly because the default isn't one place. On Linux dioxus
/// leaves it unset, so WebKitGTK picks its own directory under
/// `XDG_DATA_HOME` and derives the name itself; with no GTK application id
/// to key off, it lands on a fresh `decanter-<hash>` per build and the data
/// directory fills up with abandoned copies. Naming it puts that state
/// inside decanter's own directory, where the database and the game library
/// already live, and keeps it to one.
#[cfg(feature = "desktop")]
fn webview_data_dir() -> std::path::PathBuf {
    decanter_core::data_dir().join("webview")
}

/// Whether decanter's storage came up, resolved once here and read by
/// every screen via `use_context::<CoreResource>()`. Screens open their
/// own database connection for the query they're making — see `core`.
pub type CoreResource = Resource<Result<core::Core, String>>;

/// The profile the navbar's switcher currently has active — shared so
/// `Library` (and anything else that's per-profile) can react without its
/// own separate profile-picking UI.
pub type ActiveProfile = Signal<Option<String>>;

/// Bumped whenever the navbar's "Biblioteca" link is clicked, even while
/// already on that route (a plain route change wouldn't rerun anything,
/// since `Library` never unmounts). `Library` watches this to drop back to
/// the shelf browser instead of leaving whatever game detail was open.
pub type LibraryHome = Signal<u32>;

#[component]
fn App() -> Element {
    let core: CoreResource = use_resource(core::init);
    use_context_provider(|| core);

    let active_profile: ActiveProfile = use_signal(|| None);
    use_context_provider(|| active_profile);

    // `None` while storage isn't ready to ask yet, so the check below
    // falls through to the ordinary app shell (which already shows its own
    // loading state) rather than flashing onboarding first. Reruns
    // whenever `active_profile` changes, which covers both a profile
    // getting created here and one picked from the switcher.
    let mut needs_onboarding = use_signal(|| None::<bool>);
    use_effect(move || {
        let _ = active_profile();
        if !matches!(&*core.read(), Some(Ok(_))) {
            return;
        }
        let Ok(db) = core::db() else {
            return;
        };
        needs_onboarding.set(Some(db.list_profiles().unwrap_or_default().is_empty()));
    });

    let library_home: LibraryHome = use_signal(|| 0_u32);
    use_context_provider(|| library_home);

    let gog_library = gog_library::use_gog_library(active_profile);
    use_context_provider(|| gog_library);

    let background = background::use_background();
    use_context_provider(|| background);

    // Once, at the root: a game keeps running after decanter is gone
    // unless something goes and stops it.
    #[cfg(feature = "desktop")]
    use_hook(launch::exit_with_games_on_signal);

    // Only when "cerrar a la bandeja" is on — `close_to_tray()` is what
    // `main` used to pick the window's close behaviour, so this and that
    // never disagree about whether the window closing means "gone" or
    // "hidden". Fixed for the process's whole lifetime, so branching on it
    // here never changes which hooks run between renders.
    //
    // Linux gets its own tray (see `tray_linux`) instead of
    // `dioxus::desktop::trayicon` — that backend's icon and menu are both
    // broken there (see that module's doc). Windows/macOS aren't known to
    // have either problem, so they keep the built-in one.
    #[cfg(all(feature = "desktop", not(target_os = "linux")))]
    if close_to_tray() {
        use_hook(|| {
            use dioxus::desktop::trayicon::menu::{Menu, MenuItem, PredefinedMenuItem};

            let menu = Menu::new();
            let show = MenuItem::with_id("decanter-tray-show", "Mostrar Decanter", true, None);
            let quit = MenuItem::with_id("decanter-tray-quit", "Salir", true, None);
            let _ = menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit]);

            let icon = dioxus::desktop::icon_from_memory::<dioxus::desktop::trayicon::DioxusTrayIcon>(include_bytes!(
                "../assets/favicon.ico"
            ))
            .ok();
            dioxus::desktop::trayicon::init_tray_icon(menu, icon);
        });

        let desktop = dioxus::desktop::use_window();
        dioxus::desktop::use_tray_menu_event_handler(move |event| {
            match event.id().0.as_str() {
                "decanter-tray-show" => {
                    desktop.window.set_visible(true);
                    desktop.window.set_focus();
                }
                // Games outlive decanter by design (see `launch::stop_everything`'s
                // doc) — quitting from the tray is the same "just exit" a normal
                // window close already was before this setting existed.
                "decanter-tray-quit" => std::process::exit(0),
                _ => {}
            }
        });
    }

    #[cfg(all(feature = "desktop", target_os = "linux"))]
    if close_to_tray() {
        let desktop = dioxus::desktop::use_window();
        // `use_hook` needs its state `Clone`, which an `UnboundedReceiver`
        // on its own isn't — there's only ever meant to be one consumer,
        // this loop, so an `Rc<RefCell<_>>` around it is just satisfying
        // that bound, not sharing it.
        let commands = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(tray_linux::spawn())));
        spawn(async move {
            loop {
                let command = commands.borrow_mut().recv().await;
                match command {
                    Some(tray_linux::TrayCommand::Show) => {
                        desktop.window.set_visible(true);
                        desktop.window.set_focus();
                    }
                    Some(tray_linux::TrayCommand::Quit) => std::process::exit(0),
                    None => break,
                }
            }
        });
    }

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: GOOGLE_FONTS_HREF }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }

        // The webview's own right-click menu (Reload, Inspect Element...)
        // has no business showing up in an app that ships its own context
        // menus everywhere a right-click means something. This catches
        // whatever a screen's own `oncontextmenu` didn't already handle --
        // blank space, anything without a custom menu -- so the default
        // never leaks through anywhere.
        div {
            style: "height: 100%;",
            oncontextmenu: move |event| event.prevent_default(),
            if needs_onboarding() == Some(true) {
                Onboarding {}
            } else {
                Router::<Route> {}
            }
        }
    }
}
