//! The right-hand pane: hero art with stat pills (achievements, completion,
//! playtime — Steam's playtime pill, adapted, but self-tracked: nothing
//! upstream has a notion of a play session), a favorite toggle, a real
//! Play/Stop action wired to whichever prefix this game was installed
//! into, and two tabs — Resumen (catalog metadata already on `Game` but
//! never shown before) and Logros.

use std::path::PathBuf;
use std::rc::Rc;

use decanter_core::{CachedAchievement, Game, InstallState, InstalledGame, Playtime};
use decanter_gog::{Branch, LibraryEntry};
use gpui::{
    AnyElement, BoxShadow, Context, Div, Entity, FontWeight, ObjectFit, Pixels, SharedString, Task, WeakEntity, Window,
    canvas, div, img, point, prelude::*, px, relative, rgb,
};

use super::data::PollStatus;
use super::install::InstallRequest;
use super::{LibraryModel, actions, data, fmt};
use crate::state::{AppState, Background, InstallStatus};
use crate::theme;
use crate::views::downloads::progress_lines;
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button, menu, stat_card, tabs, tooltip};

/// `.achv-grid`'s `repeat(auto-fill, minmax(260px, 1fr))` and its gap.
const ACHV_MIN_COLUMN: f32 = 260.;
const ACHV_GAP: f32 = 10.;

#[derive(Clone, Copy, PartialEq, Default)]
enum DetailTab {
    #[default]
    Summary,
    Achievements,
}

/// The sqlite and manifest reads behind the pane, cached because `render`
/// runs on every image arrival and install progress tick. Each part is
/// re-read only when the counter or field it depends on moves.
struct Loaded {
    product_id: String,
    account_id: String,
    profile_id: Option<String>,
    organisation: u32,
    playtime_generation: u32,
    install_state: Option<InstallState>,
    is_favorite: bool,
    playtime: Playtime,
    /// Names of the profile's collections this game sits on.
    collections: Vec<String>,
    launch: Option<InstalledGame>,
}

impl Loaded {
    fn load(game: &Game, profile_id: Option<&str>, background: &Background) -> Self {
        Self {
            product_id: game.id.clone(),
            account_id: game.account_id.clone(),
            profile_id: profile_id.map(str::to_owned),
            organisation: background.organisation,
            playtime_generation: background.playtime_generation,
            install_state: game.install_state.clone(),
            is_favorite: data::is_favorite(&game.account_id, &game.id),
            playtime: data::load_playtime(&game.account_id, &game.id),
            collections: member_of(profile_id, &game.id),
            launch: launch_target(game),
        }
    }

    fn is_for(&self, game: &Game) -> bool {
        (self.product_id.as_str(), self.account_id.as_str()) == (game.id.as_str(), game.account_id.as_str())
    }

    fn refresh(&mut self, game: &Game, profile_id: Option<&str>, background: &Background) {
        if self.organisation != background.organisation || self.profile_id.as_deref() != profile_id {
            self.organisation = background.organisation;
            self.profile_id = profile_id.map(str::to_owned);
            self.is_favorite = data::is_favorite(&game.account_id, &game.id);
            self.collections = member_of(profile_id, &game.id);
        }
        if self.playtime_generation != background.playtime_generation {
            self.playtime_generation = background.playtime_generation;
            self.playtime = data::load_playtime(&game.account_id, &game.id);
        }
        if self.install_state != game.install_state {
            self.install_state.clone_from(&game.install_state);
            self.launch = launch_target(game);
        }
    }
}

fn launch_target(game: &Game) -> Option<InstalledGame> {
    let installed = game.install_state.as_ref().is_some_and(|state| state.installed);
    if installed {
        actions::resolve_launch_target(&game.account_id, game)
    } else {
        None
    }
}

