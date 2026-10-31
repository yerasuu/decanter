//! The selected game: what it is, the play button with what stands in its
//! way, and its tabs.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use decanter_core::capabilities::Feature;
use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::per_game::Category;
use decanter_ps::bbport::{self, Bbport, IniScope};
use decanter_ps::failures::Log;
use decanter_ps::layouts;
use decanter_ps::learned::{self, FrameOverlap};
use decanter_ps::mapping::Target;
use decanter_ps::port::Port;
use decanter_ps::status::{Entry, Status};
use gpui::{AnyElement, Context, Entity, ObjectFit, SharedString, Window, div, img, prelude::*, px, rgb};
use rust_i18n::t;

use super::gamepad::GamepadView;
use super::graphics::GraphicsView;
use super::imports::ImportsView;
use super::keyboard::{Controls, KeyboardView};
use super::mods::ModsView;
use super::online::OnlineTitleView;
use super::runs::RunsView;
use super::title_settings::{Changed, TitleSettingsView};
use super::{art_url, display_title, platform_label, status_text};
use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::state::{JobKey, Ps};
use crate::theme;
use crate::views::game_tools::GameToolsPanel;
use crate::views::ps::log::LogView;
use crate::views::ps::report::{Closed, ReportView, primary_failure};
use crate::widgets::{ButtonKind, button, hero_stat, tooltip};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Keyboard,
    Gamepad,
    Graphics,
    Mods,
    Online,
    Performance,
    Imports,
    Log,
}

impl Tab {
    fn label(self) -> Cow<'static, str> {
        match self {
            Self::Overview => t!("ps.detail.tabs.overview"),
            Self::Keyboard => t!("ps.detail.tabs.keyboard"),
            Self::Gamepad => t!("ps.detail.tabs.gamepad"),
            Self::Graphics => t!("ps.detail.tabs.graphics"),
            Self::Mods => t!("ps.detail.tabs.mods"),
            Self::Online => t!("ps.detail.tabs.online"),
            Self::Performance => t!("ps.detail.tabs.performance"),
            Self::Imports => t!("ps.detail.tabs.imports"),
            Self::Log => t!("ps.detail.tabs.log"),
        }
    }
}

const TABS: [Tab; 3] = [Tab::Overview, Tab::Keyboard, Tab::Log];

/// bbport has settings and gamepad layouts of its own; AnyPS5 reads the
/// controller as it is and takes no settings.
const BBPORT_TABS: [Tab; 9] = [
    Tab::Overview,
    Tab::Keyboard,
    Tab::Gamepad,
    Tab::Graphics,
    Tab::Mods,
    Tab::Online,
    Tab::Performance,
    Tab::Imports,
    Tab::Log,
];

/// Another PS4 title on bbport: its mods are Bloodborne's.
const OTHER_PS4_TABS: [Tab; 8] = [
    Tab::Overview,
    Tab::Keyboard,
    Tab::Gamepad,
    Tab::Graphics,
    Tab::Online,
    Tab::Performance,
    Tab::Imports,
    Tab::Log,
];

/// A game no port runs has nothing to bind.
const UNSUPPORTED_TABS: [Tab; 1] = [Tab::Overview];

