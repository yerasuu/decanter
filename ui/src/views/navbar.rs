use decanter_core::Profile;
use dioxus::prelude::*;

use crate::{ActiveProfile, CoreResource, LibraryHome, Route};

pub(crate) const NAVBAR_CSS: Asset = asset!("/assets/styling/navbar.css");

#[component]
pub fn Navbar() -> Element {
    let mut library_home = use_context::<LibraryHome>();

    rsx! {
        document::Link { rel: "stylesheet", href: NAVBAR_CSS }

        div { class: "app-shell",
            ResizeEdges {}
            div { class: "topbar",
                // Draggable like the empty space beside it: the app's name
                // is title-bar furniture, not a control, so grabbing it to
                // move the window is what a native title bar would do.
                span {
                    class: "logo",
                    onmousedown: move |_| start_window_drag(),
                    span { class: "dot" }
                    "DECANTER"
                }
                nav { class: "topnav",
                    Link {
                        to: Route::Library {},
                        active_class: "active",
                        onclick: move |_| library_home.set(library_home() + 1),
                        "Biblioteca"
                    }
                    Link { to: Route::Storage {}, active_class: "active", "Almacenamiento" }
                    DownloadsLink {}
                }
                // Empty on purpose: with no native title bar, this is the
                // window's only click-and-drag-to-move surface. A plain
                // child of `.topbar` rather than folded into `.topnav` or
                // `.top-right`, so dragging can't ever start on top of a
                // link, the profile switcher, or the window buttons.
                div { class: "topbar-drag", onmousedown: move |_| start_window_drag() }
                div { class: "top-right",
                    ProfileSwitcher {}
                    Link {
                        to: Route::Settings {},
                        active_class: "active",
                        class: "icon-btn",
                        title: "Ajustes",
                        "⚙"
                    }
                    WindowControls { on_close: move |()| close_main_window() }
                }
            }
            div { class: "app-body",
                Outlet::<Route> {}
            }
            DownloadBar {}
        }
    }
}

/// Carries a count so an install running in the background is visible
/// from anywhere, without having to open the section to find out.
#[component]
fn DownloadsLink() -> Element {
    let crate::background::Background { installing, .. } = use_context::<crate::background::Background>();
    let count = installing.read().len();

    rsx! {
        Link { to: Route::Downloads {}, active_class: "active",
            "Descargas"
            if count > 0 {
                span { class: "nav-count", "{count}" }
            }
        }
    }
}

