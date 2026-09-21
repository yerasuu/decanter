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

    // Only the GUI path below ever creates a webview, so only it needs
    // nix-shell's own graphics env restored — see the function's own doc.
    #[cfg(all(feature = "desktop", target_os = "linux"))]
    relaunch_with_nix_env();

    // No native title bar: `Navbar`'s own topbar draws the drag region
    // and the minimize/maximize/close controls instead, so there's one
    // bar at the top of the window rather than the OS's plus ours
    // stacked on each other. Turning decorations off also takes the
    // resize frame with them, so `Navbar` draws its own edge strips —
    // `with_resizable` is what makes those strips do anything.
    #[cfg(feature = "desktop")]
    {
        // `Config::with_data_directory` below only reaches WebKitGTK's
        // indexeddb/localStorage/media-keys storage -- its HSTS enforcer and
        // favicon database ignore it outright, and its disk cache
        // (`WebKitCache`, `CacheStorage`) has no override at all in `wry`
        // (see `wry::webkitgtk::WebContext::new`, which only wires
        // `base_data_directory`). All of those fall back to GLib's own
        // `g_get_user_data_dir()`/`g_get_user_cache_dir()` joined with
        // `g_get_prgname()`, which defaults to `argv[0]`'s basename -- "decanter"
        // for a plain binary, but the literal AppImage filename (e.g.
        // "decanter_0.1.0_x86_64.AppImage") when launched that way. Pinning it
        // here keeps those leftover directories stable across how the binary
        // is invoked, instead of scattering a fresh pair under
        // `~/.local/share` and `~/.cache` for every AppImage version.
        #[cfg(target_os = "linux")]
        glib::set_prgname(Some("decanter"));

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

/// Re-execs this same binary once, with the environment WebKitGTK's own
/// `WebProcess` needs, restored.
///
/// `ui`'s own release binary is built under this project's nix-shell (see
/// `../shell.nix`) — that's also true of the AppImage `dx bundle` produces
/// from it, since linuxdeploy only bundles this binary's own *linked*
/// dependencies, never the display stack a webview dlopens at runtime
/// (mesa's GL/EGL drivers) or the helper process WebKitGTK spawns for its
/// own content process (`WebKitWebProcess`, resolved by an absolute path
/// baked into `libwebkit2gtk` itself, not something this crate controls).
/// Both still come from that exact nix-shell snapshot no matter how the
/// binary is launched, so outside the shell — a plain double-click, or
/// the AppImage — `WebKitWebProcess` aborts trying to create its EGL
/// display: it can't find its own GL/EGL libraries without nix-shell's own
/// `LD_LIBRARY_PATH`, confirmed the hard way against the exact crash
/// (`PlatformDisplayDefault::create` aborting with `EGL_BAD_PARAMETER`)
/// this restores. Same story for glib-networking's TLS backend and CA
/// bundle: `game.cover_url` (see `views::library::browser`) points
/// straight at GOG's own CDN, rendered through a plain `<img src>` the
/// webview fetches itself — without these, that request fails TLS
/// validation with nothing logged anywhere in reach of this process.
///
/// This can't fix that by setting env vars in-process: `std::env::set_var`
/// is `unsafe` (races with any other thread reading the environment), and
/// this workspace forbids unsafe code outright. Re-executing with a
/// corrected environment needs none — `CommandExt::exec` replaces this
/// process's image in place (same pid, same parent), so anything watching
/// this process (`dx serve`'s own dev server included) never sees the
/// difference. The sentinel stops a second relaunch once that's already
/// applied.
#[cfg(all(feature = "desktop", target_os = "linux"))]
fn relaunch_with_nix_env() {
    use std::os::unix::process::CommandExt as _;

    const SENTINEL: &str = "DECANTER_NIX_ENV_APPLIED";
    if std::env::var_os(SENTINEL).is_some() {
        return;
    }
    let nix_ld_library_path = env!("DECANTER_NIX_LD_LIBRARY_PATH");
    if nix_ld_library_path.is_empty() {
        // Built outside nix-shell (see the README's "Without Nix" section)
        // — the host's own webkitgtk/mesa are already on its normal
        // search paths, nothing to restore.
        return;
    }
    let Ok(current_exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(current_exe);
    command
        .args(std::env::args_os().skip(1))
        .env(SENTINEL, "1")
        .env("LD_LIBRARY_PATH", nix_ld_library_path)
        .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
        .env("WEBKIT_DISABLE_COMPOSITING_MODE", "1")
        .env("WEBKIT_DISABLE_SANDBOX", "1")
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env("GALLIUM_DRIVER", "llvmpipe");
    let nix_gio_extra_modules = env!("DECANTER_NIX_GIO_EXTRA_MODULES");
    if !nix_gio_extra_modules.is_empty() {
        command.env("GIO_EXTRA_MODULES", nix_gio_extra_modules);
    }
    let nix_ssl_cert_file = env!("DECANTER_NIX_SSL_CERT_FILE");
    if !nix_ssl_cert_file.is_empty() {
        command.env("SSL_CERT_FILE", nix_ssl_cert_file);
    }
    let err = command
        .exec();
    // `exec` only returns on failure — fall through and run unpatched
    // rather than exit outright over a fixup that's best-effort anyway.
    tracing::warn!(%err, "failed to relaunch with the nix-shell graphics environment restored");
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
                //
                // Not `std::process::exit`: see the matching comment in
                // `launch::exit_with_games_on_signal` on why that races
                // OpenSSL's own `atexit` cleanup against a still-in-flight
                // background GOG call. `set_close_behavior` first, since this
                // branch only exists while the window is set to hide instead
                // of close.
                "decanter-tray-quit" => {
                    desktop.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
                    desktop.close();
                }
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
                    // Not `std::process::exit`: same OpenSSL/atexit race as
                    // the other tray-quit path above.
                    Some(tray_linux::TrayCommand::Quit) => {
                        desktop.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
                        desktop.close();
                    }
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
