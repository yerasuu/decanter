//! decanter's own settings — off the library nav entirely, the way Steam
//! keeps these behind the gear beside the account switcher rather than as
//! a tab of its own. Two sections: external shortcuts (per profile) and
//! the default Proton every game runs on unless it pins one of its own
//! (global, not tied to any profile).

use decanter_core::{InstalledGame, Storefront};
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
        // The onboarding gate in `main.rs` means this can't actually
        // happen any more — a profile always exists by the time any
        // route renders — kept as a fallback rather than an `unreachable!`
        // in case that gate is ever bypassed (deep-linking, a future
        // route that skips it).
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

/// The profile's storefront accounts — decanter holds every session itself
/// now, so this is where one gets added or dropped, no next-ui, no
/// plugin. A profile can hold several, even several of the same
/// storefront: each shows its own row, with its own "Cerrar sesión".
#[component]
fn AccountTab(profile_id: String) -> Element {
    let mut generation = use_signal(|| 0_u32);
    let mut accounts = use_signal(Vec::<decanter_core::Account>::new);

    use_effect({
        let profile_id = profile_id.clone();
        move || {
            let _ = generation();
            match crate::core::db() {
                Ok(db) => accounts.set(
                    db.accounts_in_profile(&profile_id)
                        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to list accounts"))
                        .unwrap_or_default(),
                ),
                Err(err) => tracing::warn!(profile_id, error = %err, "failed to open db, account list not refreshed"),
            }
        }
    });

    rsx! {
        p { class: "muted",
            "Las cuentas de este perfil. Todas aportan a la Biblioteca a la vez — no hace falta elegir una activa."
        }
        if accounts().is_empty() {
            div { class: "empty", "Todavia no hay ninguna cuenta en este perfil." }
        } else {
            div { class: "card",
                for account in accounts() {
                    AccountRow {
                        key: "{account.storefront.as_str_name()}-{account.id}",
                        account: account.clone(),
                        on_change: move |()| generation.set(generation() + 1),
                    }
                }
            }
        }
        AddGogAccount {
            profile_id,
            on_added: move |()| generation.set(generation() + 1),
        }
    }
}

/// A UI-facing name for a storefront — [`Storefront::as_str_name`] is the
/// machine-facing one (a directory/keyring segment), not fit to print.
/// Shared with the library sidebar's platform filter.
pub(crate) const fn storefront_label(storefront: Storefront) -> &'static str {
    match storefront {
        Storefront::Unspecified => "Sin tienda",
        Storefront::Steam => "Steam",
        Storefront::EpicGames => "Epic Games",
        Storefront::Gog => "GOG",
        Storefront::AmazonGames => "Amazon Games",
        Storefront::EaApp => "EA App",
        Storefront::UbisoftConnect => "Ubisoft Connect",
        Storefront::BattleNet => "Battle.net",
    }
}