pub struct DetailView {
    ps: Entity<Ps>,
    /// By folder: the list is rebuilt on every scan, and a folder is what
    /// stays the same game across one.
    selected: Option<PathBuf>,
    tab: Tab,
    log: Option<Entity<LogView>>,
    /// The selected game's measured runs, made when its Registro first shows.
    runs: Option<Entity<RunsView>>,
    /// What the selected game's last run lacked, made when its tab first shows.
    imports: Option<Entity<ImportsView>>,
    /// The bindings of `selected`, read when its Controles tab opens.
    controls: Option<Result<Entity<KeyboardView>, Text>>,
    /// Kept across selections and told which title is selected, whose settings it shows.
    gamepad: Option<Entity<GamepadView>>,
    /// Kept across selections and told which title is selected, whose settings and effects it
    /// shows.
    graphics: Option<Entity<GraphicsView>>,
    mods: Option<Entity<ModsView>>,
    online: Option<Entity<OnlineTitleView>>,
    /// By game folder, whether its eboot names an NP online library. A game missing from it is
    /// still being scanned (or could not be read) and shows no Online tab.
    online_support: HashMap<PathBuf, bool>,
    /// What earlier runs of `selected` taught, and the revision it was read at.
    learned: Option<FrameOverlap>,
    learned_read_at: Option<u32>,
    learned_error: Option<Text>,
    /// `selected`'s own tool choices, made again for each game picked.
    tools: Option<Entity<GameToolsPanel>>,
    /// Copy, export, import and reset of `selected`'s settings, for a bbport title.
    title_settings: Option<Entity<TitleSettingsView>>,
    /// The report of `selected`'s last failed run, opened from the banner.
    report: Option<Entity<ReportView>>,
    report_error: Option<Text>,
}

