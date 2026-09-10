//! The right-hand pane: hero art with stat pills (achievements, completion,
//! playtime — Steam's playtime pill, adapted, but self-tracked: nothing
//! upstream has a notion of a play session), a favorite toggle, a real
//! Play/Stop action wired to whichever prefix this game was installed
//! into, and two tabs — Resumen (catalog metadata already on `Game` but
//! never shown before) and Logros.

use std::collections::{HashMap, HashSet};

use decanter_core::{CachedAchievement, Game, InstalledGame, Playtime};
use decanter_gog::Branch;
use dioxus::prelude::*;

use super::data::PollStatus;
use super::{data, fmt, install, properties};
use crate::CoreResource;
use crate::background::{Background, InstallStatus};

#[derive(Clone, Copy, PartialEq)]
enum DetailTab {
    Summary,
    Achievements,
}

#[component]
pub(super) fn DetailPane(
    game: Game,
    /// For [`CollectionChips`] only — collections are scoped to the
    /// profile. Everything else here (favorites, playtime, install,
    /// launch) uses `game.account_id`, the specific account this copy of
    /// the game belongs to.
    profile_id: String,
    achievements: Resource<Option<anyhow::Result<Vec<CachedAchievement>>>>,
    poll_status: PollStatus,
    core: CoreResource,
    running: crate::launch::Running,
    installing: Signal<HashMap<String, InstallStatus>>,
) -> Element {
    let account_id = game.account_id.clone();
    let mut tab = use_signal(|| DetailTab::Summary);
    let mut fav_generation = use_signal(|| 0_u32);
    // Which DLC to install alongside the game, shared between the
    // checklist that sets it and the INSTALAR button that acts on it.
    // Owned here because they are siblings; all ticked by default, the way
    // Steam does it. Reset per game, since this pane is reused rather than
    // remounted when the selection changes.
    let mut wanted_dlcs = use_signal(HashSet::<String>::new);
    use_effect({
        let game_id = game.id.clone();
        move || {
            let library = use_context::<crate::gog_library::GogLibrary>();
            let all: HashSet<String> = library
                .games
                .read()
                .iter()
                .filter(|entry| entry.parent_id.as_deref() == Some(game_id.as_str()))
                .map(|entry| entry.game.id.clone())
                .collect();
            wanted_dlcs.set(all);
        }
    });
    // Not created here: `playtime_generation` gets written from
    // `PlayAction`'s `spawn_forever`'d task, which always runs in the root
    // scope — the signal has to be owned there too (see `background`).
    let Background {
        playtime_generation,
        organisation,
        ..
    } = use_context::<Background>();

    let _ = fav_generation();
    let is_fav = data::is_favorite(&account_id, &game.id);

    let _ = playtime_generation();
    let playtime = data::load_playtime(&account_id, &game.id);

    // Not on `Game`: it's a fact about the product, and it lives on the
    // library entry alongside `kind` and `parent_id`.
    let installable = use_context::<crate::gog_library::GogLibrary>()
        .games
        .read()
        .iter()
        .find(|entry| entry.game.id == game.id)
        .is_none_or(|entry| entry.installable);

    let hero_style = game
        .background_url
        .as_deref()
        .or(game.cover_url.as_deref())
        .map(|url| format!("background-image: url('{url}')"))
        .unwrap_or_default();

    let achv_data = achievements.read();
    let (unlocked, total) = match &*achv_data {
        Some(Some(Ok(list))) => (list.iter().filter(|a| a.unlocked).count(), list.len()),
        _ => (0, 0),
    };

    rsx! {
        div { class: "game-hero", style: "{hero_style}",
            div { class: "game-hero-stats",
                div { class: "hero-stat",
                    span { class: "hero-stat-value", "{unlocked}/{total}" }
                    span { class: "hero-stat-label", "LOGROS" }
                }
                div { class: "hero-stat",
                    span { class: "hero-stat-value", "{fmt::duration(playtime.seconds_total)}" }
                    span { class: "hero-stat-label", "JUGADO" }
                }
            }
            div { class: "game-hero-overlay",
                span { class: "badge badge-gog", "GOG" }
                // Beside the title rather than at the end of the action
                // bar: it says something about the game, not something to
                // do with it, and buried after DESINSTALAR is the last
                // place anyone looks for it.
                div { class: "game-hero-title",
                    h1 { "{game.title}" }
                    {
                        let game_id = game.id.clone();
                        let account_id_for_fav = account_id.clone();
                        rsx! {
                            button {
                                class: if is_fav { "icon-btn favorite-btn active" } else { "icon-btn favorite-btn" },
                                title: if is_fav { "Quitar de favoritos" } else { "Agregar a favoritos" },
                                onclick: move |_| {
                                    data::set_favorite(&account_id_for_fav, &game_id, !is_fav);
                                    fav_generation.set(fav_generation() + 1);
                                    // The sidebar and cover browser both have a
                                    // FAVORITOS shelf; without this they'd keep
                                    // showing the pre-toggle grouping.
                                    crate::background::bump_organisation(organisation);
                                },
                                if is_fav { "★" } else { "☆" }
                            }
                        }
                    }
                }
            }
        }

        div { class: "game-action-bar",
            {
                let is_installed = game.install_state.as_ref().is_some_and(|state| state.installed);
                rsx! {
                    if is_installed {
                        PlayAction {
                            game: game.clone(),
                            account_id: account_id.clone(),
                            running,
                            playtime_generation,
                        }
                        UninstallAction { account_id: account_id.clone(), product_id: game.id.clone() }
                    } else {
                        InstallAction {
                            game: game.clone(),
                            account_id: account_id.clone(),
                            installing,
                            wanted_dlcs,
                            installable,
                        }
                    }
                    if is_installed {
                        {
                            let game = game.clone();
                            let account_id = account_id.clone();
                            rsx! {
                                button {
                                    class: "icon-btn",
                                    title: "Propiedades",
                                    onclick: move |_| properties::open_properties_window(
                                        game.clone(),
                                        account_id.clone(),
                                    ),
                                    "Propiedades"
                                }
                            }
                        }
                    }
                }
            }
        }

        CollectionChips { profile_id, product_id: game.id.clone(), organisation }

        div { class: "tabs",
            button {
                class: if tab() == DetailTab::Summary { "active" } else { "" },
                onclick: move |_| tab.set(DetailTab::Summary),
                "Resumen"
            }
            button {
                class: if tab() == DetailTab::Achievements { "active" } else { "" },
                onclick: move |_| tab.set(DetailTab::Achievements),
                "Logros"
            }
        }

        match tab() {
            DetailTab::Summary => summary_tab(&game, playtime),
            DetailTab::Achievements => achievements_tab(&achv_data, &poll_status),
        }
    }
}