/// Collections are scoped to the profile, unlike everything else here
/// (favorites, playtime, install, launch), which uses `game.account_id`,
/// the specific account this copy of the game belongs to.
fn member_of(profile_id: Option<&str>, product_id: &str) -> Vec<String> {
    let Some(profile_id) = profile_id else {
        return Vec::new();
    };
    let collections = data::load_collections(profile_id);
    collections
        .list
        .iter()
        .filter(|collection| collections.contains(&collection.id, product_id))
        .map(|collection| collection.name.clone())
        .collect()
}

/// What the action bar shows, read from the library and background state
/// once per render.
struct BarState {
    launch: Option<InstalledGame>,
    running: bool,
    /// Not on `Game`: it's a fact about the product, and it lives on the
    /// library entry alongside `kind` and `parent_id`.
    installable: bool,
    status: Option<InstallStatus>,
    dlcs: Vec<(String, String)>,
}

#[derive(Default)]
struct Uninstall {
    confirming: bool,
    busy: bool,
    error: Option<String>,
}

struct InstallDialog {
    library_path: Option<PathBuf>,
    branches: Option<Result<Vec<Branch>, String>>,
    chosen: Option<String>,
    /// Dropping the dialog drops this, so a branch list still in flight
    /// for a closed dialog is abandoned rather than written back.
    _fetch: Task<()>,
}

/// UI state that belongs to one game and starts over when the selection
/// moves to another, the way the webview's per-game components did.
#[derive(Default)]
struct Local {
    tab: DetailTab,
    uninstall: Uninstall,
    dialog: Option<InstallDialog>,
}

pub struct DetailView {
    model: Entity<LibraryModel>,
    loaded: Option<Loaded>,
    local: Local,
    /// Width the achievement grid last laid out at, so the column count
    /// can follow `auto-fill`; zero until the first measurement lands.
    grid_width: Pixels,
}