impl DetailView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| {
            this.sync_log(cx);
            this.sync_learned(cx);
            cx.notify();
        })
        .detach();
        Self {
            ps,
            selected: None,
            tab: Tab::Overview,
            log: None,
            runs: None,
            imports: None,
            controls: None,
            gamepad: None,
            graphics: None,
            mods: None,
            online: None,
            online_support: HashMap::new(),
            learned: None,
            learned_read_at: None,
            learned_error: None,
            tools: None,
            title_settings: None,
            report: None,
            report_error: None,
        }
    }

    pub const fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    /// Back to the covers.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.report = None;
        self.report_error = None;
        self.controls = None;
        self.log = None;
        self.runs = None;
        self.imports = None;
        cx.notify();
    }

    pub fn select(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let (key, bbport_title) = self
            .ps
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.game.dir == dir)
            .map(|entry| {
                let title = (entry.port == Some(Port::Bbport)).then(|| entry.game.title_id.clone());
                (Some(entry.game.key()), title)
            })
            .unwrap_or_default();
        self.tools = key.map(|key| cx.new(|_| GameToolsPanel::new(key)));
        self.title_settings = bbport_title.map(|title_id| {
            let ps = self.ps.clone();
            let view = cx.new(|cx| TitleSettingsView::new(ps, title_id, cx));
            // The bindings are read once per selection, and the tab in sight before it is shown
            // again; a replacement must show in both.
            cx.subscribe(&view, |this, _, _: &Changed, cx| {
                this.controls = None;
                let entry = this.selected_port(cx);
                this.show_tab(this.tab, entry, cx);
            })
            .detach();
            view
        });
        self.selected = Some(dir.clone());
        self.sync_online_support(dir, cx);
        self.report = None;
        self.report_error = None;
        self.tab = Tab::Overview;
        self.controls = None;
        self.runs = None;
        self.imports = None;
        self.learned = None;
        self.learned_read_at = None;
        self.learned_error = None;
        self.sync_log(cx);
        self.sync_learned(cx);
        cx.notify();
    }

    fn sync_online_support(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let entry = self
            .ps
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.game.dir == dir && entry.port == Some(Port::Bbport))
            .map(|entry| entry.game.title_id.clone());
        let Some(title_id) = entry else {
            return;
        };
        if let Some(&is_online) = self.online_support.get(&dir) {
            self.show_online_support(is_online, cx);
            return;
        }
        self.show_online_support(false, cx);
        let scanned = dir.clone();
        let task = cx.background_spawn(async move {
            decanter_ps::online_support::is_online(&decanter_ps::paths::active(), &scanned, &title_id)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let is_online = result.unwrap_or_else(|err| {
                    tracing::warn!(path = %dir.join("eboot.bin").display(), %err, "online support unknown");
                    false
                });
                this.online_support.insert(dir.clone(), is_online);
                if this.selected.as_ref() == Some(&dir) {
                    this.show_online_support(is_online, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_online_support(&self, is_online: bool, cx: &mut Context<Self>) {
        if let Some(settings) = &self.title_settings {
            settings.update(cx, |settings, cx| settings.set_has_online(is_online, cx));
        }
    }

    /// Reads the profile's failures off the UI thread and opens the report of the last run's.
    fn open_report(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(path) = self.ps.read(cx).failures_path() else {
            return;
        };
        self.report_error = None;
        let task = cx.background_spawn(async move { Log::load(path).records().to_vec() });
        cx.spawn(async move |this, cx| {
            let records = task.await;
            let _ = this.update(cx, |this, cx| {
                match primary_failure(&records, &dir) {
                    Some(failure) => {
                        let panel = cx.new(|cx| ReportView::new(failure, cx));
                        cx.subscribe(&panel, |this, _, _: &Closed, cx| {
                            this.report = None;
                            cx.notify();
                        })
                        .detach();
                        this.report = Some(panel);
                    }
                    None => this.report_error = Some(Text::new("ps.report.errors.no_failure")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn selected_port(&self, cx: &Context<Self>) -> Option<(Port, String)> {
        let selected = self.selected.as_ref()?;
        self.ps
            .read(cx)
            .entries()
            .iter()
            .find(|entry| &entry.game.dir == selected)
            .and_then(|entry| entry.port.map(|port| (port, entry.game.title_id.clone())))
    }

    fn select_tab(&mut self, tab: Tab, entry: Option<(Port, String)>, cx: &mut Context<Self>) {
        if let Some(settings) = &self.title_settings {
            settings.update(cx, TitleSettingsView::close_copy);
        }
        self.show_tab(tab, entry, cx);
    }

    /// A tab's entry to copy its settings from another game, or the copy row it opened.
    fn copy_entry(&self, tab: Tab, cx: &Context<Self>) -> Option<AnyElement> {
        let settings = self.title_settings.clone()?;
        let entries: &[(Category, &str)] = match tab {
            Tab::Graphics => &[
                (Category::Graphics, "ps.per_game.copy_entry"),
                (Category::Extras, "ps.per_game.copy_entry_extras"),
            ],
            Tab::Keyboard => &[(Category::Keyboard, "ps.per_game.copy_entry")],
            Tab::Gamepad => &[(Category::Gamepad, "ps.per_game.copy_entry")],
            Tab::Mods => &[(Category::Mods, "ps.per_game.copy_entry")],
            Tab::Online => &[(Category::Online, "ps.per_game.copy_entry")],
            Tab::Overview | Tab::Performance | Tab::Imports | Tab::Log => return None,
        };
        if settings.read(cx).is_copy_only() {
            return Some(div().mb_4().child(settings).into_any_element());
        }
        let buttons = entries.iter().map(|&(category, label)| {
            let settings = settings.clone();
            button(
                SharedString::from(format!("ps-copy-entry-{}", category.id())),
                ButtonKind::Secondary,
                t!(label),
                false,
            )
            .on_click(move |_, _, cx| settings.update(cx, |settings, cx| settings.open_copy(category, cx)))
        });
        Some(div().flex().gap_2().mb_4().children(buttons).into_any_element())
    }

    fn show_tab(&mut self, tab: Tab, entry: Option<(Port, String)>, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Gamepad && self.gamepad.is_none() {
            let ps = self.ps.clone();
            self.gamepad = Some(cx.new(|cx| GamepadView::new(&ps, cx)));
        }
        if tab == Tab::Gamepad
            && let (Some(gamepad), Some((Port::Bbport, title_id))) = (&self.gamepad, &entry)
        {
            gamepad.update(cx, |gamepad, cx| gamepad.show_title(title_id, cx));
        }
        if tab == Tab::Graphics && self.graphics.is_none() {
            let ps = self.ps.clone();
            self.graphics = Some(cx.new(|cx| GraphicsView::new(&ps, cx)));
        }
        if tab == Tab::Graphics
            && let (Some(graphics), Some((Port::Bbport, title_id))) = (&self.graphics, &entry)
        {
            graphics.update(cx, |graphics, cx| graphics.show_title(title_id, cx));
        }
        if tab == Tab::Mods
            && let Some((Port::Bbport, title_id)) = &entry
        {
            let ps = self.ps.clone();
            self.mods
                .get_or_insert_with(|| cx.new(|_| ModsView::new(ps)))
                .update(cx, |mods, cx| mods.show_title(title_id, cx));
        }
        if tab == Tab::Online
            && let Some((Port::Bbport, title_id)) = &entry
        {
            let ps = self.ps.clone();
            self.online
                .get_or_insert_with(|| cx.new(|cx| OnlineTitleView::new(ps, cx)))
                .update(cx, |online, cx| online.show_title(title_id, cx));
        }
        if tab == Tab::Keyboard
            && self.controls.is_none()
            && let Some((port, title_id)) = entry
        {
            self.load_controls(port, title_id, cx);
        }
        cx.notify();
    }

    fn load_controls(&mut self, port: Port, title_id: String, cx: &mut Context<Self>) {
        let target = match port {
            Port::Bbport => Target::Bbport,
            Port::AnyPs5 => Target::AnyPs5,
        };
        let selected = self.selected.clone();
        let task = cx.background_spawn(async move {
            let root = decanter_ps::paths::active();
            let (path, scope) = match port {
                Port::Bbport => match Bbport::title_ini(&root, &title_id) {
                    Ok(port_ini) => (port_ini.path, port_ini.scope),
                    Err(err) => return Err(Text::new("ps.scope.read_error").arg("error", err)),
                },
                Port::AnyPs5 => (AnyPs5::binds_path(&root, &title_id), IniScope::Title),
            };
            let store = decanter_ps::layouts::Store::load()
                .map_err(|err| Text::new("ps.layouts.read_error").arg("error", err))?;
            let recorded = layouts::choice::recorded(&root, layouts::Kind::Keys(target), &title_id)
                .map_err(|err| Text::new("ps.scope.read_error").arg("error", err))?;
            let mapping = decanter_ps::binds::read(target, &path).map_err(Text::verbatim)?;
            Ok(Controls {
                title_id,
                path,
                scope,
                mapping,
                recorded,
                store,
            })
        });
        cx.spawn(async move |this, cx| {
            let mapping = task.await;
            let _ = this.update(cx, |this, cx| {
                // Another game picked meanwhile: its own tab reads its own file.
                if this.selected != selected {
                    return;
                }
                let ps = this.ps.clone();
                this.controls = Some(mapping.map(|controls| cx.new(|cx| KeyboardView::new(ps, target, controls, cx))));
                cx.notify();
            });
        })
        .detach();
    }

    /// Follows the selected game's latest job, which a new run replaces.
    fn sync_log(&mut self, cx: &mut Context<Self>) {
        let job = self
            .selected
            .clone()
            .and_then(|dir| self.ps.read(cx).job(&JobKey::Game(dir)).cloned());
        match (job, &self.log) {
            (Some(job), Some(log)) if log.read(cx).job() == &job => {}
            (Some(job), _) => self.log = Some(cx.new(|cx| LogView::new(job, cx))),
            (None, _) => self.log = None,
        }
    }

    /// Reads the selected title's record only when it changed: the section notifies far more
    /// often than a run teaches something.
    fn sync_learned(&mut self, cx: &mut Context<Self>) {
        let ps = self.ps.read(cx);
        let revision = ps.learned_revision();
        if self.learned_read_at == Some(revision) {
            return;
        }
        let Some(title_id) = self
            .selected
            .as_ref()
            .and_then(|dir| ps.entries().iter().find(|entry| &entry.game.dir == dir))
            .map(|entry| entry.game.title_id.clone())
        else {
            return;
        };
        self.learned_read_at = Some(revision);
        let selected = self.selected.clone();
        let task = cx.background_spawn(async move {
            learned::Store::load(&decanter_ps::paths::active())
                .title(&title_id)
                .frame_overlap_off
        });
        cx.spawn(async move |this, cx| {
            let learned = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.selected != selected {
                    return;
                }
                this.learned = learned;
                cx.notify();
            });
        })
        .detach();
    }

    /// Drops the record, so the next run takes the port's default again.
    fn forget_frame_overlap(&mut self, title_id: String, cx: &mut Context<Self>) {
        self.learned = None;
        let task = cx.background_spawn(async move {
            learned::Store::forget_frame_overlap(&decanter_ps::paths::active(), &title_id)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.ps.update(cx, |ps, cx| ps.learned_changed(cx)),
                Err(err) => {
                    this.learned_error = Some(Text::new("ps.learned.forget_error").arg("error", err));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Biblioteca's hero: the dump's own wide art (`pic1.png`) under a
    /// scrim, the platform badge and the title large, a couple of figures.
    fn hero(entry: &Entry) -> AnyElement {
        let game = &entry.game;
        let fallback = || {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(gpui::linear_gradient(
                    150.,
                    gpui::linear_color_stop(rgb(theme::BG_PANEL_LIGHT), 0.),
                    gpui::linear_color_stop(rgb(theme::BG_PANEL_3), 1.),
                ))
                .into_any_element()
        };
        div()
            .relative()
            .min_h(px(300.))
            .overflow_hidden()
            .flex()
            .items_end()
            .bg(rgb(theme::BG_PANEL_2))
            .child(
                img(crate::art::source(&art_url(game, "pic1.png"), gpui::size(1600., 400.)))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .with_fallback(fallback),
            )
            .child(div().absolute().top_0().left_0().size_full().bg(gpui::linear_gradient(
                180.,
                gpui::linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.35),
                gpui::linear_color_stop(theme::alpha(theme::BG_DEEP, 0.95), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .top(px(20.))
                    .right(px(24.))
                    .flex()
                    .gap(px(10.))
                    .child(hero_stat(game.version.clone(), t!("ps.detail.hero.version")))
                    .child(hero_stat(
                        status_text(&entry.status).into_owned(),
                        t!("ps.detail.hero.status"),
                    )),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .p(px(32.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(theme::badge(platform_label(game.platform), true))
                            .child(theme::badge(&game.title_id, false)),
                    )
                    .child(
                        theme::display(34.)
                            .min_w_0()
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(display_title(game).to_uppercase()),
                    ),
            )
            .into_any_element()
    }

    /// JUGAR or DETENER first, then what else there is to do with the game.
    fn action_bar(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let game = &entry.game;
        let key = JobKey::Game(game.dir.clone());
        let ps = self.ps.read(cx);
        let running = ps.is_running(&key, cx) || ps.is_launching(&game.dir);
        let preparing = entry
            .port
            .and_then(|port| ps.port(port))
            .is_some_and(|state| state.is_busy());
        let playable = entry.status.playable();
        let action = if running && ps.is_stopping(&game.dir) {
            button("ps-stop", ButtonKind::PlaySecondary, t!("ps.detail.stopping"), true).into_any_element()
        } else if running {
            button("ps-stop", ButtonKind::PlaySecondary, t!("ps.detail.stop"), false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    this.ps.update(cx, |ps, cx| ps.end(&key, cx));
                }))
                .into_any_element()
        } else {
            let label = if preparing {
                t!("ps.detail.preparing")
            } else {
                t!("ps.detail.play")
            };
            let disabled = !playable || preparing;
            let dir = game.dir.clone();
            button("ps-play", ButtonKind::Play, label, disabled)
                .when(!disabled, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = Tab::Log;
                        this.ps.update(cx, |ps, cx| ps.play(&dir, cx));
                    }))
                })
                .into_any_element()
        };
        let folder = game.dir.clone();
        div()
            .pt(px(24.))
            .px(px(32.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(action)
            .child(
                button("ps-open-folder", ButtonKind::Icon, t!("ps.detail.open_folder"), false)
                    .tooltip(tooltip(t!("ps.detail.open_folder_tooltip")))
                    .on_click(move |_, _, _| {
                        if let Err(err) = crate::desktop::open(&folder) {
                            tracing::warn!(dir = %folder.display(), %err, "could not open the game folder");
                        }
                    }),
            )
            .into_any_element()
    }

    fn overview(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let game = &entry.game;
        let fact = |label: Cow<'static, str>, value: String| {
            div()
                .flex()
                .gap_4()
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(SharedString::from(label)),
                )
                .child(theme::mono().min_w_0().truncate().child(value))
        };
        let explanation = match &entry.status {
            Status::Ready | Status::Experimental => None,
            Status::NeedsConvert => Some(t!("ps.detail.explain.needs_convert")),
            Status::NeedsMerge(_) => Some(t!("ps.detail.explain.needs_merge")),
            Status::HasPlayable(dir) => Some(t!("ps.detail.explain.has_playable", dir = dir.display())),
            Status::OtherEdition => Some(t!("ps.detail.explain.other_edition")),
            Status::NeedsUpdate => Some(t!("ps.detail.explain.needs_update")),
            Status::NeedsVr => Some(t!("ps.detail.explain.needs_vr")),
            Status::Encrypted => Some(t!("ps.detail.explain.encrypted")),
            Status::NoEboot => Some(t!("ps.detail.explain.no_eboot")),
        };
        let merge = match &entry.status {
            Status::NeedsMerge(update) => {
                let (base, update) = (game.dir.clone(), update.clone());
                Some(
                    button("ps-merge", ButtonKind::Primary, t!("ps.detail.merge"), false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            let (base, update) = (base.clone(), update.clone());
                            this.ps.update(cx, |ps, cx| ps.merge(base, update, cx));
                        },
                    )),
                )
            }
            _ => None,
        };
        let library = self.ps.read(cx).library_of(&game.dir).map(|(root, primary)| {
            if primary {
                t!("storage.folder.primary").into_owned()
            } else {
                format!("{} · {}", t!("storage.folder.extra"), root.display())
            }
        });
        let learned = self.learned.as_ref().map(|record| {
            let title_id = game.title_id.clone();
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(theme::muted(t!(
                    "ps.learned.frame_overlap_off",
                    date = decanter_ps::failures::format_time(record.time)
                )))
                .child(
                    button(
                        "ps-learned-forget",
                        ButtonKind::Secondary,
                        t!("ps.learned.forget"),
                        false,
                    )
                    .tooltip(tooltip(t!("ps.learned.forget_tooltip")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.forget_frame_overlap(title_id.clone(), cx);
                    })),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(explanation.map(theme::muted))
            .children(merge.map(|merge| div().flex().child(merge)))
            .children(learned)
            .children(self.learned_error.as_ref().map(theme::error))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(fact(t!("ps.detail.facts.title_id"), game.title_id.clone()))
                    .child(fact(t!("ps.detail.facts.version"), game.version.clone()))
                    .children(library.map(|library| fact(t!("ps.detail.facts.library"), library)))
                    .child(fact(t!("ps.detail.facts.folder"), game.dir.display().to_string())),
            )
            .children(entry.port.and(self.tools.clone()))
            .children(self.title_settings.clone())
            .into_any_element()
    }
}

impl Render for DetailView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ps = self.ps.read(cx);
        let notice = ps.notice().cloned();
        let entry = self
            .selected
            .as_ref()
            .and_then(|selected| ps.entries().iter().find(|entry| &entry.game.dir == selected))
            .map(|entry| Entry {
                game: entry.game.clone(),
                port: entry.port,
                status: entry.status.clone(),
            });
        let run_notice = entry
            .as_ref()
            .and_then(|entry| ps.run_notice(&entry.game.dir))
            .map(|(text, error)| (text.clone(), *error));
        let Some(entry) = entry else {
            return div()
                .p_8()
                .child(theme::muted(t!("ps.detail.pick_game")))
                .into_any_element();
        };
        let entity = cx.entity();
        let (report_entity, report_dir) = (cx.entity(), entry.game.dir.clone());
        let port = entry.port.map(|port| (port, entry.game.title_id.clone()));
        let shown: &[Tab] = match entry.port {
            Some(Port::Bbport) if bbport::is_bloodborne(&entry.game) => &BBPORT_TABS,
            Some(Port::Bbport) => &OTHER_PS4_TABS,
            Some(Port::AnyPs5) => &TABS,
            None => &UNSUPPORTED_TABS,
        };
        let has_online = !Caps::get(cx).is_never_here(Feature::PsOnline)
            && self.online_support.get(&entry.game.dir).copied().unwrap_or(false);
        let items: Vec<_> = shown
            .iter()
            .filter(|&&tab| tab != Tab::Online || has_online)
            .map(|&tab| (tab, tab.label()))
            .collect();
        let tabs = crate::widgets::tabs("ps-detail-tabs", &items, self.tab, move |tab, _, cx| {
            let port = port.clone();
            entity.update(cx, |this, cx| this.select_tab(tab, port, cx));
        });
        // Only psport measures frames; the view is made here since JUGAR opens Registro too.
        if matches!(self.tab, Tab::Performance | Tab::Log) && entry.port == Some(Port::Bbport) && self.runs.is_none() {
            let (ps, dir, title_id) = (self.ps.clone(), entry.game.dir.clone(), entry.game.title_id.clone());
            self.runs = Some(cx.new(|cx| RunsView::new(ps, dir, title_id, cx)));
        }
        if self.tab == Tab::Imports && entry.port == Some(Port::Bbport) && self.imports.is_none() {
            let (ps, dir, title_id) = (self.ps.clone(), entry.game.dir.clone(), entry.game.title_id.clone());
            self.imports = Some(cx.new(|cx| ImportsView::new(ps, dir, title_id, cx)));
        }
        let copy_entry = self.copy_entry(self.tab, cx);
        let body = match self.tab {
            Tab::Performance => self
                .runs
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Imports => self
                .imports
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Log => match &self.log {
                Some(log) => div().h(px(460.)).child(log.clone()).into_any_element(),
                None => theme::muted(t!("ps.detail.no_runs")).into_any_element(),
            },
            Tab::Gamepad => self
                .gamepad
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Keyboard => match &self.controls {
                Some(Ok(controls)) => controls.clone().into_any_element(),
                Some(Err(err)) => theme::error(err).into_any_element(),
                None => theme::muted(t!("ps.detail.reading_keyboard")).into_any_element(),
            },
            Tab::Graphics => self
                .graphics
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Mods => self
                .mods
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Online => self
                .online
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Overview => self.overview(&entry, cx),
        };
        div()
            .flex()
            .flex_col()
            .child(Self::hero(&entry))
            .child(self.action_bar(&entry, cx))
            .children(notice.clone().or_else(|| run_notice.clone()).map(|(text, error)| {
                div()
                    .mx(px(32.))
                    .mt(px(16.))
                    .child(if error { theme::error(text) } else { theme::muted(text) })
            }))
            .children(
                (notice.is_none() && run_notice.is_some() && self.report.is_none()).then(|| {
                    div()
                        .mx(px(32.))
                        .mt(px(12.))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().flex().child(
                            button("ps-run-report", ButtonKind::Secondary, t!("ps.report.open"), false).on_click(
                                move |_, _, cx| {
                                    report_entity.update(cx, |this, cx| this.open_report(report_dir.clone(), cx));
                                },
                            ),
                        ))
                }),
            )
            .children(
                self.report_error
                    .as_ref()
                    .map(|text| div().mx(px(32.)).mt(px(12.)).child(theme::error(text))),
            )
            .children(
                self.report
                    .clone()
                    .map(|panel| div().mx(px(32.)).mt(px(12.)).child(panel)),
            )
            .child(div().mx(px(32.)).mt(px(24.)).child(tabs))
            .child(div().px(px(32.)).pt(px(20.)).children(copy_entry).child(body))
            .into_any_element()
    }
}
