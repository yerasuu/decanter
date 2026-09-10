//! The properties panel: everything Steam tucks behind right-click →
//! Propiedades for an installed game, in the same tabbed shape — General,
//! Actualizaciones, Archivos locales, Betas, plus two achv adds on top:
//! Versiones (rollback to a specific historical build, not just Steam's
//! per-branch newest) and Compatibilidad (this game's own Proton pin).
//!
//! A real second OS window (`open_properties_window`, via Dioxus desktop's
//! `new_window`) rather than an in-app overlay — the same "its own
//! floating dialog, draggable and resizable independent of the library"
//! shape Steam's own Properties has. That means a second, independent
//! `VirtualDom`, which normally costs having to re-plumb every context
//! provider the main window's root sets up (`CoreResource`, `GogLibrary`,
//! `Background`) into it — but nothing below ever reads one of those, only
//! `crate::core`/`achv_gog` free functions and the `game`/`account_id`
//! passed in directly, so there is nothing to re-plumb.

use achv_core::{CachedGame, Game};
use achv_gog::cloud_saves::{ForceDirection, SyncAction, SyncOutcome};
use achv_gog::{Branch, BrokenFile, Build, VerifyReport};
use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    General,
    Updates,
    LocalFiles,
    CloudSaves,
    Dlc,
    Betas,
    Versions,
    Compatibility,
}

/// Opens Propiedades for `game` in its own window. Fire-and-forget: unlike
/// an overlay, there's no state here for the caller to hold onto or close
/// later — the window manages its own lifetime, closed the same way any
/// other window is.
///
/// Nothing stops a second click from opening a second window on the same
/// game; Steam's own dialog doesn't either.
#[cfg(feature = "desktop")]
pub(super) fn open_properties_window(game: Game, account_id: String) {
    let title = format!("Propiedades — {}", game.title);
    let dom = VirtualDom::new_with_props(PropertiesWindow, PropertiesWindowProps { game, account_id });
    dioxus::desktop::window().new_window(
        dom,
        dioxus::desktop::Config::new().with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(title)
                // No native title bar -- the same custom topbar (drag
                // region, resize edges, window controls) the main window
                // draws for itself, so this reads as part of the same app
                // rather than a plain webview popup.
                .with_decorations(false)
                .with_inner_size(dioxus::desktop::LogicalSize::new(640.0, 560.0))
                .with_min_inner_size(dioxus::desktop::LogicalSize::new(480.0, 360.0))
                .with_resizable(true),
        ),
    );
}

#[cfg(not(feature = "desktop"))]
pub(super) fn open_properties_window(_game: Game, _account_id: String) {}

/// This window's own close -- unlike the main window's (see
/// `navbar::close_main_window`), nothing to stop first.
#[cfg(feature = "desktop")]
fn close_this_window() {
    dioxus::desktop::window().close();
}

#[cfg(not(feature = "desktop"))]
const fn close_this_window() {}