impl DetailView {
    pub fn new(model: Entity<LibraryModel>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.session, |_, _, cx| cx.notify()).detach();
        Self {
            model,
            loaded: None,
            local: Local::default(),
            grid_width: px(0.),
        }
    }

    fn sync(&mut self, game: &Game, profile_id: Option<&str>, background: &Background) {
        match &mut self.loaded {
            Some(loaded) if loaded.is_for(game) => loaded.refresh(game, profile_id, background),
            _ => {
                self.loaded = Some(Loaded::load(game, profile_id, background));
                self.local = Local::default();
            }
        }
    }

    fn open_install_dialog(&mut self, account_id: String, product_id: String, cx: &mut Context<Self>) {
        let library_path = crate::core::library_for_account(&account_id)
            .inspect_err(
                |err| tracing::warn!(account_id, error = %err, "failed to resolve library root for install dialog"),
            )
            .ok()
            .map(|library| library.common());
        let fetch = cx.spawn(async move |this, cx| {
            tracing::debug!(account_id, product_id, "fetching gog branch list for install dialog");
            let result = decanter_gog::list_branches(&account_id, &product_id)
                .await
                .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "failed to fetch branch list"))
                .map_err(|err| err.to_string());
            let _ = this.update(cx, |this, cx| {
                if let Some(dialog) = &mut this.local.dialog {
                    dialog.branches = Some(result);
                    cx.notify();
                }
            });
        });
        self.local.dialog = Some(InstallDialog {
            library_path,
            branches: None,
            chosen: None,
            _fetch: fetch,
        });
        cx.notify();
    }

    fn uninstall(&mut self, account_id: String, product_id: String, cx: &mut Context<Self>) {
        self.local.uninstall.busy = true;
        cx.notify();
        let state = AppState::get(cx).clone();
        // Detached rather than held by the view: it deletes real files, so
        // selecting another game mid-way mustn't cancel it half done.
        cx.spawn(async move |this, cx| {
            let result = super::install::uninstall(account_id.clone(), product_id.clone(), state, cx).await;
            let _ = this.update(cx, |this, cx| {
                // The pane may have moved on to another game by now, whose
                // confirm step this result has nothing to do with.
                let still_shown = this
                    .loaded
                    .as_ref()
                    .is_some_and(|loaded| loaded.product_id == product_id && loaded.account_id == account_id);
                if !still_shown {
                    return;
                }
                let uninstall = &mut this.local.uninstall;
                if let Err(err) = result {
                    uninstall.error = Some(err);
                }
                uninstall.busy = false;
                uninstall.confirming = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::get(cx).clone();
        let Some(selected) = self.model.read(cx).selected.clone() else {
            return div().into_any_element();
        };
        let games = &state.library.read(cx).games;
        let Some(entry) = games.iter().find(|entry| entry.game.id == selected) else {
            return div().into_any_element();
        };
        let game = entry.game.clone();
        let installable = entry.installable;
        let dlcs: Vec<(String, String)> = dlcs_of(games, &game.id).collect();
        let profile_id = state.session.read(cx).active_profile.clone();
        let background = state.background.read(cx);
        self.sync(&game, profile_id.as_deref(), background);
        let running = background.running.contains(&game.id);
        let install_status = background.installing.get(&game.id).cloned();

        let (achievements, poll_status) = {
            let model = self.model.read(cx);
            (model.achievements.clone(), model.poll_status.clone())
        };
        let (unlocked, total) = match &achievements {
            Some(Ok(list)) => (list.iter().filter(|a| a.unlocked).count(), list.len()),
            _ => (0, 0),
        };

        let Some(loaded) = &self.loaded else {
            return div().into_any_element();
        };
        let is_fav = loaded.is_favorite;
        let playtime = loaded.playtime;
        let collections = loaded.collections.clone();
        let launch = loaded.launch.clone();

        let hero = hero(&game, unlocked, total, playtime, is_fav, cx);
        let bar = BarState {
            launch,
            running,
            installable,
            status: install_status,
            dlcs,
        };
        let action_bar = self.action_bar(&game, bar, window, cx);

        let active = self.local.tab;
        let tab_row = tabs(
            "detail-tabs",
            &[(DetailTab::Summary, "Resumen"), (DetailTab::Achievements, "Logros")],
            active,
            {
                let this = cx.entity().downgrade();
                move |tab, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.local.tab = tab;
                        cx.notify();
                    });
                }
            },
        );
        let body = match active {
            DetailTab::Summary => summary_tab(&game, playtime).into_any_element(),
            DetailTab::Achievements => self.achievements_tab(achievements.as_ref(), &poll_status, cx),
        };

        div()
            .flex()
            .flex_col()
            .child(hero)
            .child(action_bar)
            .children(collection_chips(collections))
            .child(div().mx(px(32.)).mt(px(24.)).child(tab_row))
            .child(body)
            .into_any_element()
    }
}

/// Every DLC of `product_id`, as `(product id, title)`. All of them ride
/// along with the game, the way Steam installs them.
fn dlcs_of<'games>(
    games: &'games [LibraryEntry],
    product_id: &'games str,
) -> impl Iterator<Item = (String, String)> + 'games {
    games
        .iter()
        .filter(move |entry| entry.parent_id.as_deref() == Some(product_id))
        .map(|entry| (entry.game.id.clone(), entry.game.title.clone()))
}