fn summary_tab(game: &Game, playtime: Playtime) -> Element {
    rsx! {
        div { class: "stat-row",
            div { class: "stat-card",
                div { class: "num", "{fmt::duration(playtime.seconds_total)}" }
                div { class: "lbl", "Tiempo jugado" }
            }
            if let Some(last) = playtime.last_played_at.and_then(fmt::unix_date) {
                div { class: "stat-card",
                    div { class: "num", "{last}" }
                    div { class: "lbl", "Ultima sesion" }
                }
            }
            if let Some(rating) = &game.rating {
                div { class: "stat-card",
                    div { class: "num", "{rating}" }
                    div { class: "lbl", "Calificacion" }
                }
            }
            if let Some(date) = game.release_date.and_then(fmt::unix_date) {
                div { class: "stat-card",
                    div { class: "num", "{date}" }
                    div { class: "lbl", "Lanzamiento" }
                }
            }
        }
        div { class: "summary-text",
            if let Some(desc) = &game.description {
                p { class: "muted", "{desc}" }
            }
            if game.developer.is_some() || game.publisher.is_some() {
                p { class: "muted",
                    if let Some(dev) = &game.developer {
                        "Desarrollador: {dev}  "
                    }
                    if let Some(publisher) = &game.publisher {
                        "Editor: {publisher}"
                    }
                }
            }
            if !game.genres.is_empty() {
                p { class: "muted", "Generos: {game.genres.join(\", \")}" }
            }
        }
    }
}