#[component]
fn PropertiesWindow(game: Game, account_id: String) -> Element {
    let mut tab = use_signal(|| Tab::General);

    rsx! {
        // A second window is a second document — none of `App`'s own
        // `document::Link`s carry over into it.
        document::Link { rel: "stylesheet", href: crate::MAIN_CSS }
        document::Link { rel: "stylesheet", href: crate::TAILWIND_CSS }
        document::Link { rel: "stylesheet", href: crate::GOOGLE_FONTS_HREF }
        document::Link { rel: "stylesheet", href: crate::views::navbar::NAVBAR_CSS }

        div { class: "app-shell",
            crate::views::navbar::ResizeEdges {}
            div { class: "topbar",
                span {
                    class: "logo",
                    onmousedown: move |_| crate::views::navbar::start_window_drag(),
                    span { class: "dot" }
                    "PROPIEDADES"
                }
                div {
                    class: "topbar-drag",
                    onmousedown: move |_| crate::views::navbar::start_window_drag(),
                }
                div { class: "top-right",
                    crate::views::navbar::WindowControls { on_close: move |()| close_this_window() }
                }
            }

            div { class: "app-body properties-window",
                div { class: "properties-nav",
                    button {
                        class: if tab() == Tab::General { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::General),
                        "General"
                    }
                    button {
                        class: if tab() == Tab::Updates { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::Updates),
                        "Actualizaciones"
                    }
                    button {
                        class: if tab() == Tab::LocalFiles { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::LocalFiles),
                        "Archivos locales"
                    }
                    button {
                        class: if tab() == Tab::CloudSaves { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::CloudSaves),
                        "Guardado en la nube"
                    }
                    button {
                        class: if tab() == Tab::Dlc { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::Dlc),
                        "DLC"
                    }
                    button {
                        class: if tab() == Tab::Betas { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::Betas),
                        "Betas"
                    }
                    button {
                        class: if tab() == Tab::Versions { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::Versions),
                        "Versiones"
                    }
                    button {
                        class: if tab() == Tab::Compatibility { "properties-nav-item active" } else { "properties-nav-item" },
                        onclick: move |_| tab.set(Tab::Compatibility),
                        "Compatibilidad"
                    }
                }

                div { class: "properties-body",
                    match tab() {
                        Tab::General => rsx! {
                            GeneralTab { game, account_id }
                        },
                        Tab::Updates => rsx! {
                            UpdatesTab { game, account_id }
                        },
                        Tab::LocalFiles => rsx! {
                            LocalFilesTab { game, account_id }
                        },
                        Tab::CloudSaves => rsx! {
                            CloudSavesTab { game, account_id }
                        },
                        Tab::Dlc => rsx! {
                            DlcTab { game, account_id }
                        },
                        Tab::Betas => rsx! {
                            BetasTab { game, account_id }
                        },
                        Tab::Versions => rsx! {
                            VersionsTab { game, account_id }
                        },
                        Tab::Compatibility => rsx! {
                            CompatibilityTab { game, account_id }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn GeneralTab(game: Game, account_id: String) -> Element {
    let installed = crate::core::installed_game(&account_id, &game.id);
    let playtime = crate::views::library::data::load_playtime(&account_id, &game.id);
    let path = installed.as_ref().and_then(|installed| {
        let Some(library) = crate::core::library()
            .inspect_err(
                |err| tracing::warn!(account_id, product_id = %game.id, error = %err, "failed to resolve library root"),
            )
            .ok()
        else {
            return None;
        };
        library.install_dir(&installed.install_dir)
    });

    rsx! {
        div { class: "properties-rows",
            div { class: "properties-row",
                span { "Título" }
                span { "{game.title}" }
            }
            div { class: "properties-row",
                span { "Tienda" }
                span { "GOG" }
            }
            div { class: "properties-row",
                span { "Product ID" }
                span { "{game.id}" }
            }
            if let Some(installed) = &installed {
                div { class: "properties-row",
                    span { "Build" }
                    span { "{installed.build_id}" }
                }
                if let Some(size) = installed.size_on_disk {
                    div { class: "properties-row",
                        span { "Tamaño" }
                        span { "{crate::format::bytes(size)}" }
                    }
                }
            }
            div { class: "properties-row",
                span { "Jugado" }
                span { "{crate::views::library::fmt::duration(playtime.seconds_total)}" }
            }
            if let Some(last) = installed.as_ref().and_then(|installed| installed.last_played) {
                div { class: "properties-row",
                    span { "Última sesión" }
                    span { "{crate::views::library::fmt::unix_date(last).unwrap_or_default()}" }
                }
            }
        }
        if let Some(path) = path {
            button {
                class: "secondary",
                onclick: move |_| {
                    if let Err(err) = open::that_detached(&path) {
                        tracing::error!("no se pudo abrir la carpeta: {err}");
                    }
                },
                "Abrir carpeta"
            }
        }
    }
}

#[component]
fn UpdatesTab(game: Game, account_id: String) -> Element {
    let mut checking = use_signal(|| false);
    let mut result = use_signal(|| None::<Option<String>>);
    let mut updating = use_signal(|| false);
    let mut error = use_signal(String::new);

    let installed = crate::core::installed_game(&account_id, &game.id);
    let Some(installed) = installed else {
        return rsx! {
            p { class: "muted", "El juego no está instalado." }
        };
    };

    rsx! {
        div { class: "properties-rows",
            div { class: "properties-row",
                span { "Build instalado" }
                span { "{installed.build_id}" }
            }
            div { class: "properties-row",
                span { "Rama" }
                span { "{installed.branch.clone().unwrap_or_else(|| \"mainline\".to_string())}" }
            }
        }

        div { class: "field-row",
            button {
                class: "secondary",
                disabled: checking(),
                onclick: move |_| {
                    let account_id = account_id.clone();
                    let installed = installed.clone();
                    checking.set(true);
                    error.set(String::new());
                    tracing::debug!(account_id, product_id = %installed.game_id, "checking gog for a newer build");
                    spawn(async move {
                        match achv_gog::check_update(&account_id, &installed).await {
                            Ok(update) => result.set(Some(update)),
                            Err(err) => {
                                tracing::error!(account_id, product_id = %installed.game_id, error = %err, "update check failed");
                                error.set(err.to_string());
                            }
                        }
                        checking.set(false);
                    });
                },
                if checking() {
                    "Buscando..."
                } else {
                    "Buscar actualización"
                }
            }

            match result() {
                Some(Some(build_id)) => {
                    let account_id = account_id.clone();
                    let product_id = game.id.clone();
                    let title = game.title;
                    rsx! {
                        span { class: "muted", "Hay una nueva build: {build_id}" }
                        button {
                            disabled: updating(),
                            onclick: move |_| {
                                let account_id = account_id.clone();
                                let product_id = product_id.clone();
                                let title = title.clone();
                                updating.set(true);
                                error.set(String::new());
                                tracing::info!(account_id, product_id, "update install started");
                                spawn(async move {
                                    let outcome = achv_gog::install(
                                            &account_id,
                                            &product_id,
                                            None,
                                            &title,
                                            None,
                                            None,
                                            |_| {},
                                        )
                                        .await;
                                    match outcome {
                                        Ok(()) => {
                                            tracing::info!(account_id, product_id, "update install finished");
                                            result.set(Some(None));
                                        }
                                        Err(err) => {
                                            tracing::error!(account_id, product_id, error = %err, "update install failed");
                                            error.set(err.to_string());
                                        }
                                    }
                                    updating.set(false);
                                });
                            },
                            if updating() {
                                "Actualizando..."
                            } else {
                                "Actualizar ahora"
                            }
                        }
                    }
                }
                Some(None) => rsx! {
                    span { class: "muted", "Al día." }
                },
                None => rsx! {},
            }
        }
        if !error().is_empty() {
            div { class: "error", "{error}" }
        }
    }
}

#[component]
fn LocalFilesTab(game: Game, account_id: String) -> Element {
    let mut verifying = use_signal(|| false);
    let mut report = use_signal(|| None::<VerifyReport>);
    let mut repairing = use_signal(|| false);
    let mut error = use_signal(String::new);

    let installed = crate::core::installed_game(&account_id, &game.id);
    if installed.is_none() {
        return rsx! {
            p { class: "muted", "El juego no está instalado." }
        };
    }

    rsx! {
        p { class: "muted",
            "Compara cada archivo instalado contra el manifiesto de GOG y vuelve a bajar lo que no coincida."
        }
        div { class: "field-row",
            button {
                class: "secondary",
                disabled: verifying(),
                onclick: move |_| {
                    let account_id = account_id.clone();
                    let product_id = game.id.clone();
                    verifying.set(true);
                    error.set(String::new());
                    report.set(None);
                    tracing::info!(account_id, product_id, "file verification started");
                    spawn(async move {
                        match achv_gog::verify(&account_id, &product_id).await {
                            Ok(outcome) => {
                                tracing::info!(
                                    account_id,
                                    product_id,
                                    files_checked = outcome.files_checked,
                                    broken = outcome.broken.len(),
                                    "file verification finished",
                                );
                                report.set(Some(outcome));
                            }
                            Err(err) => {
                                tracing::error!(account_id, product_id, error = %err, "file verification failed");
                                error.set(err.to_string());
                            }
                        }
                        verifying.set(false);
                    });
                },
                if verifying() {
                    "Verificando..."
                } else {
                    "Verificar archivos"
                }
            }

            if let Some(current) = report() {
                if current.is_clean() {
                    span { class: "muted", "{current.files_checked} archivos, todo en orden." }
                } else {
                    span { class: "error",
                        "{current.broken.len()} de {current.files_checked} archivos no coinciden."
                    }
                    {
                        let account_id = account_id.clone();
                        let product_id = game.id.clone();
                        let broken: Vec<BrokenFile> = current.broken;
                        rsx! {
                            button {
                                disabled: repairing(),
                                onclick: move |_| {
                                    let account_id = account_id.clone();
                                    let product_id = product_id.clone();
                                    let broken = broken.clone();
                                    repairing.set(true);
                                    error.set(String::new());
                                    tracing::info!(account_id, product_id, broken_count = broken.len(), "file repair started");
                                    spawn(async move {
                                        if let Err(err) = achv_gog::repair(&account_id, &product_id, &broken)
                                            .await
                                        {
                                            tracing::error!(account_id, product_id, error = %err, "file repair failed");
                                            error.set(err.to_string());
                                            repairing.set(false);
                                            return;
                                        }
                                        tracing::info!(account_id, product_id, "file repair finished");
                                        match achv_gog::verify(&account_id, &product_id).await {
                                            Ok(outcome) => report.set(Some(outcome)),
                                            Err(err) => {
                                                tracing::error!(account_id, product_id, error = %err, "re-verify after repair failed");
                                                error.set(err.to_string());
                                            }
                                        }
                                        repairing.set(false);
                                    });
                                },
                                if repairing() {
                                    "Reparando..."
                                } else {
                                    "Reparar"
                                }
                            }
                        }
                    }
                }
            }
        }
        if !error().is_empty() {
            div { class: "error", "{error}" }
        }
    }
}

/// Syncing a local save folder against GOG's Cloud Saves — see
/// `achv_gog::cloud_saves` for the sync protocol and
/// `achv_gog::save_location` for how the folder itself gets found:
/// GOG's own remote-config for this game's build, when it reports one,
/// with manual entry (and the generic prefix shortcuts below) as the
/// fallback for whatever it doesn't cover.
#[component]
fn CloudSavesTab(game: Game, account_id: String) -> Element {
    let mut generation = use_signal(|| 0_u32);
    let mut path_input = use_signal(String::new);
    let mut syncing = use_signal(|| false);
    let mut last_outcome = use_signal(|| None::<SyncAction>);
    let mut error = use_signal(String::new);
    let detecting = use_signal(|| false);
    let detected: Signal<Option<Vec<achv_gog::save_location::SaveLocation>>> = use_signal(|| None);
    let mut auto_detect_attempted = use_signal(|| false);

    let Some(installed) = crate::core::installed_game(&account_id, &game.id) else {
        return rsx! { p { class: "muted", "El juego no está instalado." } };
    };
    let _ = generation();
    if path_input.read().is_empty()
        && let Some(saved) = &installed.cloud_save_path
    {
        path_input.set(saved.clone());
    }

    // Runs once, on first open of a game with no saved path yet — nothing
    // to override, so a single unambiguous location is saved right away
    // instead of waiting on a click. `generation` is read so a manual
    // "Guardar carpeta" elsewhere doesn't leave this stale, but
    // `auto_detect_attempted` keeps it from ever firing a second time.
    use_effect({
        let account_id = account_id.clone();
        let game_id = game.id.clone();
        move || {
            let _ = generation();
            if *auto_detect_attempted.read() {
                return;
            }
            let Some(current) = crate::core::installed_game(&account_id, &game_id) else {
                return;
            };
            if current.cloud_save_path.is_some() {
                return;
            }
            auto_detect_attempted.set(true);
            // Silent: a game with no auto-detectable location is the
            // ordinary case, not something worth an error banner the
            // moment the tab opens.
            run_cloud_save_detection(
                account_id.clone(),
                game_id.clone(),
                false,
                detecting,
                detected,
                error,
                path_input,
                generation,
            );
        }
    });

    let prefix = crate::core::prefix_for(&account_id, &game.id);

    rsx! {
        p { class: "muted",
            "achv intenta detectar la carpeta de guardado con la configuración remota de GOG. Si no encuentra nada, indicala a mano."
        }

        div { class: "field-row",
            button {
                class: "secondary",
                disabled: detecting(),
                onclick: {
                    let account_id = account_id.clone();
                    let game_id = game.id.clone();
                    move |_| {
                        // A manual click always wants the folder saved
                        // too — the same one-click "just find it" the
                        // automatic first-run attempt does.
                        run_cloud_save_detection(account_id.clone(), game_id.clone(), true, detecting, detected, error, path_input, generation);
                    }
                },
                if detecting() {
                    "Detectando..."
                } else {
                    "Detectar automáticamente"
                }
            }
        }

        if let Some(locations) = detected() {
            if locations.len() > 1 {
                div { class: "field-row",
                    for location in locations {
                        button {
                            class: "secondary",
                            key: "{location.name}",
                            onclick: {
                                let path = location.path.to_string_lossy().into_owned();
                                move |_| path_input.set(path.clone())
                            },
                            "{location.name}"
                        }
                    }
                }
            }
        }
        div { class: "properties-rows",
            div { class: "properties-row",
                span { "Última sincronización" }
                span {
                    if let Some(last) = installed.cloud_save_last_sync {
                        "{crate::views::library::fmt::unix_date(last).unwrap_or_default()}"
                    } else {
                        "nunca"
                    }
                }
            }
        }

        div { class: "field-row",
            input {
                placeholder: "Carpeta de guardado",
                value: "{path_input}",
                oninput: move |event| path_input.set(event.value()),
            }
            button {
                class: "secondary",
                onclick: {
                    let starting = prefix.as_ref().map(achv_runtime::Prefix::drive_c);
                    move |_| {
                        let starting = starting.clone();
                        spawn(async move {
                            let mut dialog = rfd::AsyncFileDialog::new().set_title("Elegí la carpeta de guardado");
                            if let Some(starting) = starting {
                                dialog = dialog.set_directory(starting);
                            }
                            if let Some(folder) = dialog.pick_folder().await {
                                let path = folder.path().to_string_lossy().into_owned();
                                tracing::debug!(path, "cloud save folder picked from file dialog");
                                path_input.set(path);
                            }
                        });
                    }
                },
                "Elegir carpeta..."
            }
            button {
                disabled: path_input.read().trim().is_empty(),
                onclick: {
                    let account_id = account_id.clone();
                    let product_id = game.id.clone();
                    move |_| {
                        let value = path_input.read().trim().to_string();
                        match crate::core::set_cloud_save_path(&account_id, &product_id, Some(&value)) {
                            Ok(()) => {
                                tracing::info!(account_id, product_id, path = value, "cloud save path set");
                                error.set(String::new());
                                generation.set(generation() + 1);
                            }
                            Err(err) => {
                                tracing::error!(account_id, product_id, path = value, error = %err, "failed to save cloud save path");
                                error.set(err);
                            }
                        }
                    }
                },
                "Guardar carpeta"
            }
        }

        div { class: "field-row",
            button {
                disabled: syncing() || installed.cloud_save_path.is_none(),
                onclick: {
                    let account_id = account_id;
                    let product_id = game.id;
                    let local_root = installed.cloud_save_path.clone();
                    let last_sync = installed.cloud_save_last_sync;
                    move |_| {
                        let Some(local_root) = local_root.clone() else { return };
                        let account_id = account_id.clone();
                        let product_id = product_id.clone();
                        syncing.set(true);
                        error.set(String::new());
                        spawn(async move {
                            match run_cloud_sync(account_id, product_id, local_root, last_sync, None).await {
                                Ok(outcome) => {
                                    last_outcome.set(Some(outcome.action));
                                    generation.set(generation() + 1);
                                }
                                Err(err) => error.set(err),
                            }
                            syncing.set(false);
                        });
                    }
                },
                if syncing() {
                    "Sincronizando..."
                } else {
                    "Sincronizar ahora"
                }
            }

            match last_outcome() {
                Some(SyncAction::Conflict) => rsx! {
                    span { class: "error", "Cambios en ambos lados desde la última sync." }
                },
                Some(other) => rsx! {
                    span { class: "muted", "{sync_action_label(other)}" }
                },
                None => rsx! {},
            }
        }

        if last_outcome() == Some(SyncAction::Conflict) {
            div { class: "field-row",
                button {
                    class: "secondary",
                    disabled: syncing(),
                    onclick: {
                        let account_id = account_id.clone();
                        let product_id = game.id.clone();
                        let local_root = installed.cloud_save_path.clone();
                        let last_sync = installed.cloud_save_last_sync;
                        move |_| {
                            let Some(local_root) = local_root.clone() else { return };
                            let account_id = account_id.clone();
                            let product_id = product_id.clone();
                            syncing.set(true);
                            error.set(String::new());
                            spawn(async move {
                                let outcome = run_cloud_sync(
                                        account_id,
                                        product_id,
                                        local_root,
                                        last_sync,
                                        Some(ForceDirection::Upload),
                                    )
                                    .await;
                                match outcome {
                                    Ok(outcome) => {
                                        last_outcome.set(Some(outcome.action));
                                        generation.set(generation() + 1);
                                    }
                                    Err(err) => error.set(err),
                                }
                                syncing.set(false);
                            });
                        }
                    },
                    "Subir de todos modos"
                }
                button {
                    class: "secondary",
                    disabled: syncing(),
                    onclick: {
                        let account_id = account_id.clone();
                        let product_id = game.id.clone();
                        let local_root = installed.cloud_save_path.clone();
                        let last_sync = installed.cloud_save_last_sync;
                        move |_| {
                            let Some(local_root) = local_root.clone() else { return };
                            let account_id = account_id.clone();
                            let product_id = product_id.clone();
                            syncing.set(true);
                            error.set(String::new());
                            spawn(async move {
                                let outcome = run_cloud_sync(
                                        account_id,
                                        product_id,
                                        local_root,
                                        last_sync,
                                        Some(ForceDirection::Download),
                                    )
                                    .await;
                                match outcome {
                                    Ok(outcome) => {
                                        last_outcome.set(Some(outcome.action));
                                        generation.set(generation() + 1);
                                    }
                                    Err(err) => error.set(err),
                                }
                                syncing.set(false);
                            });
                        }
                    },
                    "Descargar de todos modos"
                }
            }
        }

        if !error().is_empty() {
            div { class: "error", "{error}" }
        }
    }
}

/// Asks GOG's remote-config for this game's save locations and, when it
/// names exactly one, saves it right away — no point making the user
/// retype what was just found. `report_errors` silences the "nothing
/// found" / request-failed messages for the automatic first-run attempt,
/// where a miss is the ordinary case rather than something to flag; the
/// explicit "Detectar automáticamente" button always reports them.
fn run_cloud_save_detection(
    account_id: String,
    game_id: String,
    report_errors: bool,
    mut detecting: Signal<bool>,
    mut detected: Signal<Option<Vec<achv_gog::save_location::SaveLocation>>>,
    mut error: Signal<String>,
    mut path_input: Signal<String>,
    mut generation: Signal<u32>,
) {
    detecting.set(true);
    if report_errors {
        error.set(String::new());
    }
    tracing::debug!(account_id, product_id = %game_id, report_errors, "detecting gog cloud save location");
    spawn(async move {
        let Some(installed) = crate::core::installed_game(&account_id, &game_id) else {
            detecting.set(false);
            return;
        };
        match crate::core::detect_cloud_save_locations(&account_id, &installed).await {
            Ok(locations) if locations.is_empty() => {
                tracing::debug!(account_id, product_id = %game_id, "gog reports no cloud save location");
                if report_errors {
                    error.set("GOG no reporta una carpeta de guardado para este juego.".to_string());
                }
                detected.set(None);
            }
            Ok(locations) => {
                if let [only] = locations.as_slice() {
                    let resolved = only.path.to_string_lossy().into_owned();
                    path_input.set(resolved.clone());
                    match crate::core::set_cloud_save_path(&account_id, &game_id, Some(&resolved)) {
                        Ok(()) => {
                            tracing::info!(account_id, product_id = %game_id, path = resolved, "cloud save path auto-detected and saved");
                            generation.set(generation() + 1);
                        }
                        Err(err) if report_errors => {
                            tracing::error!(account_id, product_id = %game_id, error = %err, "failed to save auto-detected cloud save path");
                            error.set(err);
                        }
                        Err(err) => {
                            tracing::warn!(account_id, product_id = %game_id, error = %err, "failed to save auto-detected cloud save path, not reported to user");
                        }
                    }
                }
                detected.set(Some(locations));
            }
            Err(err) if report_errors => {
                tracing::error!(account_id, product_id = %game_id, error = %err, "cloud save location detection failed");
                error.set(err);
            }
            Err(err) => {
                tracing::debug!(account_id, product_id = %game_id, error = %err, "cloud save location detection failed, not reported to user");
            }
        }
        detecting.set(false);
    });
}

const fn sync_action_label(action: SyncAction) -> &'static str {
    match action {
        SyncAction::Upload => "Subido.",
        SyncAction::Download => "Descargado.",
        SyncAction::None => "Ya estaba al día.",
        SyncAction::Conflict => "Conflicto sin resolver.",
    }
}

/// Runs one sync cycle and, unless it ended in a conflict, records the new
/// sync timestamp on the manifest — a conflict leaves both sides exactly
/// as they were, so there's nothing to advance yet.
async fn run_cloud_sync(
    account_id: String,
    product_id: String,
    local_root: String,
    last_sync: Option<i64>,
    force: Option<ForceDirection>,
) -> Result<SyncOutcome, String> {
    tracing::info!(account_id, product_id, local_root, force = ?force, "cloud save sync started");
    let outcome = crate::achv_try!(
        achv_gog::cloud_saves::sync(
            &account_id,
            &product_id,
            std::path::Path::new(&local_root),
            last_sync,
            force
        )
        .await
        .map_err(|err| err.to_string())
        // Bubbles up to the properties tab's red error text.
        .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "cloud save sync failed"))
    );
    tracing::info!(account_id, product_id, action = ?outcome.action, "cloud save sync finished");
    if outcome.action != SyncAction::Conflict {
        crate::achv_try!(
            crate::core::record_cloud_save_sync(&account_id, &product_id, outcome.last_sync).inspect_err(
                |err| tracing::warn!(account_id, product_id, error = %err, "failed to record cloud save sync timestamp")
            )
        );
    }
    Ok(outcome)
}

/// Which DLC are installed alongside this game — Steam's own Properties →
/// DLC, applied as you go (ticking installs, unticking uninstalls) rather
/// than staged for later. What a *first* install of the base game fetches
/// alongside it is a different moment, decided in the install dialog
/// before the game exists at all; this tab is purely post-install
/// management, the same way Steam's is.
#[component]
fn DlcTab(game: Game, account_id: String) -> Element {
    let mut generation = use_signal(|| 0_u32);
    let mut busy = use_signal(|| None::<String>);
    let mut error = use_signal(String::new);

    let Some(installed) = crate::core::installed_game(&account_id, &game.id) else {
        return rsx! { p { class: "muted", "El juego no está instalado." } };
    };
    let _ = generation();

    // Read from the cached catalog rather than the live library: this
    // window has no `GogLibrary` context of its own to read (see the
    // module doc), and a plain cache read is all a DLC list needs.
    // GOG files an artbook and a playable expansion under the same `dlc`
    // type, but a goodie has no installer and no depot in the game's
    // build — ticking one could only ever fail, so it isn't offered.
    let (dlcs, goodies): (Vec<CachedGame>, Vec<CachedGame>) = crate::core::db()
        .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to open db, dlc list is empty"))
        .ok()
        .and_then(|db| {
            db.list_games(&account_id)
                .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to list cached games for dlc tab"))
                .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.parent_id.as_deref() == Some(game.id.as_str()))
        .partition(|entry| entry.installable);

    if dlcs.is_empty() && goodies.is_empty() {
        return rsx! { p { class: "muted", "Este juego no tiene contenido adicional." } };
    }

    rsx! {
        if !dlcs.is_empty() {
            div { class: "card",
                for dlc in dlcs {
                    {
                        let dlc_id = dlc.product_id.clone();
                        let title = dlc.title;
                        let checked = installed.dlcs.contains_key(&dlc_id);
                        let is_busy = busy() == Some(dlc_id.clone());
                        let account_id = account_id.clone();
                        let parent_id = game.id.clone();
                        rsx! {
                            label { class: "list-row", key: "{dlc_id}",
                                input {
                                    r#type: "checkbox",
                                    checked,
                                    disabled: is_busy,
                                    onchange: move |event| {
                                        let on = event.checked();
                                        let account_id = account_id.clone();
                                        let parent_id = parent_id.clone();
                                        let dlc_id = dlc_id.clone();
                                        let title = title.clone();
                                        error.set(String::new());
                                        busy.set(Some(dlc_id.clone()));
                                        tracing::info!(account_id, parent_id, dlc_id, on, "dlc install state toggled");
                                        spawn(async move {
                                            let outcome = if on {
                                                achv_gog::install(
                                                    &account_id,
                                                    &dlc_id,
                                                    Some(&parent_id),
                                                    &title,
                                                    None,
                                                    None,
                                                    |_| {},
                                                )
                                                .await
                                            } else {
                                                achv_gog::uninstall_dlc(&account_id, &dlc_id, &parent_id).await
                                            };
                                            busy.set(None);
                                            match outcome {
                                                Ok(()) => generation.set(generation() + 1),
                                                Err(err) => {
                                                    tracing::error!(account_id, dlc_id, on, error = %err, "dlc install state change failed");
                                                    error.set(err.to_string());
                                                }
                                            }
                                        });
                                    },
                                }
                                span { style: "flex: 1;", "{title}" }
                                if is_busy {
                                    span { class: "muted", "aplicando…" }
                                }
                            }
                        }
                    }
                }
            }
        }
        if !error().is_empty() {
            div { class: "error", "{error}" }
        }

        if !goodies.is_empty() {
            h3 { "Extras" }
            p { class: "muted",
                "Artbooks, bandas sonoras y similares. Son tuyos, pero GOG no los entrega como contenido instalable — se descargan aparte, y achv todavia no lo hace."
            }
            div { class: "card",
                for goodie in goodies {
                    div { class: "list-row", key: "{goodie.product_id}",
                        span { style: "flex: 1;", "{goodie.title}" }
                        span { class: "muted", "no instalable" }
                    }
                }
            }
        }
    }
}

/// Which Proton this game runs on, and the escape hatch when its prefix
/// goes bad — Steam's own Properties → Compatibilidad, moved out of a
/// library-wide screen since both are already scoped to one game.
#[component]
#[expect(
    clippy::redundant_clone,
    reason = "false positive attributed to this rsx! call site by clippy's MIR analysis of the \
              macro's own generated code, not a clone in this function's source"
)]
fn CompatibilityTab(game: Game, account_id: String) -> Element {
    let mut generation = use_signal(|| 0_u32);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);

    let Some(installed) = crate::core::installed_game(&account_id, &game.id) else {
        return rsx! {
            p { class: "muted", "El juego no está instalado." }
        };
    };
    let _ = generation();

    let runners = achv_runtime::steam_runners();
    let prefix = crate::core::prefix_for(&account_id, &game.id);
    // A prefix that was never launched into doesn't exist yet, which is
    // the normal state for a game you installed but haven't opened.
    let created = prefix.as_ref().is_some_and(|prefix| prefix.path().is_dir());
    let pinned = installed.proton;

    rsx! {
        div { class: "properties-rows",
            div { class: "properties-row",
                span { "Prefijo" }
                span {
                    if created {
                        "creado"
                    } else {
                        "sin prefijo (se crea al jugar)"
                    }
                }
            }
        }

        div { class: "field-row",
            select {
                onchange: {
                    let account_id = account_id.clone();
                    let game = game.clone();
                    move |event: Event<FormData>| {
                        let chosen = event.value();
                        let chosen = (!chosen.is_empty()).then_some(chosen);
                        match set_proton(&account_id, &game.id, chosen.as_deref()) {
                            Ok(()) => {
                                error.set(String::new());
                                generation.set(generation() + 1);
                            }
                            Err(err) => error.set(err),
                        }
                    }
                },
                option { value: "", selected: pinned.is_none(), "Usar el de por defecto" }
                for available in runners.clone() {
                    option {
                        key: "{available}",
                        value: "{available}",
                        selected: pinned.as_deref() == Some(available.as_str()),
                        "{available}"
                    }
                }
            }

            button {
                class: "secondary",
                disabled: !created || busy(),
                title: "Borra el prefijo. Se regenera al abrir el juego. Las partidas guardadas dentro del prefijo se pierden.",
                onclick: {
                    let account_id = account_id;
                    let game_id = game.id;
                    move |_| {
                        let account_id = account_id.clone();
                        let game_id = game_id.clone();
                        busy.set(true);
                        tracing::info!(account_id, product_id = %game_id, "resetting proton prefix");
                        spawn(async move {
                            match crate::launch::reset_prefix(account_id.clone(), game_id.clone()).await {
                                Ok(()) => {
                                    tracing::info!(account_id, product_id = %game_id, "proton prefix reset");
                                    error.set(String::new());
                                    generation.set(generation() + 1);
                                }
                                Err(err) => {
                                    tracing::error!(account_id, product_id = %game_id, error = %err, "failed to reset proton prefix");
                                    error.set(err);
                                }
                            }
                            busy.set(false);
                        });
                    }
                },
                if busy() {
                    "Reseteando..."
                } else {
                    "Resetear prefijo"
                }
            }
        }
        if !error().is_empty() {
            div { class: "error", "{error}" }
        }
    }
}

/// Every build GOG has published for the branch this game is on, newest
/// first — a manual rollback to a specific older build, distinct from
/// Betas (which picks *which branch*, always at that branch's newest).
#[component]
fn VersionsTab(game: Game, account_id: String) -> Element {
    let installed = crate::core::installed_game(&account_id, &game.id);
    let Some(installed) = installed else {
        return rsx! {
            p { class: "muted", "El juego no está instalado." }
        };
    };

    let mut builds = use_resource({
        let account_id = account_id.clone();
        let product_id = game.id.clone();
        let branch = installed.branch.clone();
        move || {
            let account_id = account_id.clone();
            let product_id = product_id.clone();
            let branch = branch.clone();
            async move { achv_gog::list_builds(&account_id, &product_id, branch.as_deref()).await }
        }
    });
    let mut installing = use_signal(|| None::<String>);
    let mut error = use_signal(String::new);

    match &*builds.read() {
        None => rsx! {
            p { class: "muted", "Cargando..." }
        },
        Some(Err(err)) => rsx! {
            div { class: "error", "{err}" }
        },
        Some(Ok(list)) => {
            let list: Vec<Build> = list.clone();
            rsx! {
                div { class: "card",
                    for build in list {
                        {
                            let label = build.version_name.clone().unwrap_or_else(|| build.build_id.clone());
                            let is_current = build.build_id == installed.build_id;
                            let busy = installing() == Some(build.build_id.clone());
                            let account_id = account_id.clone();
                            let product_id = game.id.clone();
                            let title = game.title.clone();
                            let branch = installed.branch.clone();
                            let target_build_id = build.build_id.clone();
                            rsx! {
                                div { class: "list-row", key: "{build.build_id}",
                                    span { style: "flex: 1;", "{label}" }
                                    if let Some(date) = &build.date_published {
                                        span { class: "muted", "{date}" }
                                    }
                                    if is_current {
                                        span { class: "badge", "ACTUAL" }
                                    } else {
                                        button {
                                            class: "secondary",
                                            disabled: installing().is_some(),
                                            onclick: move |_| {
                                                let account_id = account_id.clone();
                                                let product_id = product_id.clone();
                                                let title = title.clone();
                                                let branch = branch.clone();
                                                let target_build_id = target_build_id.clone();
                                                installing.set(Some(target_build_id.clone()));
                                                error.set(String::new());
                                                tracing::info!(account_id, product_id, target_build_id, "build rollback started");
                                                spawn(async move {
                                                    let outcome = achv_gog::install(
                                                            &account_id,
                                                            &product_id,
                                                            None,
                                                            &title,
                                                            branch.as_deref(),
                                                            Some(&target_build_id),
                                                            |_| {},
                                                        )
                                                        .await;
                                                    installing.set(None);
                                                    match outcome {
                                                        Ok(()) => {
                                                            tracing::info!(account_id, product_id, target_build_id, "build rollback finished");
                                                            builds.restart();
                                                        }
                                                        Err(err) => {
                                                            tracing::error!(account_id, product_id, target_build_id, error = %err, "build rollback failed");
                                                            error.set(err.to_string());
                                                        }
                                                    }
                                                });
                                            },
                                            if busy {
                                                "Instalando..."
                                            } else {
                                                "Instalar esta version"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                p { class: "muted",
                    "Instalar una version anterior vuelve a descargar el juego entero desde esa build. Verificar archivos la va a tratar como la version instalada, no como una desactualizacion."
                }
                if !error().is_empty() {
                    div { class: "error", "{error}" }
                }
            }
        }
    }
}

/// Pins (or unpins) a game's Proton by rewriting its manifest — the
/// manifests are the record, so there's nowhere else to put it.
fn set_proton(account_id: &str, product_id: &str, proton: Option<&str>) -> Result<(), String> {
    let library = crate::achv_try!(crate::core::library().inspect_err(
        |err| tracing::error!(account_id, product_id, error = %err, "failed to resolve library, proton pin not changed")
    ));
    // Re-read rather than editing the copy this tab rendered from, so a
    // change made elsewhere since isn't clobbered.
    let mut current = crate::achv_try!(
        crate::core::installed_game(account_id, product_id).ok_or("el juego ya no figura como instalado")
    );
    current.proton = proton.map(str::to_string);
    match library.write_manifest(&current) {
        Ok(()) => {
            tracing::info!(account_id, product_id, proton, "proton pin changed");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, product_id, proton, error = %err, "failed to save proton pin");
            Err(format!("guardando el manifiesto: {err}"))
        }
    }
}

#[component]
fn BetasTab(game: Game, account_id: String) -> Element {
    let mut branches = use_resource({
        let account_id = account_id.clone();
        let product_id = game.id.clone();
        move || {
            let account_id = account_id.clone();
            let product_id = product_id.clone();
            async move { achv_gog::list_branches(&account_id, &product_id).await }
        }
    });
    let mut switching = use_signal(|| None::<Option<String>>);
    let mut error = use_signal(String::new);

    let installed = crate::core::installed_game(&account_id, &game.id);
    let Some(installed) = installed else {
        return rsx! {
            p { class: "muted", "El juego no está instalado." }
        };
    };

    match &*branches.read() {
        None => rsx! {
            p { class: "muted", "Cargando..." }
        },
        Some(Err(err)) => rsx! {
            div { class: "error", "{err}" }
        },
        Some(Ok(list)) => {
            let list: Vec<Branch> = list.clone();
            rsx! {
                div { class: "card",
                    for branch in list {
                        {
                            let label = branch.name.clone().unwrap_or_else(|| "mainline".to_string());
                            let is_current = branch.name == installed.branch;
                            let busy = switching() == Some(branch.name.clone());
                            let account_id = account_id.clone();
                            let product_id = game.id.clone();
                            let title = game.title.clone();
                            let branch_name = branch.name.clone();
                            rsx! {
                                div { class: "list-row", key: "{label}",
                                    span { style: "flex: 1;", "{label}" }
                                    span { class: "muted", "build {branch.build_id}" }
                                    if is_current {
                                        span { class: "badge", "ACTUAL" }
                                    } else {
                                        button {
                                            class: "secondary",
                                            disabled: switching().is_some(),
                                            onclick: move |_| {
                                                let account_id = account_id.clone();
                                                let product_id = product_id.clone();
                                                let title = title.clone();
                                                let branch_name = branch_name.clone();
                                                switching.set(Some(branch_name.clone()));
                                                error.set(String::new());
                                                tracing::info!(account_id, product_id, branch = branch_name, "branch switch started");
                                                spawn(async move {
                                                    let outcome = achv_gog::install(
                                                            &account_id,
                                                            &product_id,
                                                            None,
                                                            &title,
                                                            branch_name.as_deref(),
                                                            None,
                                                            |_| {},
                                                        )
                                                        .await;
                                                    switching.set(None);
                                                    match outcome {
                                                        Ok(()) => {
                                                            tracing::info!(account_id, product_id, branch = branch_name, "branch switch finished");
                                                            branches.restart();
                                                        }
                                                        Err(err) => {
                                                            tracing::error!(account_id, product_id, branch = branch_name, error = %err, "branch switch failed");
                                                            error.set(err.to_string());
                                                        }
                                                    }
                                                });
                                            },
                                            if busy {
                                                "Cambiando..."
                                            } else {
                                                "Cambiar"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                p { class: "muted",
                    "Cambiar de rama vuelve a descargar el juego entero desde esa build. El contenido adicional ya instalado no se toca."
                }
                if !error().is_empty() {
                    div { class: "error", "{error}" }
                }
            }
        }
    }
}