fn hero(game: &Game, unlocked: usize, total: usize, playtime: Playtime, is_fav: bool, cx: &Context<DetailView>) -> Div {
    let art = game.background_url.as_deref().or(game.cover_url.as_deref());
    let hero = div()
        .relative()
        .min_h(px(300.))
        .overflow_hidden()
        .flex()
        .items_end()
        .bg(rgb(theme::BG_PANEL_2));
    // The inline `background-image` replaced the whole gradient stack in the
    // webview build, so the gradients only ever showed with no art at all.
    let hero = match art {
        Some(url) => hero.child(
            // Wide enough for the detail pane on a maximized 1080p window;
            // the hero's height follows its text, 300 px at the least.
            img(crate::art::source(url, gpui::size(1600., 400.)))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => hero
            .child(
                // GPUI has no radial gradient: a diagonal two-stop linear
                // one stands in for the light-at-top-left radial.
                div().absolute().top_0().left_0().size_full().bg(gpui::linear_gradient(
                    150.,
                    gpui::linear_color_stop(rgb(theme::BG_PANEL_LIGHT), 0.),
                    gpui::linear_color_stop(rgb(theme::BG_PANEL_3), 1.),
                )),
            )
            .child(
                // 0 → .55 at 55% → 1 is a straight line, so two stops are exact.
                div().absolute().top_0().left_0().size_full().bg(gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.),
                    gpui::linear_color_stop(theme::alpha(theme::BG_DEEP, 1.), 1.),
                )),
            ),
    };

    let account_id = game.account_id.clone();
    let product_id = game.id.clone();
    let favorite = button("favorite", ButtonKind::Icon, if is_fav { "★" } else { "☆" }, false)
        .text_size(px(26.))
        .line_height(relative(1.))
        .px(px(6.))
        .py(px(4.))
        .when(is_fav, |star| star.text_color(rgb(theme::ACCENT_GOLD)))
        .tooltip(tooltip(if is_fav {
            "Quitar de favoritos"
        } else {
            "Agregar a favoritos"
        }))
        .on_click(cx.listener(move |_, _, _, cx| {
            data::set_favorite(&account_id, &product_id, !is_fav);
            // The sidebar and cover browser both have a FAVORITOS shelf;
            // without this they'd keep showing the pre-toggle grouping. It
            // also re-reads the star here, since this pane caches it.
            actions::bump_organisation(cx);
        }));

    hero.child(
        div()
            .absolute()
            .top(px(20.))
            .right(px(24.))
            .flex()
            .gap(px(10.))
            .child(hero_stat(format!("{unlocked}/{total}"), "LOGROS"))
            .child(hero_stat(fmt::duration(playtime.seconds_total), "JUGADO")),
    )
    .child(
        div()
            .relative()
            .w_full()
            .p(px(32.))
            .flex()
            .flex_col()
            .gap(px(6.))
            // In a row of its own: the column stretches its children, which
            // drew the badge as a bar across the whole hero.
            .child(div().flex().child(theme::badge("GOG", true)))
            // Beside the title rather than at the end of the action bar: it
            // says something about the game, not something to do with it,
            // and buried after DESINSTALAR is the last place anyone looks
            // for it.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .w_full()
                    .child(
                        theme::display(34.)
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(game.title.to_uppercase()),
                    )
                    .child(favorite),
            ),
    )
}

