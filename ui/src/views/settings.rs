//! achv's own settings — off the library nav entirely, the way Steam
//! keeps these behind the gear beside the account switcher rather than as
//! a tab of its own. Two sections: external shortcuts (per profile) and
//! the default Proton every game runs on unless it pins one of its own
//! (global, not tied to any profile).

use achv_core::InstalledGame;
use dioxus::prelude::*;

use crate::{ActiveProfile, CoreResource, gog_library::GogLibrary};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    General,
    Account,
    Compatibility,
    Shortcuts,
}

#[component]
pub fn Settings() -> Element {
    let core = use_context::<CoreResource>();
    let active_profile = use_context::<ActiveProfile>();
    let mut tab = use_signal(|| Tab::Account);

    let profile_id = active_profile.read().clone();

    let body = match (&*core.read(), profile_id) {
        (Some(Err(err)), _) => {
            rsx! { div { class: "error", "Error abriendo el almacenamiento: {err}" } }
        }
        (None, _) => rsx! { p { class: "muted", "Cargando..." } },
        (Some(Ok(_)), None) => rsx! { div { class: "empty", "Elegi un perfil arriba." } },
        (Some(Ok(_)), Some(profile_id)) => match tab() {
            Tab::General => rsx! { GeneralTab {} },
            Tab::Account => rsx! { AccountTab { profile_id } },
            Tab::Compatibility => rsx! { CompatibilityTab {} },
            Tab::Shortcuts => rsx! { ShortcutsTab { profile_id } },
        },
    };

    rsx! {
        div { class: "page",
            h1 { "Ajustes" }
            div { class: "tabs",
                button {
                    class: if tab() == Tab::General { "active" } else { "" },
                    onclick: move |_| tab.set(Tab::General),
                    "General"
                }
                button {
                    class: if tab() == Tab::Account { "active" } else { "" },
                    onclick: move |_| tab.set(Tab::Account),
                    "Cuenta"
                }
                button {
                    class: if tab() == Tab::Compatibility { "active" } else { "" },
                    onclick: move |_| tab.set(Tab::Compatibility),
                    "Compatibilidad"
                }
                button {
                    class: if tab() == Tab::Shortcuts { "active" } else { "" },
                    onclick: move |_| tab.set(Tab::Shortcuts),
                    "Externos"
                }
            }
            {body}
        }
    }
}

/// GOG sign-in. Achv holds the session itself now, so this is where it
/// gets created — no next-ui, no plugin.
///
/// GOG's OAuth has no loopback or device flow: the only way through is a
/// browser login that lands on a page whose URL carries a one-shot code.
/// So the user opens the page, logs in, and pastes the URL they end up on
/// back here.
#[component]
fn AccountTab(profile_id: String) -> Element {
    let library = use_context::<GogLibrary>();

    let mut generation = use_signal(|| 0_u32);
    let mut pasted = use_signal(String::new);
    let mut status = use_signal(|| AccountStatus::Idle);

    // Re-checked whenever a login/logout lands, so the panel reflects what
    // actually happened rather than what we hoped would.
    let session = use_resource({
        let profile_id = profile_id.clone();
        move || {
            let profile_id = profile_id.clone();
            let _ = generation();
            async move { achv_gog::session(&profile_id).await }
        }
    });

    rsx! {
        match &*session.read() {
            None => rsx! { p { class: "muted", "Comprobando sesión…" } },
            Some(Ok(Some(account))) => rsx! {
                p {
                    "Conectado a GOG como "
                    strong { "{account.display_name}" }
                }
                button {
                    class: "secondary",
                    disabled: *status.read() == AccountStatus::Working,
                    onclick: {
                        let profile_id = profile_id;
                        move |_| {
                            let profile_id = profile_id.clone();
                            status.set(AccountStatus::Working);
                            spawn(async move {
                                match achv_gog::log_out(&profile_id).await {
                                    Ok(()) => {
                                        status.set(AccountStatus::Idle);
                                        generation.set(generation() + 1);
                                        library.refresh();
                                    }
                                    Err(err) => status.set(AccountStatus::Failed(err.to_string())),
                                }
                            });
                        }
                    },
                    "Cerrar sesión"
                }
            },
            Some(Ok(None)) => rsx! {
                p { class: "muted",
                    "Abrí GOG, iniciá sesión, y pegá acá la dirección de la página a la que llegues."
                }
                button {
                    onclick: move |_| {
                        if let Err(err) = open::that_detached(achv_gog::auth_url()) {
                            status.set(AccountStatus::Failed(format!("no se pudo abrir el navegador: {err}")));
                        }
                    },
                    "Abrir GOG"
                }
                div { class: "field-row",
                    input {
                        r#type: "text",
                        placeholder: "https://embed.gog.com/on_login_success?…&code=…",
                        value: "{pasted}",
                        oninput: move |event| pasted.set(event.value()),
                    }
                    button {
                        disabled: *status.read() == AccountStatus::Working || pasted.read().trim().is_empty(),
                        onclick: {
                            let profile_id = profile_id;
                            move |_| {
                                let profile_id = profile_id.clone();
                                let code = pasted.read().clone();
                                status.set(AccountStatus::Working);
                                spawn(async move {
                                    match achv_gog::log_in(&profile_id, &code).await {
                                        Ok(_) => {
                                            pasted.set(String::new());
                                            status.set(AccountStatus::Idle);
                                            generation.set(generation() + 1);
                                            library.refresh();
                                        }
                                        Err(err) => status.set(AccountStatus::Failed(err.to_string())),
                                    }
                                });
                            }
                        },
                        "Conectar"
                    }
                }
            },
            // A stored session GOG no longer accepts: the fix is the
            // same as a fresh login, so say so rather than showing a
            // dead "connected" state.
            Some(Err(err)) => rsx! {
                p { class: "error", "La sesión guardada ya no sirve: {err}" }
                button {
                    onclick: move |_| {
                        if let Err(err) = open::that_detached(achv_gog::auth_url()) {
                            status.set(AccountStatus::Failed(format!("no se pudo abrir el navegador: {err}")));
                        }
                    },
                    "Volver a iniciar sesión"
                }
            },
        }

        if let AccountStatus::Failed(message) = &*status.read() {
            p { class: "error", "{message}" }
        }
    }
}

