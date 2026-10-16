//! The right-hand pane: hero art with stat pills (achievements, completion,
//! playtime — Steam's playtime pill, adapted, but self-tracked: nothing
//! upstream has a notion of a play session), a favorite toggle, a real
//! Play/Stop action wired to whichever prefix this game was installed
//! into, and two tabs — Resumen (catalog metadata already on `Game` but
//! never shown before) and Logros.

mod achievements;
mod action_bar;
mod hero;
mod install_dialog;
mod summary;

use decanter_core::{Game, InstallState, InstalledGame, Playtime, Storefront};
use decanter_gog::LibraryEntry;
use gpui::{Context, Div, Entity, Pixels, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::{LibraryModel, actions, data};
use crate::state::{AppState, Background, InstallStatus};
use crate::theme;
use crate::views::ps::log::LogView;
use crate::widgets::tabs;
use hero::hero;
use install_dialog::InstallDialog;
use summary::summary_tab;

#[derive(Clone, Copy, PartialEq, Default)]
enum DetailTab {
    #[default]
    Summary,
    Achievements,
    Log,
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
            is_favorite: data::is_favorite(&game.account_id, game.storefront, &game.id),
            playtime: data::load_playtime(&game.account_id, game.storefront, &game.id),
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
            self.is_favorite = data::is_favorite(&game.account_id, game.storefront, &game.id);
            self.collections = member_of(profile_id, &game.id);
        }
        if self.playtime_generation != background.playtime_generation {
            self.playtime_generation = background.playtime_generation;
            self.playtime = data::load_playtime(&game.account_id, game.storefront, &game.id);
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

/// UI state that belongs to one game and starts over when the selection
/// moves to another, the way the webview's per-game components did.
#[derive(Default)]
struct Local {
    tab: DetailTab,
    uninstall: Uninstall,
    dialog: Option<InstallDialog>,
    /// Follows the game's latest run, which a new one replaces.
    log: Option<Entity<LogView>>,
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

    fn sync_log(&mut self, game: &Game, cx: &mut Context<Self>) {
        let run = AppState::get(cx).background.read(cx).runs.get(&game.id).cloned();
        match (run, &self.local.log) {
            (Some(job), Some(log)) if log.read(cx).job() == &job => {}
            (Some(job), _) => self.local.log = Some(cx.new(|cx| LogView::new(job, cx))),
            (None, _) => self.local.log = None,
        }
    }

    /// JUGAR shows the run as it starts, the way the PS section does.
    pub(super) fn show_log(&mut self, cx: &mut Context<Self>) {
        self.local.tab = DetailTab::Log;
        cx.notify();
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
        let is_installed = game.install_state.as_ref().is_some_and(|install| install.installed);
        self.sync_log(&game, cx);

        let (achievements, poll_status) = {
            let model = self.model.read(cx);
            (model.achievements.clone(), model.poll_status.clone())
        };
        let (unlocked, total) = match &achievements {
            Some(Ok(list)) => (list.iter().filter(|a| a.cached.unlocked).count(), list.len()),
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

        let external = game.storefront == Storefront::Unspecified;
        let hidden = (external && self.local.tab == DetailTab::Achievements)
            || (!is_installed && self.local.tab == DetailTab::Log);
        let active = if hidden { DetailTab::Summary } else { self.local.tab };
        let mut detail_tabs = vec![(DetailTab::Summary, t!("library.detail.tabs.summary"))];
        if !external {
            detail_tabs.push((DetailTab::Achievements, t!("library.detail.tabs.achievements")));
        }
        if is_installed {
            detail_tabs.push((DetailTab::Log, t!("library.detail.tabs.log")));
        }
        let tab_row = tabs("detail-tabs", &detail_tabs, active, {
            let this = cx.entity().downgrade();
            move |tab, _, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.local.tab = tab;
                    cx.notify();
                });
            }
        });
        let body = match active {
            DetailTab::Summary => summary_tab(&game, playtime).into_any_element(),
            DetailTab::Achievements => self.achievements_tab(achievements.as_ref(), &poll_status, cx),
            DetailTab::Log => {
                let log = div().mt(px(20.)).mx(px(32.));
                match &self.local.log {
                    Some(view) => log.h(px(460.)).child(view.clone()).into_any_element(),
                    None => log.child(theme::muted(t!("library.detail.no_runs"))).into_any_element(),
                }
            }
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