/// `.hero-stat`. The webview blurred what was behind it
/// (`backdrop-filter`), which GPUI can't; a more opaque fill keeps the
/// number readable over busy art instead.
fn hero_stat(value: String, label: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .bg(theme::alpha(theme::BG_DEEP, 0.85))
        .px_4()
        .py_2()
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(rgb(theme::LINE))
        .child(
            theme::mono()
                .text_size(px(19.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(value),
        )
        .child(
            div()
                .mt(px(2.))
                .text_size(px(10.))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label),
        )
}

impl DetailView {
    fn action_bar(&self, game: &Game, bar_state: BarState, window: &Window, cx: &Context<Self>) -> Div {
        let bar = div().pt(px(24.)).px(px(32.)).flex().items_center().gap(px(10.));
        let is_installed = game.install_state.as_ref().is_some_and(|state| state.installed);
        if !is_installed {
            return bar.children(self.install_action(game, bar_state, window, cx));
        }
        let properties_game = game.clone();
        bar.child(play_action(game, bar_state.launch, bar_state.running))
            .children(self.uninstall_action(game, cx))
            .child(
                button("properties", ButtonKind::Icon, "Propiedades", false)
                    .tooltip(tooltip("Propiedades"))
                    .on_click(move |_, _, cx| super::open_properties(&properties_game, cx)),
            )
    }

    /// Always safe to call — uninstalling something that isn't installed is
    /// a no-op — but it deletes real files, so it still gets a confirm step.
    fn uninstall_action(&self, game: &Game, cx: &Context<Self>) -> Vec<AnyElement> {
        let uninstall = &self.local.uninstall;
        if uninstall.busy {
            return vec![theme::muted("Desinstalando...").into_any_element()];
        }
        if !uninstall.confirming {
            return vec![
                button("uninstall", ButtonKind::Icon, "Desinstalar", false)
                    .tooltip(tooltip("Desinstalar"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.local.uninstall.confirming = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            ];
        }
        let account_id = game.account_id.clone();
        let product_id = game.id.clone();
        let mut elements = vec![
            button("uninstall-confirm", ButtonKind::Icon, "Confirmar desinstalacion", false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.uninstall(account_id.clone(), product_id.clone(), cx);
                }))
                .into_any_element(),
            button("uninstall-cancel", ButtonKind::Icon, "Cancelar", false)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.local.uninstall.confirming = false;
                    cx.notify();
                }))
                .into_any_element(),
        ];
        if let Some(err) = &uninstall.error {
            elements.push(theme::error(err.clone()).into_any_element());
        }
        elements
    }

    /// Not installed yet: one button, no questions. The `installing` map is
    /// both the progress display and the guard against a second install for
    /// the same game while one is in flight.
    fn install_action(&self, game: &Game, bar_state: BarState, window: &Window, cx: &Context<Self>) -> Vec<AnyElement> {
        let BarState {
            installable,
            status,
            dlcs,
            ..
        } = bar_state;
        // GOG carries superseded listings — an old edition someone owns
        // alongside the current one — with nothing behind them to install.
        // Pressing install on one only ever fails, so it isn't offered.
        if !installable {
            return vec![theme::muted("GOG no ofrece descarga para esta edicion").into_any_element()];
        }
        let account_id = game.account_id.clone();
        let product_id = game.id.clone();
        match status {
            Some(status @ (InstallStatus::Queued | InstallStatus::Installing { .. })) => {
                let current_file = match &status {
                    InstallStatus::Installing { current_file, .. } => Some(current_file.clone()),
                    InstallStatus::Queued | InstallStatus::Failed(_) => None,
                };
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .min_w(px(220.))
                        .children(progress_lines(&product_id, &status))
                        .children(current_file.map(|file| {
                            div()
                                .mt_1()
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_MUTED))
                                .truncate()
                                .child(file)
                        }))
                        .into_any_element(),
                    button("install-cancel", ButtonKind::Icon, "Cancelar", false)
                        .on_click(move |_, _, cx| actions::cancel_install(account_id.clone(), product_id.clone(), cx))
                        .into_any_element(),
                ]
            }
            Some(InstallStatus::Failed(err)) => vec![
                theme::error(format!("Instalacion fallida: {err}")).into_any_element(),
                button("install-dismiss", ButtonKind::Secondary, "Descartar", false)
                    .on_click(move |_, _, cx| actions::dismiss_install(&product_id, cx))
                    .into_any_element(),
            ],
            // Nothing to choose: the files go to the shared library and the
            // prefix is derived from (profile, game) when the game first runs.
            None => {
                let mut elements = vec![
                    button("install", ButtonKind::PlaySecondary, "INSTALAR", false)
                        .on_click(cx.listener({
                            let account_id = account_id.clone();
                            let product_id = product_id.clone();
                            move |this, _, _, cx| this.open_install_dialog(account_id.clone(), product_id.clone(), cx)
                        }))
                        .into_any_element(),
                ];
                if let Some(dialog) = &self.local.dialog {
                    let request = InstallRequest {
                        account_id,
                        product_id,
                        parent_id: None,
                        title: game.title.clone(),
                        branch: None,
                        dlcs,
                    };
                    elements.push(install_dialog(dialog, request, window, cx));
                }
                elements
            }
        }
    }

    /// The cache being empty is ambiguous on its own — still loading, this
    /// game genuinely has no achievements, or the poll failed. `poll_status`
    /// (the live poll that just ran, or is running, for this exact game)
    /// disambiguates it instead of always pointing at a CLI command.
    fn achievements_tab(
        &self,
        achievements: Option<&Result<Vec<CachedAchievement>, String>>,
        poll_status: &PollStatus,
        cx: &Context<Self>,
    ) -> AnyElement {
        let status_text = |text: String| div().mt(px(20.)).mx(px(32.)).child(theme::muted(text));
        let status_error = |text: String| div().mt(px(20.)).mx(px(32.)).child(theme::error(text));
        match achievements {
            Some(Ok(list)) if !list.is_empty() => return self.achievement_grid(list, cx),
            Some(Err(err)) => return status_error(format!("Error leyendo cache de logros: {err}")).into_any_element(),
            Some(Ok(_)) | None => {}
        }
        match poll_status {
            PollStatus::Polling => status_text("Buscando logros...".to_owned()),
            PollStatus::Done => div()
                .mt(px(20.))
                .mx(px(32.))
                .child(theme::empty("Este juego no tiene logros en GOG.")),
            PollStatus::Failed(err) => status_error(format!("No se pudo consultar logros: {err}")),
            PollStatus::Idle => status_text("Cargando...".to_owned()),
        }
        .into_any_element()
    }

    /// `auto-fill` has no GPUI equivalent, so the grid measures its own
    /// width each frame and derives the column count the way CSS would:
    /// as many 260px-minimum columns as fit, sharing what's left over.
    fn achievement_grid(&self, achievements: &[CachedAchievement], cx: &Context<Self>) -> AnyElement {
        let width = f32::from(self.grid_width);
        let columns = ((width + ACHV_GAP) / (ACHV_MIN_COLUMN + ACHV_GAP)).floor().max(1.) as u16;
        let this = cx.entity().downgrade();
        div()
            .relative()
            .mt(px(20.))
            .mx(px(32.))
            .child(measure_width(this))
            .child(
                div()
                    .grid()
                    .grid_cols(columns)
                    .gap(px(ACHV_GAP))
                    .children(achievements.iter().map(achievement_tile)),
            )
            .into_any_element()
    }
}