#[derive(Clone, PartialEq)]
enum AccountStatus {
    Idle,
    Working,
    Failed(String),
}

/// Window behaviour that isn't tied to any profile or game.
#[component]
fn GeneralTab() -> Element {
    let mut generation = use_signal(|| 0_u32);
    let _ = generation();
    let close_to_tray = crate::core::db()
        .and_then(|db| db.close_to_tray().map_err(|err| err.to_string()))
        .unwrap_or(false);

    rsx! {
        h2 { "Al cerrar la ventana" }
        label { class: "field-row",
            input {
                r#type: "checkbox",
                checked: close_to_tray,
                onchange: move |event| {
                    if let Ok(db) = crate::core::db() {
                        let _ = db.set_close_to_tray(event.checked());
                        generation.set(generation() + 1);
                    }
                },
            }
            "Dejar achv corriendo en la bandeja en vez de cerrarlo"
        }
        p { class: "muted",
            "Requiere reiniciar achv para que el cambio tenga efecto."
        }
    }
}

/// The Proton every game runs on unless it pins one of its own. Per-game
/// pinning and prefix resets live in each game's own Propiedades instead
/// (Steam's own shape: that's a per-game concern, not this screen's).
#[component]
fn CompatibilityTab() -> Element {
    let mut generation = use_signal(|| 0_u32);
    let _ = generation();
    let runners = achv_runtime::steam_runners();
    let current = crate::core::db().ok().and_then(|db| db.default_proton().ok().flatten());

    rsx! {
        p { class: "muted",
            "Cada juego corre en su propio prefijo, creado la primera vez que lo abris. Para pinear un Proton distinto o resetear el prefijo de un juego puntual, abri sus Propiedades desde la biblioteca."
        }
        h2 { "Proton por defecto" }
        div { class: "field-row",
            select {
                onchange: move |event| {
                    let chosen = event.value();
                    let chosen = (!chosen.is_empty()).then_some(chosen);
                    if let Ok(db) = crate::core::db() {
                        let _ = db.set_default_proton(chosen.as_deref());
                        generation.set(generation() + 1);
                    }
                },
                option { value: "", selected: current.is_none(), "Automatico (lo resuelve umu)" }
                for available in runners.clone() {
                    option {
                        key: "{available}",
                        value: "{available}",
                        selected: current.as_deref() == Some(available.as_str()),
                        "{available}"
                    }
                }
            }
        }
        if runners.is_empty() {
            p { class: "muted",
                "No hay Proton instalado en Steam (compatibilitytools.d). umu va a descargar uno la primera vez."
            }
        }
    }
}

