#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
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
    reason = "to_string() on &str values reads the same as to_owned() here"
)]
#![expect(
    clippy::single_call_fn,
    reason = "render helpers are split out to name a region of the layout, not because they're reused"
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
    reason = "GPUI builders take impl Into<ElementId>/impl IntoElement by convention; a generic \
              type param would add nothing a caller needs to name"
)]
#![expect(
    clippy::pattern_type_mismatch,
    reason = "matching by-value on Copy state (Option<T>, enums) throughout the UI reads clearer \
              than threading extra references through every match arm"
)]
#![expect(
    clippy::use_self,
    reason = "spelling out the type name in impls reads clearer than Self here"
)]
#![expect(
    clippy::let_underscore_untyped,
    reason = "the dropped value's type is obvious from the right-hand side at every one of these \
              call sites"
)]
#![expect(
    clippy::as_conversions,
    reason = "numeric casts here are widening/lossless UI measurements (f64/f32 pixel math), not \
              the truncating kind this lint is meant to catch"
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
    reason = "UI layout/measurement code is f32/f64 pixel math throughout by design"
)]

use clap::Parser as _;

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

mod art;
mod cli;
mod closing;
mod core;
mod desktop;
mod external;
mod format;
mod game_tools;
mod i18n;
mod launch;
mod logs;
mod releases;
mod relocate;
mod run_log;
mod self_update;
mod state;
mod taskbar;
mod theme;
#[cfg(target_os = "linux")]
mod tray_linux;
mod views;
mod widgets;

rust_i18n::i18n!("locales", fallback = "es");

fn main() {
    logs::init();
    let cli = cli::Cli::parse();
    open_library(cli.library);
    logs::open();

    // A recognized subcommand runs headless and exits before any of the
    // GUI setup below ever runs — see `cli`. No subcommand (the ordinary
    // double-click launch) falls through to the GUI unchanged.
    if let Some(cmd) = cli.cmd {
        cli::run(cmd);
    }

    // Everything on this runtime waits on the network or disk; the default
    // of one worker per core (32 here) only bought idle threads, each with
    // a stack and allocator state of its own.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(%err, "cannot start the tokio runtime");
            return;
        }
    };
    // Entered on the UI thread for the app's whole life, so `decanter-gog`'s
    // and `decanter-runtime`'s futures (reqwest, tokio timers and fs,
    // `spawn_blocking`) can be awaited straight from GPUI tasks: they
    // register with this runtime's reactor, which its worker threads drive.
    let tokio_context = runtime.enter();
    art::init(runtime.handle().clone());
    runtime.spawn(taskbar::sweep());

    // Only Linux has a tray to close into; a library brought from there may
    // still have the setting on.
    let close_to_tray = cfg!(target_os = "linux") && close_to_tray();
    i18n::init();
    gpui_platform::application()
        .with_quit_mode(gpui::QuitMode::Explicit)
        .run(move |cx| {
            if let Err(err) = theme::load_fonts(cx) {
                tracing::error!(%err, "cannot load bundled fonts");
            }
            widgets::input::bind_keys(cx);
            state::AppState::init(cx);
            views::Lifecycle::init(close_to_tray, cx);

            // Once, at the root: a game keeps running after decanter is
            // gone unless something goes and stops it.
            cx.spawn(async move |cx| launch::exit_with_games_on_signal(cx).await)
                .detach();
            #[cfg(target_os = "linux")]
            if close_to_tray {
                spawn_tray(cx);
            }
            views::show_main_window(cx);
        });

    state::gog_library::shut_down_fetch_loops();
    drop(tokio_context);
    // Dropping the runtime blocks until outstanding `spawn_blocking` work
    // finishes — the reason quitting goes through here rather than
    // `std::process::exit` (see `launch::exit_with_games_on_signal`).
    drop(runtime);
    self_update::restart_if_requested();
}

/// Before anything reads the database, for the GUI and the CLI alike: the
/// one-time move into the library, then the lock every running decanter
/// holds.
fn open_library(chosen: Option<std::path::PathBuf>) {
    if let Some(root) = chosen {
        decanter_core::bootstrap::set_override(root);
    }
    #[cfg(not(windows))]
    let migrated = decanter_core::migrate::run(Some(decanter_runtime::umu_run_path));
    #[cfg(windows)]
    let migrated = decanter_core::migrate::run(None);
    match migrated {
        Ok(outcome) => tracing::debug!(?outcome, "checked for data to migrate"),
        Err(err) => tracing::error!(%err, "cannot move decanter's data into the library, still using the old folder"),
    }
    decanter_core::bootstrap::hold_shared();
}

/// Whether closing the window should leave decanter in the tray instead of
/// quitting — read once, before the window exists. Toggling the setting
/// in Ajustes only takes effect on the next launch.
fn close_to_tray() -> bool {
    let value = core::db()
        .and_then(|db| db.close_to_tray().map_err(crate::core::StoreError::Query))
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "could not read close_to_tray setting");
            false
        });
    tracing::debug!(close_to_tray = value, "resolved close-to-tray setting");
    value
}

/// GPUI has no tray of its own; `tray_linux` runs one over D-Bus and
/// reports clicks back through a channel this loop drains.
#[cfg(target_os = "linux")]
fn spawn_tray(cx: &mut gpui::App) {
    let mut commands = tray_linux::spawn();
    cx.spawn(async move |cx| {
        while let Some(command) = commands.recv().await {
            match command {
                tray_linux::TrayCommand::Show => cx.update(views::show_main_window),
                // Games outlive decanter by design (see
                // `launch::stop_everything`'s doc) — quitting from the tray
                // is the same "just exit" a window close was before this
                // setting existed.
                tray_linux::TrayCommand::Quit => cx.update(|cx| cx.quit()),
            }
        }
    })
    .detach();
}