fn measure_width(this: WeakEntity<DetailView>) -> impl IntoElement {
    canvas(
        move |bounds, _, cx| {
            let width = bounds.size.width;
            let unchanged = this.read_with(cx, |this, _| this.grid_width == width).unwrap_or(true);
            if unchanged {
                return;
            }
            // Deferred because a notify during prepaint is dropped: the
            // window is mid-draw, so it would never schedule the re-layout
            // with the new column count.
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| {
                    this.grid_width = width;
                    cx.notify();
                });
            });
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn play_action(game: &Game, launch: Option<InstalledGame>, running: bool) -> AnyElement {
    let Some(installed) = launch else {
        return theme::muted("Instalado, pero sin ejecutable detectado").into_any_element();
    };
    let account_id = game.account_id.clone();
    let product_id = game.id.clone();
    if running {
        return button("stop", ButtonKind::PlaySecondary, "DETENER", false)
            .on_click(move |_, _, cx| {
                tracing::info!(account_id, product_id, "stopping game");
                actions::stop_game(account_id.clone(), product_id.clone(), cx);
            })
            .into_any_element();
    }
    button("play", ButtonKind::Play, "JUGAR", false)
        .on_click(move |_, _, cx| actions::launch_game(account_id.clone(), installed.clone(), cx))
        .into_any_element()
}