/// The cache being empty is ambiguous on its own — still loading, this
/// game genuinely has no achievements, or the poll failed. `poll_status`
/// (the live poll that just ran, or is running, for this exact game)
/// disambiguates it instead of always pointing at a CLI command.
fn achievements_tab(
    achv_data: &Option<Option<anyhow::Result<Vec<CachedAchievement>>>>,
    poll_status: &PollStatus,
) -> Element {
    let cached = match achv_data {
        Some(Some(Ok(list))) if !list.is_empty() => Some(list.clone()),
        _ => None,
    };

    if let Some(achievements) = cached {
        return rsx! { AchievementGrid { achievements } };
    }

    if let Some(Some(Err(err))) = achv_data {
        return rsx! {
            div { class: "error", style: "margin: 20px 32px 0;", "Error leyendo cache de logros: {err}" }
        };
    }

    match poll_status {
        PollStatus::Polling => rsx! {
            p { class: "muted", style: "margin: 20px 32px 0;", "Buscando logros..." }
        },
        PollStatus::Done => rsx! {
            div { class: "empty achv-empty", "Este juego no tiene logros en GOG." }
        },
        PollStatus::Failed(err) => rsx! {
            div { class: "error", style: "margin: 20px 32px 0;", "No se pudo consultar logros: {err}" }
        },
        PollStatus::Idle => rsx! {
            p { class: "muted", style: "margin: 20px 32px 0;", "Cargando..." }
        },
    }
}

#[component]
fn AchievementGrid(achievements: Vec<CachedAchievement>) -> Element {
    rsx! {
        div { class: "achv-grid",
            for achievement in achievements {
                div {
                    class: if achievement.unlocked { "achv-tile unlocked" } else { "achv-tile locked" },
                    key: "{achievement.achievement_key}",
                    div { class: "achv-icon",
                        if let Some(icon) = icon_for(&achievement) {
                            img { src: "{icon}", alt: "" }
                        }
                    }
                    div { class: "achv-info",
                        div { class: "achv-name", "{achievement.name}" }
                        if let Some(desc) = &achievement.description {
                            div { class: "achv-desc muted", "{desc}" }
                        }
                        if let Some(at) = &achievement.unlocked_at {
                            div { class: "achv-date muted", "{at}" }
                        }
                    }
                }
            }
        }
    }
}

fn icon_for(achievement: &CachedAchievement) -> Option<&str> {
    if achievement.unlocked {
        achievement
            .icon_unlocked_url
            .as_deref()
            .or(achievement.icon_locked_url.as_deref())
    } else {
        achievement
            .icon_locked_url
            .as_deref()
            .or(achievement.icon_unlocked_url.as_deref())
    }
}

/// The installed game behind a catalog entry, or `None` if this profile
/// doesn't have it. One manifest read — the manifest names the files, the
/// build and what launches it.
pub(super) fn resolve_launch_target(account_id: &str, game: &Game) -> Option<InstalledGame> {
    let product_id = game.id.as_str();
    let Some(installed) = crate::core::installed_game(account_id, product_id) else {
        tracing::debug!(account_id, product_id, "no install manifest found, not launchable");
        return None;
    };
    // No executable means installed-but-not-launchable, which the caller
    // renders differently from not installed at all.
    let Some(_) = installed.launch_executable.as_ref() else {
        tracing::warn!(account_id, product_id, "installed but no launch executable resolved");
        return None;
    };
    Some(installed)
}

/// Where this game's files live on disk, once it's installed. Unlike
/// [`resolve_launch_target`], this doesn't need a launch executable -- it
/// works for the "instalado, pero sin ejecutable detectado" case too,
/// where there's nothing to start but the files are still there to look at.
pub(super) fn game_install_path(account_id: &str, product_id: &str) -> Option<std::path::PathBuf> {
    let Some(installed) = crate::core::installed_game(account_id, product_id) else {
        tracing::debug!(
            account_id,
            product_id,
            "no install manifest found, no install path to show"
        );
        return None;
    };
    let Some(library) = crate::core::library()
        .inspect_err(|err| tracing::warn!(account_id, product_id, error = %err, "failed to resolve library root"))
        .ok()
    else {
        return None;
    };
    library.install_dir(&installed.install_dir)
}

/// Hands a path to whatever the desktop's own file manager is -- the same
/// mechanism the rest of the app uses to hand a path to the desktop.
pub(super) fn open_in_file_manager(path: &std::path::Path) {
    if let Err(err) = std::process::Command::new("xdg-open").arg(path).spawn() {
        tracing::warn!("couldn't open {}: {err}", path.display());
    }
}