/// One logged-in account. `on_change` fires after a logout actually
/// removes the row — the caller re-lists rather than this row guessing
/// how to remove itself from a list it doesn't own.
#[component]
fn AccountRow(account: decanter_core::Account, on_change: EventHandler<()>) -> Element {
    let library = use_context::<GogLibrary>();
    let mut status = use_signal(|| AccountStatus::Idle);

    // Re-checked on mount, so a session GOG has since revoked shows as
    // such instead of a dead "connected" state.
    let session = use_resource({
        let account_id = account.id.clone();
        move || {
            let account_id = account_id.clone();
            async move {
                tracing::debug!(account_id, "checking gog session status");
                decanter_gog::session(&account_id)
                    .await
                    .inspect(|session| {
                        if session.is_none() {
                            // Rendered as a red "la sesion se cerro en GOG" —
                            // visible to the user, not just a cache miss.
                            tracing::error!(account_id, "gog session no longer valid, revoked upstream");
                        }
                    })
                    .inspect_err(|err| tracing::error!(account_id, error = %err, "gog session check failed"))
            }
        }
    });

    let label = match &*session.read() {
        None => rsx! { span { class: "muted", "Comprobando sesión…" } },
        Some(Ok(Some(session_account))) => rsx! { strong { "{session_account.display_name}" } },
        Some(Ok(None)) => rsx! { span { class: "error", "la sesión se cerró en GOG" } },
        Some(Err(err)) => rsx! { span { class: "error", "sesión inválida: {err}" } },
    };

    rsx! {
        div { class: "list-row",
            span { class: "muted", style: "width: 8rem;", "{storefront_label(account.storefront)}" }
            span { style: "flex: 1;", {label} }
            button {
                class: "secondary",
                disabled: *status.read() == AccountStatus::Working,
                onclick: {
                    let account_id = account.id.clone();
                    let storefront = account.storefront;
                    move |_| {
                        let account_id = account_id.clone();
                        status.set(AccountStatus::Working);
                        tracing::info!(account_id, "logging out of gog");
                        spawn(async move {
                            // Storefront-specific logout is the only part
                            // that varies here — GOG is the only one
                            // implemented today, but a future storefront
                            // would branch on `storefront` at this one
                            // spot, not rewrite the row around it.
                            if let Err(err) = decanter_gog::log_out(&account_id).await {
                                tracing::error!(account_id, error = %err, "gog logout failed");
                                status.set(AccountStatus::Failed(err.to_string()));
                                return;
                            }
                            match crate::core::db()
                                .and_then(|db| db.forget_account(&account_id, storefront).map_err(|err| err.to_string()))
                            {
                                Ok(()) => {
                                    tracing::info!(account_id, "account removed from profile");
                                    // Otherwise a fetch task left running past logout survives
                                    // a same-session re-login under the same account id too —
                                    // see `GogLibrary::forget_account`'s own doc for why.
                                    library.forget_account(&account_id);
                                    status.set(AccountStatus::Idle);
                                    on_change.call(());
                                }
                                Err(err) => {
                                    tracing::error!(account_id, error = %err, "failed to remove account from profile");
                                    status.set(AccountStatus::Failed(err));
                                }
                            }
                        });
                    }
                },
                "Cerrar sesión"
            }
        }
        if let AccountStatus::Failed(message) = &*status.read() {
            p { class: "error", "{message}" }
        }
    }
}