/// Confirms an install before it starts, the way Steam does — where the
/// files go, and which branch to install (mainline by default, same as
/// Steam defaults to no beta opt-in). The library location shown here is
/// whatever this account's profile already resolved to (its own chosen
/// location, or the shared default) — informational, since changing it is
/// a per-profile setting, not something to pick mid-install.
fn install_dialog(
    dialog: &InstallDialog,
    request: InstallRequest,
    window: &Window,
    cx: &Context<DetailView>,
) -> AnyElement {
    let this = cx.entity().downgrade();
    let close: menu::Dismiss = Rc::new({
        let this = this.clone();
        move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.local.dialog = None;
                cx.notify();
            });
        }
    });
    let viewport = window.viewport_size();
    let dlc_count = request.dlcs.len();
    let title = request.title.clone();

    let rows = install_rows(dialog, dlc_count);
    let branch = branch_picker(dialog, this);
    let field_row = || div().flex().gap(px(10.)).mb_3().max_w(px(520.));

    let chosen = dialog.chosen.clone();
    // A click handler is `Fn`, but the request is spent once: the dialog
    // closes on the same click, so a second one never reaches it.
    let request = std::cell::Cell::new(Some(request));
    let confirm = cx.listener(move |this, _, _, cx| {
        this.local.dialog = None;
        cx.notify();
        if let Some(mut request) = request.take() {
            request.branch.clone_from(&chosen);
            actions::start_install(request, cx);
        }
    });
    let cancel = Rc::clone(&close);
    let dismiss = Rc::clone(&close);

    let panel = div()
        .w(px(640.).min(viewport.width * 0.92))
        .max_h(viewport.height * 0.82)
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL_2))
        .border_1()
        .border_color(rgb(theme::LINE))
        .rounded(px(6.))
        .shadow(vec![BoxShadow {
            color: gpui::black().opacity(0.5),
            offset: point(px(0.), px(24.)),
            blur_radius: px(64.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_6()
                .py(px(18.))
                .border_b_1()
                .border_color(rgb(theme::LINE))
                .child(
                    theme::display(16.)
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(format!("Instalar — {title}").to_uppercase()),
                )
                .child(
                    button("install-dialog-close", ButtonKind::Icon, "✕", false)
                        .on_click(move |_, window, cx| close(window, cx)),
                ),
        )
        .child(
            div()
                .id("install-dialog-body")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .pt(px(20.))
                .px_6()
                .pb_6()
                .child(rows)
                .child(field_row().child(div().child("Rama")).child(branch))
                .child(
                    field_row()
                        .child(button("install-confirm", ButtonKind::Play, "INSTALAR", false).on_click(confirm))
                        .child(
                            button("install-dialog-cancel", ButtonKind::Secondary, "Cancelar", false)
                                .on_click(move |_, window, cx| cancel(window, cx)),
                        ),
                ),
        );
    menu::modal(panel, window, dismiss)
}

/// `.properties-rows`: where the files go and which DLC rides along.
fn install_rows(dialog: &InstallDialog, dlc_count: usize) -> Div {
    let row = |label: &'static str, value: String| {
        div()
            .flex()
            .justify_between()
            .gap_4()
            .text_size(px(13.))
            .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label))
            .child(div().child(value))
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .mb_4()
        .children(
            dialog
                .library_path
                .as_ref()
                .map(|path| row("Ubicación", path.display().to_string())),
        )
        .when(dlc_count > 0, |rows| {
            rows.child(row(
                "Contenido adicional",
                format!("{dlc_count} marcado(s), se instala junto con el juego"),
            ))
        })
}

fn branch_picker(dialog: &InstallDialog, this: WeakEntity<DetailView>) -> AnyElement {
    match &dialog.branches {
        None => theme::muted("Cargando...").into_any_element(),
        Some(Err(err)) => theme::error(err.clone()).into_any_element(),
        Some(Ok(list)) => {
            let options = std::iter::once((SharedString::default(), SharedString::from("Mainline (por defecto)")))
                .chain(
                    list.iter()
                        .filter_map(|branch| branch.name.as_deref())
                        .map(|name| (SharedString::from(name.to_owned()), SharedString::from(name.to_owned()))),
                );
            Select::new("install-branch", options, dialog.chosen.clone().unwrap_or_default())
                .on_change(move |value, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(dialog) = &mut this.local.dialog {
                            dialog.chosen = (!value.is_empty()).then(|| value.to_string());
                            cx.notify();
                        }
                    });
                })
                .into_any_element()
        }
    }
}