/// The actual launch, shared between the detail pane's PLAY button and the
/// right-click menu's -- both just need to fire this and move on. The
/// session bookkeeping lives in `crate::launch`; what's here on top of it
/// is the playtime tally. `spawn_forever`, not `spawn`, at every call
/// site: this outlives whatever button started it, the same reasoning
/// `start_install` below documents.
pub(super) async fn launch_game(
    account_id: String,
    game: InstalledGame,
    running: crate::launch::Running,
    mut playtime_generation: Signal<u32>,
) {
    let product_id = game.game_id.clone();
    tracing::info!(account_id, product_id, "launching game");
    let Some(played) = crate::launch::start(account_id.clone(), game, running).await else {
        return;
    };
    tracing::debug!(
        account_id,
        product_id,
        seconds = played.as_secs(),
        "recording play session"
    );
    data::add_playtime(&account_id, &product_id, played.as_secs() as i64);
    playtime_generation.set(playtime_generation() + 1);
}

#[component]
fn PlayAction(
    game: Game,
    account_id: String,
    running: crate::launch::Running,
    playtime_generation: Signal<u32>,
) -> Element {
    let Some(installed) = resolve_launch_target(&account_id, &game) else {
        return rsx! { span { class: "muted", "Instalado, pero sin ejecutable detectado" } };
    };

    let product_id = game.id;
    if running.read().contains(&product_id) {
        let stop_profile = account_id;
        return rsx! {
            button {
                class: "play-btn secondary",
                // Plain `spawn` is fine here — this doesn't write to any
                // Signal itself, `launch::start`'s own (root-scoped,
                // spawn_forever'd) task is what notices the session ended
                // and updates `running`.
                onclick: move |_| {
                    tracing::info!(account_id = %stop_profile, product_id, "stopping game");
                    spawn(crate::launch::stop(stop_profile.clone(), product_id.clone()));
                },
                "DETENER"
            }
        };
    }

    rsx! {
        button {
            class: "play-btn",
            // spawn_forever: `running.write()` inside `launch_game` flips
            // this button out of existence (swaps to DETENER) — a plain
            // `spawn` ties the task to *this* component scope and would
            // get cancelled by that very re-render, exactly the bug that
            // broke installs the same way.
            onclick: move |_| {
                dioxus::core::spawn_forever(launch_game(
                    account_id.clone(),
                    installed.clone(),
                    running,
                    playtime_generation,
                ));
            },
            "JUGAR"
        }
    }
}

/// Not installed yet: one button, no questions.
///
/// The `installing` map (lifted to `Library`, not local state) is both the
/// progress display and the guard against a second install for the same
/// game while one is in flight.
#[component]
fn InstallAction(
    game: Game,
    account_id: String,
    installing: Signal<HashMap<String, InstallStatus>>,
    wanted_dlcs: Signal<HashSet<String>>,
    installable: bool,
) -> Element {
    // Owned at the app root, so the install task can refresh this game's
    // install state once it finishes without going back out to GOG.
    let library = use_context::<crate::gog_library::GogLibrary>();
    let mut confirm_open = use_signal(|| false);

    // GOG carries superseded listings — an old edition someone owns
    // alongside the current one — with nothing behind them to install.
    // Pressing install on one only ever fails, so it isn't offered.
    if !installable {
        return rsx! {
            span { class: "muted", "GOG no ofrece descarga para esta edicion" }
        };
    }

    let status = installing.read().get(&game.id).cloned();

    match status {
        Some(InstallStatus::Queued) => {
            let account_id = account_id;
            let product_id = game.id;
            rsx! {
                div { class: "install-progress",
                    div { class: "bar-track indeterminate", div { class: "bar-fill", style: "width: 100%" } }
                    div { class: "muted install-progress-label", "En cola…" }
                }
                button {
                    class: "icon-btn",
                    onclick: move |_| {
                        spawn(install::cancel(account_id.clone(), product_id.clone()));
                    },
                    "Cancelar"
                }
            }
        }
        Some(InstallStatus::Installing {
            current_file,
            bytes_downloaded,
            total_bytes,
            bytes_per_second,
        }) => {
            let fraction = InstallStatus::Installing {
                current_file: current_file.clone(),
                bytes_downloaded,
                total_bytes,
                bytes_per_second,
            }
            .fraction();
            let width = fraction.map_or_else(|| "100%".to_string(), |value| format!("{:.1}%", value * 100.0));
            let remaining = total_bytes.unwrap_or(0).saturating_sub(bytes_downloaded);
            let account_id = account_id;
            let product_id = game.id;
            rsx! {
                div { class: "install-progress",
                    div { class: if fraction.is_some() { "bar-track" } else { "bar-track indeterminate" },
                        div { class: "bar-fill", style: "width: {width}" }
                    }
                    div { class: "muted install-progress-label",
                        "{crate::format::bytes(bytes_downloaded)}"
                        if let Some(total) = total_bytes {
                            " / {crate::format::bytes(total)}"
                        }
                        " · {crate::format::rate(bytes_per_second)}"
                        if let Some(left) = crate::format::eta(remaining, bytes_per_second) {
                            " · faltan {left}"
                        }
                    }
                    div { class: "muted download-file", "{current_file}" }
                }
                button {
                    class: "icon-btn",
                    onclick: move |_| {
                        let account_id = account_id.clone();
                        let product_id = product_id.clone();
                        spawn(install::cancel(account_id, product_id));
                    },
                    "Cancelar"
                }
            }
        }
        Some(InstallStatus::Failed(err)) => {
            let game_id = game.id;
            let mut installing = installing;
            rsx! {
                div { class: "error", "Instalacion fallida: {err}" }
                button {
                    class: "secondary",
                    onclick: move |_| { installing.write().remove(&game_id); },
                    "Descartar"
                }
            }
        }
        // Nothing to choose: the files go to the shared library and the
        // prefix is derived from (profile, game) when the game first runs.
        None => {
            let account_id = account_id;
            let product_id = game.id.clone();
            let game_title = game.title.clone();
            let dlc_count = wanted_dlcs.read().len();
            rsx! {
                button {
                    class: "play-btn secondary",
                    onclick: move |_| confirm_open.set(true),
                    "INSTALAR"
                }
                if confirm_open() {
                    InstallDialog {
                        game: game,
                        account_id: account_id.clone(),
                        dlc_count,
                        on_confirm: move |branch: Option<String>| {
                            confirm_open.set(false);
                            // Whatever the DLC checklist has ticked rides along
                            // with the game, the way Steam installs them.
                            let ticked = wanted_dlcs.read().clone();
                            let dlcs: Vec<(String, String)> = library
                                .games
                                .read()
                                .iter()
                                .filter(|entry| ticked.contains(&entry.game.id))
                                .map(|entry| (entry.game.id.clone(), entry.game.title.clone()))
                                .collect();
                            start_install(
                                account_id.clone(),
                                product_id.clone(),
                                None,
                                game_title.clone(),
                                branch,
                                dlcs,
                                installing,
                                library,
                            );
                        },
                        on_close: move |()| confirm_open.set(false),
                    }
                }
            }
        }
    }
}