/// Which profile decanter is working with right now — pick the active one, or
/// switch. Switching swaps every account that profile groups at once (see
/// `Ajustes > Cuenta`, where accounts are added to it one at a time).
///
/// Creating a profile is a step of its own, not something logging into a
/// storefront account does implicitly — the last option in the `<select>`
/// opens a small inline "name it" form instead of switching.
#[component]
fn ProfileSwitcher() -> Element {
    let core = use_context::<CoreResource>();
    let mut active_profile = use_context::<ActiveProfile>();
    let mut profiles = use_signal(Vec::<Profile>::new);
    let mut generation = use_signal(|| 0_u32);
    let mut creating = use_signal(|| false);

    use_effect(move || {
        let _ = generation();
        if !matches!(&*core.read(), Some(Ok(_))) {
            return;
        }
        let Ok(db) = crate::core::db()
            .inspect_err(|err| tracing::warn!(error = %err, "failed to open db, profile switcher shows nothing"))
        else {
            return;
        };
        profiles.set(
            db.list_profiles()
                .inspect_err(|err| tracing::warn!(error = %err, "failed to list profiles"))
                .unwrap_or_default(),
        );
        active_profile.set(
            db.active_profile()
                .inspect_err(|err| tracing::warn!(error = %err, "failed to read active profile"))
                .ok()
                .flatten()
                .map(|profile| profile.id),
        );
    });

    // The active profile's initial, so which account is in play reads at a
    // glance instead of only from the select's current text.
    let initial = active_profile
        .read()
        .as_deref()
        .and_then(|id| profiles().into_iter().find(|profile| profile.id == id))
        .and_then(|profile| profile.name.chars().next())
        .map_or_else(|| "?".to_string(), |first| first.to_uppercase().to_string());

    rsx! {
        div { class: "profile-group",
        span { class: "profile-avatar", "{initial}" }
        select {
            id: "profile-switcher",
            onchange: move |event| {
                let chosen = event.value();
                if chosen == NEW_PROFILE_OPTION {
                    creating.set(true);
                    return;
                }
                match crate::core::db().and_then(|db| db.set_active_profile(&chosen).map_err(|err| err.to_string())) {
                    Ok(()) => {
                        tracing::info!(profile_id = chosen, "active profile switched");
                        generation.set(generation() + 1);
                    }
                    Err(err) => tracing::warn!(profile_id = chosen, error = %err, "failed to switch active profile"),
                }
            },
            for profile in profiles() {
                option {
                    key: "{profile.id}",
                    value: "{profile.id}",
                    selected: active_profile().as_deref() == Some(profile.id.as_str()),
                    "{profile.name}"
                }
            }
            option { value: NEW_PROFILE_OPTION, "+ Crear perfil" }
        }
        }
        // A floating popup, not inline in the topbar: the topbar's row is a
        // fixed 52px and clips anything taller, which an input, two
        // buttons and a possible error line would not have fit inside.
        if creating() {
            NewProfileForm {
                on_done: move |()| {
                    creating.set(false);
                    generation.set(generation() + 1);
                },
            }
        }
    }
}

/// Sentinel `<option>` value: not a real profile id (those are uuids), so
/// it can never collide with one.
const NEW_PROFILE_OPTION: &str = "__new__";

/// The popup `ProfileSwitcher` opens when "+ Crear perfil" is chosen.
/// `on_done` fires after a successful create (the new profile is already
/// active by then) and on cancel alike, so the caller doesn't need to tell
/// the two apart to know it's time to close the popup.
#[component]
fn NewProfileForm(on_done: EventHandler<()>) -> Element {
    let mut active_profile = use_context::<ActiveProfile>();
    let mut name = use_signal(String::new);
    let mut library_path = use_signal(|| {
        decanter_core::Library::default_path().map_or_else(|| String::new(), |path| path.display().to_string())
    });
    // Only `Some` once the user actually picks a folder — same rule as
    // `Onboarding`'s own copy of this form.
    let mut library_path_chosen = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let mut submit = move || {
        let name = name.read().trim().to_string();
        let chosen_path = library_path_chosen().then(|| library_path.read().clone());
        match crate::core::create_profile(&name, chosen_path.as_deref()) {
            Ok(profile) => {
                tracing::info!(profile_id = %profile.id, name, "profile created");
                active_profile.set(Some(profile.id));
                on_done.call(());
            }
            Err(err) => {
                tracing::error!(name, error = %err, "failed to create profile");
                error.set(Some(err));
            }
        }
    };

    rsx! {
        // Same backdrop-plus-floating-panel pairing every other popup in
        // this app uses (see `library::context_menu`), so this reads as
        // "the same kind of thing" instead of a one-off.
        div { class: "context-backdrop", onclick: move |_| on_done.call(()) }
        div {
            class: "new-profile-popup",
            onclick: move |event| event.stop_propagation(),
            h2 { "Nuevo perfil" }
            input {
                r#type: "text",
                placeholder: "Nombre del perfil",
                value: "{name}",
                autofocus: true,
                oninput: move |event| name.set(event.value()),
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter && !name.read().trim().is_empty() {
                        submit();
                    }
                },
            }
            p { class: "muted", "Dónde vive la biblioteca de este perfil:" }
            div { class: "field-row",
                input { r#type: "text", readonly: true, value: "{library_path}" }
                button {
                    class: "secondary",
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(folder) = rfd::AsyncFileDialog::new()
                                .set_title("Elegí dónde vive la biblioteca de este perfil")
                                .pick_folder()
                                .await
                            {
                                // Content goes under `.../steamapps`,
                                // matching Steam's own layout for a
                                // library folder — see
                                // `Library::root_under`'s own doc.
                                let root = decanter_core::Library::root_under(folder.path());
                                tracing::debug!(path = %root.display(), "profile library path picked from file dialog");
                                library_path.set(root.to_string_lossy().into_owned());
                                library_path_chosen.set(true);
                            }
                        });
                    },
                    "Elegir carpeta"
                }
            }
            if let Some(message) = &*error.read() {
                p { class: "error", "{message}" }
            }
            div { class: "new-profile-actions",
                button { class: "secondary", onclick: move |_| on_done.call(()), "Cancelar" }
                button {
                    disabled: name.read().trim().is_empty(),
                    onclick: move |_| submit(),
                    "Crear"
                }
            }
        }
    }
}