/// Which shelves this game sits on. Read-only here — membership is edited
/// from the right-click menu in the list, where you can see the shelf you
/// are putting it on.
fn collection_chips(names: Vec<String>) -> Option<Div> {
    // Favourite isn't a shelf: the star on the hero says it, and a chip
    // here would only repeat it. An empty row renders nothing.
    if names.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .pb_4()
            .children(names.into_iter().map(|name| {
                div()
                    .bg(rgb(theme::BG_PANEL))
                    .border_1()
                    .border_color(rgb(theme::LINE))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .rounded_full()
                    .px(px(9.))
                    .py(px(3.))
                    .text_size(px(11.))
                    .child(name)
            })),
    )
}

fn summary_tab(game: &Game, playtime: Playtime) -> Div {
    let stats = div()
        .flex()
        .flex_wrap()
        .gap(px(14.))
        .mt(px(20.))
        .mx(px(32.))
        .child(stat_card(fmt::duration(playtime.seconds_total), "Tiempo jugado"))
        .children(
            playtime
                .last_played_at
                .and_then(fmt::unix_date)
                .map(|last| stat_card(last, "Ultima sesion")),
        )
        .children(game.rating.clone().map(|rating| stat_card(rating, "Calificacion")))
        .children(
            game.release_date
                .and_then(fmt::unix_date)
                .map(|date| stat_card(date, "Lanzamiento")),
        );

    // HTML collapsed the two spaces the webview put after the developer
    // into one, so that's what is shown between the two credits.
    let credits = match (&game.developer, &game.publisher) {
        (None, None) => None,
        (Some(dev), None) => Some(format!("Desarrollador: {dev}")),
        (None, Some(publisher)) => Some(format!("Editor: {publisher}")),
        (Some(dev), Some(publisher)) => Some(format!("Desarrollador: {dev} Editor: {publisher}")),
    };
    let genres = (!game.genres.is_empty()).then(|| format!("Generos: {}", game.genres.join(", ")));
    let paragraph = |text: String| {
        div()
            .mb(px(10.))
            .line_height(relative(1.6))
            .text_color(rgb(theme::TEXT_MUTED))
            .child(text)
    };
    let text = div()
        .mt(px(20.))
        .mx(px(32.))
        .mb(px(40.))
        .max_w(px(720.))
        .children(game.description.clone().map(paragraph))
        .children(credits.map(paragraph))
        .children(genres.map(paragraph));

    div().flex().flex_col().child(stats).child(text)
}

fn achievement_tile(achievement: &CachedAchievement) -> Div {
    let unlocked = achievement.unlocked;
    let icon = div()
        .size(px(40.))
        .flex_none()
        .rounded(px(theme::RADIUS_SM))
        .bg(rgb(theme::BG_PANEL_LIGHT))
        .overflow_hidden()
        .children(icon_for(achievement).map(|url| {
            img(crate::art::source(url, gpui::size(40., 40.)))
                .size_full()
                .rounded(px(theme::RADIUS_SM))
                .object_fit(ObjectFit::Cover)
                .grayscale(!unlocked)
        }));
    let info = div()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(12.5))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(achievement.name.clone()),
        )
        .children(achievement.description.clone().map(|desc| {
            div()
                .mt(px(2.))
                .text_size(px(11.))
                .text_color(rgb(theme::TEXT_MUTED))
                .truncate()
                .child(desc)
        }))
        .children(achievement.unlocked_at.clone().map(|at| {
            theme::mono()
                .mt_1()
                .text_size(px(10.5))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(at)
        }));
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_w_0()
        .bg(rgb(theme::BG_PANEL_2))
        .border_1()
        .border_color(if unlocked {
            theme::alpha(theme::ACCENT_GOLD, 0.4)
        } else {
            rgb(theme::LINE).into()
        })
        .rounded(px(theme::RADIUS))
        .px_3()
        .py(px(10.))
        .when(!unlocked, |tile| tile.opacity(0.5))
        .child(icon)
        .child(info)
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