/// Non-Steam-style shortcuts: an arbitrary executable, added by hand, that
/// runs through the same runner as a GOG install — its own prefix, its
/// own Proton pin — but with no catalog entry behind it at all.
///
/// Deliberately not folded into the GOG library: there is no product id
/// to fetch metadata for, no depot to verify against, nothing achv
/// installed. It is just a path someone pointed at.
#[component]
fn ShortcutsTab(profile_id: String) -> Element {
    let generation = use_signal(|| 0_u32);
    let running = use_context::<crate::background::Background>().running;
    let _ = generation();

    let shortcuts = crate::core::shortcuts(&profile_id);

    rsx! {
        p { class: "muted",
            "Ejecutables que no vinieron de GOG — cada uno corre en su propio prefijo, igual que un juego instalado."
        }

        AddShortcut { profile_id: profile_id.clone(), generation }

        if shortcuts.is_empty() {
            div { class: "empty", "No hay juegos externos todavia." }
        } else {
            div { class: "card",
                for game in shortcuts {
                    ShortcutRow {
                        key: "{game.game_id}",
                        profile_id: profile_id.clone(),
                        game: game,
                        running,
                        generation,
                    }
                }
            }
        }
    }
}

#[component]
fn AddShortcut(profile_id: String, mut generation: Signal<u32>) -> Element {
    let mut name = use_signal(String::new);
    let mut path = use_signal(String::new);
    let mut error = use_signal(String::new);

    rsx! {
        h2 { "Agregar" }
        div { class: "field-row",
            input {
                placeholder: "Nombre",
                value: "{name}",
                oninput: move |event| name.set(event.value()),
            }
            input {
                placeholder: "Ejecutable",
                value: "{path}",
                oninput: move |event| path.set(event.value()),
            }
            button {
                class: "secondary",
                onclick: move |_| {
                    // Native file picker (XDG portal), not a bare text
                    // field the user has to type a path into — that's
                    // how Steam does it, and typos in an absolute path
                    // are otherwise a silent "no lo pudo abrir" later.
                    spawn(async move {
                        if let Some(file) = rfd::AsyncFileDialog::new()
                            .set_title("Elegi el ejecutable")
                            .pick_file()
                            .await
                        {
                            let chosen = file.path().to_path_buf();
                            if name.read().trim().is_empty()
                                && let Some(stem) = chosen.file_stem().and_then(|s| s.to_str())
                            {
                                name.set(stem.to_string());
                            }
                            path.set(chosen.to_string_lossy().into_owned());
                        }
                    });
                },
                "Elegir archivo..."
            }
            button {
                onclick: move |_| {
                    let name_value = name().trim().to_string();
                    let path_value = path().trim().to_string();
                    if name_value.is_empty() || path_value.is_empty() {
                        error.set("Poné un nombre y un ejecutable.".to_string());
                        return;
                    }
                    let executable = std::path::PathBuf::from(&path_value);
                    if !executable.is_file() {
                        error.set("Ese archivo no existe.".to_string());
                        return;
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    match crate::core::add_shortcut(&profile_id, &id, &name_value, &executable) {
                        Ok(()) => {
                            name.set(String::new());
                            path.set(String::new());
                            error.set(String::new());
                            generation.set(generation() + 1);
                        }
                        Err(err) => error.set(err),
                    }
                },
                "Agregar"
            }
        }
        if !error().is_empty() {
            div { class: "error", "{error}" }
        }
    }
}

#[component]
fn ShortcutRow(
    profile_id: String,
    game: InstalledGame,
    running: crate::launch::Running,
    mut generation: Signal<u32>,
) -> Element {
    let is_running = running.read().contains(&game.game_id);
    let start_profile = profile_id.clone();
    let start_game = game.clone();
    let stop_profile = profile_id.clone();
    let stop_id = game.game_id.clone();
    let remove_profile = profile_id;
    let remove_id = game.game_id.clone();

    rsx! {
        div { class: "list-row",
            span { style: "flex: 1; font-weight: 600;", "{game.name}" }
            span { class: "muted", "{game.launch_executable.clone().unwrap_or_default()}" }

            if is_running {
                button {
                    class: "secondary",
                    onclick: move |_| { spawn(crate::launch::stop(stop_profile.clone(), stop_id.clone())); },
                    "DETENER"
                }
            } else {
                button {
                    onclick: move |_| {
                        let profile_id = start_profile.clone();
                        let game = start_game.clone();
                        dioxus::core::spawn_forever(async move {
                            crate::launch::start(profile_id, game, running).await;
                        });
                    },
                    "JUGAR"
                }
            }

            button {
                class: "icon-btn",
                disabled: is_running,
                title: if is_running { "Deten\u{e9} el juego antes de eliminarlo" } else { "Eliminar" },
                onclick: move |_| {
                    if let Err(err) = crate::core::remove_shortcut(&remove_profile, &remove_id) {
                        tracing::error!("no se pudo eliminar el atajo: {err}");
                    }
                    generation.set(generation() + 1);
                },
                "Eliminar"
            }
        }
    }
}