/// Starts an OS-level move of the window, the same drag a native title
/// bar would start on mousedown. A no-op anywhere without one to move --
/// `dioxus::desktop::window()` doesn't exist in that build at all.
///
/// `pub(crate)`: also the drag handle for `properties::PropertiesWindow`'s
/// own custom topbar, not just this one.
#[cfg(feature = "desktop")]
pub(crate) fn start_window_drag() {
    dioxus::desktop::window().drag();
}

#[cfg(not(feature = "desktop"))]
pub(crate) const fn start_window_drag() {}

/// Stops every running game, then closes the main window -- this app's
/// own meaning for its window controls' "✕", passed to `WindowControls`
/// as `on_close` rather than baked into it (see that component's doc).
#[cfg(feature = "desktop")]
fn close_main_window() {
    let window = dioxus::desktop::window();
    // Hidden first so closing feels immediate: stopping a game means
    // asking it to exit and waiting for it, and a window sitting there
    // unresponsive meanwhile reads as a hang.
    window.set_visible(false);
    dioxus::core::spawn_forever(async move {
        crate::launch::stop_everything().await;
        window.close();
    });
}

#[cfg(not(feature = "desktop"))]
const fn close_main_window() {}

/// The eight grab strips a native window frame would have.
///
/// Turning decorations off in `main.rs` takes the resize border with them,
/// so the window becomes fixed-size in practice — the compositor has
/// nothing left to grab. These put it back: thin, transparent strips
/// pinned to the window edges, each asking the compositor for an
/// interactive resize in its own direction.
///
/// They sit on top of everything and are deliberately only a few pixels
/// wide, so the content underneath keeps its own clicks.
#[cfg(feature = "desktop")]
#[component]
pub(crate) fn ResizeEdges() -> Element {
    use dioxus::desktop::tao::window::ResizeDirection;

    // `class` doubles as the direction: each maps to one CSS rule that
    // pins the strip to that edge and sets the matching cursor.
    const EDGES: [(&str, ResizeDirection); 8] = [
        ("n", ResizeDirection::North),
        ("s", ResizeDirection::South),
        ("e", ResizeDirection::East),
        ("w", ResizeDirection::West),
        ("ne", ResizeDirection::NorthEast),
        ("nw", ResizeDirection::NorthWest),
        ("se", ResizeDirection::SouthEast),
        ("sw", ResizeDirection::SouthWest),
    ];

    rsx! {
        for (name , direction) in EDGES {
            div {
                key: "{name}",
                class: "resize-edge resize-{name}",
                onmousedown: move |event| {
                    // Left button only: a right-click here belongs to
                    // whatever the compositor does with window edges.
                    if event.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary)
                    {
                        return;
                    }
                    if let Err(err) = dioxus::desktop::window().drag_resize_window(direction) {
                        tracing::debug!("resize not available: {err}");
                    }
                },
            }
        }
    }
}

#[cfg(not(feature = "desktop"))]
#[component]
pub(crate) fn ResizeEdges() -> Element {
    rsx! {}
}