/// "+ Agregar cuenta GOG" and the paste-the-redirect-URL flow it opens.
///
/// GOG's OAuth has no loopback or device flow: the only way through is a
/// browser login that lands on a page whose URL carries a one-shot code.
/// So the user opens the page, logs in, and pastes the URL they end up on
/// back here. No account id goes into `decanter_gog::log_in` — which account
/// this becomes is only known once GOG answers, so `profile_id` (already
/// known) is what ties the *result* to this profile, via `upsert_account`.
#[component]
fn AddGogAccount(profile_id: String, on_added: EventHandler<()>) -> Element {
    let library = use_context::<GogLibrary>();
    let mut adding = use_signal(|| false);
    let mut pasted = use_signal(String::new);
    let mut status = use_signal(|| AccountStatus::Idle);

    if !adding() {
        return rsx! {
            button { onclick: move |_| adding.set(true), "+ Agregar cuenta GOG" }
        };
    }

    rsx! {
        div { class: "card",
            p { class: "muted",
                "Abrí GOG, iniciá sesión, y pegá acá la dirección de la página a la que llegues."
            }
            button {
                onclick: move |_| {
                    if let Err(err) = open::that_detached(decanter_gog::auth_url()) {
                        tracing::error!(error = %err, "failed to open browser for gog login");
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
                        let profile_id = profile_id.clone();
                        move |_| {
                            let profile_id = profile_id.clone();
                            let code = pasted.read().clone();
                            status.set(AccountStatus::Working);
                            tracing::info!(profile_id, "logging in to gog");
                            spawn(async move {
                                match decanter_gog::log_in(&code).await {
                                    Ok(account) => {
                                        let now = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map_or(0, |since| since.as_secs() as i64);
                                        let saved = crate::core::db().and_then(|db| {
                                            db.upsert_account(
                                                    &account.account_id,
                                                    Storefront::Gog,
                                                    &profile_id,
                                                    &account.display_name,
                                                    now,
                                                )
                                                .map_err(|err| err.to_string())
                                        });
                                        match saved {
                                            Ok(_) => {
                                                tracing::info!(
                                                    profile_id,
                                                    account_id = account.account_id,
                                                    "gog account added",
                                                );
                                                pasted.set(String::new());
                                                status.set(AccountStatus::Idle);
                                                adding.set(false);
                                                library.refresh();
                                                on_added.call(());
                                            }
                                            Err(err) => {
                                                tracing::error!(profile_id, error = %err, "failed to save gog account");
                                                status.set(AccountStatus::Failed(err));
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        tracing::error!(profile_id, error = %err, "gog login failed");
                                        status.set(AccountStatus::Failed(err.to_string()));
                                    }
                                }
                            });
                        }
                    },
                    "Conectar"
                }
            }
            if let AccountStatus::Failed(message) = &*status.read() {
                p { class: "error", "{message}" }
            }
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
        .inspect_err(|err| tracing::warn!(error = %err, "failed to read close_to_tray setting, defaulting to false"))
        .unwrap_or(false);

    rsx! {
        h2 { "Al cerrar la ventana" }
        label { class: "field-row",
            input {
                r#type: "checkbox",
                checked: close_to_tray,
                onchange: move |event| {
                    let close_to_tray = event.checked();
                    match crate::core::db() {
                        Ok(db) => match db.set_close_to_tray(close_to_tray) {
                            Ok(()) => {
                                tracing::info!(close_to_tray, "close-to-tray setting changed");
                                generation.set(generation() + 1);
                            }
                            Err(err) => {
                                tracing::warn!(close_to_tray, error = %err, "failed to save close_to_tray setting");
                            }
                        },
                        Err(err) => {
                            tracing::warn!(close_to_tray, error = %err, "failed to open db, close_to_tray setting not saved");
                        }
                    }
                },
            }
            "Dejar Decanter corriendo en la bandeja en vez de cerrarlo"
        }
        p { class: "muted",
            "Requiere reiniciar Decanter para que el cambio tenga efecto."
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
    let runners = decanter_runtime::steam_runners();
    let current = crate::core::db()
        .inspect_err(|err| tracing::warn!(error = %err, "failed to open db, default proton unknown"))
        .ok()
        .and_then(|db| {
            db.default_proton()
                .inspect_err(|err| tracing::warn!(error = %err, "failed to read default proton setting"))
                .ok()
                .flatten()
        });

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
                    match crate::core::db() {
                        Ok(db) => match db.set_default_proton(chosen.as_deref()) {
                            Ok(()) => {
                                tracing::info!(proton = chosen, "default proton pin changed");
                                generation.set(generation() + 1);
                            }
                            Err(err) => tracing::warn!(proton = chosen, error = %err, "failed to save default proton setting"),
                        },
                        Err(err) => {
                            tracing::warn!(proton = chosen, error = %err, "failed to open db, default proton not changed");
                        }
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
/// to fetch metadata for, no depot to verify against, nothing decanter
/// installed. It is just a path someone pointed at.
///
/// Filed under the profile itself rather than one of its accounts — a
/// shortcut isn't owned by any storefront, so there's no account to pick,
/// and unlike a GOG game it never needs to show up twice for two accounts
/// that both own it.
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
                            tracing::debug!(path = %chosen.display(), "shortcut executable picked from file dialog");
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
                            tracing::info!(profile_id, shortcut_id = id, name = name_value, "shortcut added");
                            name.set(String::new());
                            path.set(String::new());
                            error.set(String::new());
                            generation.set(generation() + 1);
                        }
                        Err(err) => {
                            tracing::error!(profile_id, name = name_value, error = %err, "failed to add shortcut");
                            error.set(err);
                        }
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
                    onclick: move |_| {
                        tracing::info!(profile_id = %stop_profile, product_id = %stop_id, "stopping shortcut");
                        spawn(crate::launch::stop(stop_profile.clone(), stop_id.clone()));
                    },
                    "DETENER"
                }
            } else {
                button {
                    onclick: move |_| {
                        let account_id = start_profile.clone();
                        let game = start_game.clone();
                        tracing::info!(profile_id = %account_id, product_id = %game.game_id, "launching shortcut");
                        dioxus::core::spawn_forever(async move {
                            crate::launch::start(account_id, game, running).await;
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
                    match crate::core::remove_shortcut(&remove_profile, &remove_id) {
                        Ok(()) => tracing::info!(profile_id = %remove_profile, shortcut_id = %remove_id, "shortcut removed"),
                        Err(err) => tracing::error!("no se pudo eliminar el atajo: {err}"),
                    }
                    generation.set(generation() + 1);
                },
                "Eliminar"
            }
        }
    }
}