/// Confirms an install before it starts, the way Steam does — where the
/// files go, and which branch to install (mainline by default, same as
/// Steam defaults to no beta opt-in). There's only one library location
/// today, so that row is informational rather than a picker; the branch
/// choice is the real one.
#[component]
fn InstallDialog(
    game: Game,
    account_id: String,
    dlc_count: usize,
    on_confirm: EventHandler<Option<String>>,
    on_close: EventHandler<()>,
) -> Element {
    let branches = use_resource({
        let account_id = account_id;
        let product_id = game.id.clone();
        move || {
            let account_id = account_id.clone();
            let product_id = product_id.clone();
            async move {
                tracing::debug!(account_id, product_id, "fetching gog branch list for install dialog");
                decanter_gog::list_branches(&account_id, &product_id).await.inspect_err(
                    |err| tracing::error!(account_id, product_id, error = %err, "failed to fetch branch list"),
                )
            }
        }
    });
    let mut chosen = use_signal(|| None::<String>);
    let library_path = crate::core::library().ok().map(|library| library.common());

    rsx! {
        div {
            class: "modal-backdrop",
            onclick: move |_| on_close.call(()),
            div {
                class: "modal-panel properties-panel",
                onclick: move |event| event.stop_propagation(),

                div { class: "modal-header",
                    h2 { "Instalar — {game.title}" }
                    button { class: "icon-btn", onclick: move |_| on_close.call(()), "✕" }
                }

                div { class: "properties-body",
                    div { class: "properties-rows",
                        if let Some(path) = &library_path {
                            div { class: "properties-row", span { "Ubicación" } span { "{path.display()}" } }
                        }
                        if dlc_count > 0 {
                            div { class: "properties-row",
                                span { "Contenido adicional" }
                                span { "{dlc_count} marcado(s), se instala junto con el juego" }
                            }
                        }
                    }

                    div { class: "field-row",
                        span { "Rama" }
                        match &*branches.read() {
                            None => rsx! { span { class: "muted", "Cargando..." } },
                            Some(Err(err)) => rsx! { span { class: "error", "{err}" } },
                            Some(Ok(list)) => {
                                let list: Vec<Branch> = list.clone();
                                rsx! {
                                    select {
                                        onchange: move |event: Event<FormData>| {
                                            let value = event.value();
                                            chosen.set((!value.is_empty()).then_some(value));
                                        },
                                        option { value: "", selected: chosen().is_none(), "Mainline (por defecto)" }
                                        for branch in list {
                                            if let Some(name) = branch.name {
                                                option {
                                                    key: "{name}",
                                                    value: "{name}",
                                                    selected: chosen().as_deref() == Some(name.as_str()),
                                                    "{name}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "field-row",
                        button { class: "play-btn", onclick: move |_| on_confirm.call(chosen()), "Instalar" }
                        button { class: "secondary", onclick: move |_| on_close.call(()), "Cancelar" }
                    }
                }
            }
        }
    }
}

/// `branch` pins the base game to a specific GOG branch (a beta, say) —
/// ignored when `parent_id` is `Some`, same as `decanter_gog::install`
/// itself: a DLC's branch always comes from the base game it belongs to.
pub(super) fn start_install(
    account_id: String,
    product_id: String,
    parent_id: Option<String>,
    title: String,
    branch: Option<String>,
    dlcs: Vec<(String, String)>,
    mut installing: Signal<HashMap<String, InstallStatus>>,
    library: crate::gog_library::GogLibrary,
) {
    installing.write().insert(product_id.clone(), InstallStatus::Queued);
    // `spawn` (not `spawn_forever`) ties a task's lifetime to whichever
    // component scope was "current" when it started — here, the button
    // inside `InstallAction` that's about to disappear the moment
    // `installing` flips this game to `Installing` and the action bar
    // re-renders into the progress view. That's the real cause of a
    // download dying mid-transfer (h2 GOAWAY from *our* side, surfaced by
    // the server as "Download was cancelled") — nothing server-side was
    // wrong. `spawn_forever` runs in the root scope instead, so no UI
    // re-render (switching tabs, navigating away, this component's own
    // status change) can tear it down.
    dioxus::core::spawn_forever(install::install(
        account_id,
        product_id.clone(),
        parent_id,
        title,
        branch,
        dlcs,
        product_id,
        installing,
        library,
    ));
}

/// Always safe to call — uninstalling something that isn't installed is a
/// no-op — but it deletes real files, so it still gets a confirm step: an
/// in-UI two-click, not a native `confirm()` (unreliable across the
/// desktop webview).
#[component]
fn UninstallAction(account_id: String, product_id: String) -> Element {
    let library = use_context::<crate::gog_library::GogLibrary>();
    let mut confirming = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    if busy() {
        return rsx! { span { class: "muted", "Desinstalando..." } };
    }

    if confirming() {
        rsx! {
            button {
                class: "icon-btn",
                onclick: move |_| {
                    busy.set(true);
                    let account_id = account_id.clone();
                    let product_id = product_id.clone();
                    // Plain `spawn`: writes only to `busy`/`confirming`/
                    // `error`, all owned by this same component, and a
                    // single quick RPC call — no cross-scope risk.
                    spawn(async move {
                        if let Err(err) = install::uninstall(account_id, product_id, library).await {
                            error.set(Some(err));
                        }
                        busy.set(false);
                        confirming.set(false);
                    });
                },
                "Confirmar desinstalacion"
            }
            button { class: "icon-btn", onclick: move |_| confirming.set(false), "Cancelar" }
            if let Some(err) = error() {
                span { class: "error", "{err}" }
            }
        }
    } else {
        rsx! {
            button {
                class: "icon-btn",
                title: "Desinstalar",
                onclick: move |_| confirming.set(true),
                "Desinstalar"
            }
        }
    }
}

/// Which shelves this game sits on. Read-only here — membership is edited
/// from the right-click menu in the list, where you can see the shelf you
/// are putting it on.
#[component]
fn CollectionChips(profile_id: String, product_id: String, organisation: Signal<u32>) -> Element {
    let _ = organisation();
    let collections = data::load_collections(&profile_id);

    let member_of: Vec<String> = collections
        .list
        .iter()
        .filter(|collection| collections.contains(&collection.id, &product_id))
        .map(|collection| collection.name.clone())
        .collect();

    // Favourite isn't a shelf: the star on the hero says it, and a chip
    // here would only repeat it. An empty row renders nothing.
    if member_of.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "collection-chips",
            for name in member_of {
                span { class: "chip", key: "{name}", "{name}" }
            }
        }
    }
}