/// Minimize/maximize/close, standing in for the native title bar's own
/// buttons now that `main.rs` turns decorations off -- otherwise the
/// window would have no way to do any of the three.
///
/// `on_close` is the caller's, not a plain `window.close()` baked in here:
/// this app's main window has to stop every running game first (see this
/// module's own usage below), but `properties::PropertiesWindow` -- the
/// other thing that uses this -- is just a second window with nothing of
/// its own to shut down, and closing it has no business touching a game
/// in progress.
#[cfg(feature = "desktop")]
#[component]
pub(crate) fn WindowControls(on_close: EventHandler<()>) -> Element {
    let window = dioxus::desktop::use_window();

    rsx! {
        div { class: "window-controls",
            button {
                class: "window-btn",
                title: "Minimizar",
                onclick: {
                    let window = std::rc::Rc::clone(&window);
                    move |_| window.set_minimized(true)
                },
                "─"
            }
            button {
                class: "window-btn",
                title: "Maximizar",
                onclick: {
                    let window = std::rc::Rc::clone(&window);
                    move |_| window.set_maximized(!window.is_maximized())
                },
                "☐"
            }
            button {
                class: "window-btn window-btn-close",
                title: "Cerrar",
                onclick: move |_| on_close.call(()),
                "✕"
            }
        }
    }
}

#[cfg(not(feature = "desktop"))]
#[component]
pub(crate) fn WindowControls(on_close: EventHandler<()>) -> Element {
    let _ = on_close;
    rsx! {}
}

/// The bottom download strip from the concept mockup, wired to real
/// installs instead of mock data. Present on every route (Steam's own
/// equivalent isn't scoped to one screen either), and only takes up space
/// while there's something to show -- `.app-shell`'s `auto` row collapses
/// to nothing when this renders empty, rather than sitting there blank.
///
/// Only queued/downloading entries: a failure already has a resolution
/// step (Descartar) in the detail pane and Descargas, and duplicating it
/// here would just be a second place the same dismissal has to happen.
#[component]
fn DownloadBar() -> Element {
    let crate::background::Background { installing, .. } = use_context::<crate::background::Background>();
    let games = use_context::<crate::gog_library::GogLibrary>().games;

    let active: Vec<(String, crate::background::InstallStatus)> = installing
        .read()
        .iter()
        .filter(|(_, status)| !matches!(status, crate::background::InstallStatus::Failed(_)))
        .map(|(id, status)| (id.clone(), status.clone()))
        .collect();

    if active.is_empty() {
        return rsx! {};
    }

    let games = games.read();
    let title_for = |id: &str| {
        games
            .iter()
            .find(|entry| entry.game.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.game.title.clone())
    };

    rsx! {
        div { class: "downbar",
            for (product_id, status) in active {
                {
                    let title = title_for(&product_id);
                    let fraction = status.fraction();
                    match status {
                        crate::background::InstallStatus::Queued => rsx! {
                            div { class: "dl-item", key: "{product_id}",
                                span { class: "lbl", "Descargando: {title}" }
                                span { "En cola…" }
                            }
                        },
                        crate::background::InstallStatus::Installing { bytes_per_second, .. } => {
                            let width = fraction.map_or_else(|| "100%".to_string(), |value| format!("{:.0}%", value * 100.0));
                            rsx! {
                                div { class: "dl-item", key: "{product_id}",
                                    span { class: "lbl", "Descargando: {title}" }
                                    div { class: "dl-bar", b { style: "width: {width}" } }
                                    span {
                                        if let Some(fraction) = fraction {
                                            "{(fraction * 100.0) as u32}% · "
                                        }
                                        "{crate::format::rate(bytes_per_second)}"
                                    }
                                }
                            }
                        }
                        // Filtered out above; `InstallStatus` just isn't
                        // exhaustive without this arm.
                        crate::background::InstallStatus::Failed(_) => rsx! {},
                    }
                }
            }
        }
    }
}
