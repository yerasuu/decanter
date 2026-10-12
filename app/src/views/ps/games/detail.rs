//! The selected game: what it is, the play button with what stands in its
//! way, and its tabs.

use std::path::PathBuf;

use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::{self, Bbport};
use decanter_ps::mapping::Target;
use decanter_ps::port::Port;
use decanter_ps::status::{Entry, Status};
use gpui::{AnyElement, Context, Entity, ObjectFit, Window, div, img, prelude::*, px, rgb};

use super::gamepad::GamepadView;
use super::graphics::GraphicsView;
use super::keyboard::KeyboardView;
use super::mods::ModsView;
use super::{art_url, display_title, platform_label, status_text};
use crate::state::{JobKey, Ps};
use crate::theme;
use crate::views::ps::log::LogView;
use crate::widgets::{ButtonKind, button, tooltip};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Keyboard,
    Gamepad,
    Graphics,
    Mods,
    Log,
}

const TABS: [(Tab, &str); 3] = [
    (Tab::Overview, "Resumen"),
    (Tab::Keyboard, "Teclado"),
    (Tab::Log, "Registro"),
];

/// bbport has settings and gamepad layouts of its own; AnyPS5 reads the
/// controller as it is and takes no settings.
const BBPORT_TABS: [(Tab, &str); 6] = [
    (Tab::Overview, "Resumen"),
    (Tab::Keyboard, "Teclado"),
    (Tab::Gamepad, "Mando"),
    (Tab::Graphics, "Gráficos"),
    (Tab::Mods, "Mods"),
    (Tab::Log, "Registro"),
];

/// Another PS4 title on bbport: its mods are Bloodborne's.
const OTHER_PS4_TABS: [(Tab, &str); 5] = [
    (Tab::Overview, "Resumen"),
    (Tab::Keyboard, "Teclado"),
    (Tab::Gamepad, "Mando"),
    (Tab::Graphics, "Gráficos"),
    (Tab::Log, "Registro"),
];

/// A game no port runs has nothing to bind.
const UNSUPPORTED_TABS: [(Tab, &str); 1] = [(Tab::Overview, "Resumen")];

pub struct DetailView {
    ps: Entity<Ps>,
    /// By folder: the list is rebuilt on every scan, and a folder is what
    /// stays the same game across one.
    selected: Option<PathBuf>,
    tab: Tab,
    log: Option<Entity<LogView>>,
    /// The bindings of `selected`, read when its Controles tab opens.
    controls: Option<Result<Entity<KeyboardView>, String>>,
    /// Shared by every Bloodborne copy, so kept across selections.
    gamepad: Option<Entity<GamepadView>>,
    /// Shared by every Bloodborne copy, so kept across selections.
    graphics: Option<Entity<GraphicsView>>,
    mods: Option<Entity<ModsView>>,
}

impl DetailView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| {
            this.sync_log(cx);
            cx.notify();
        })
        .detach();
        Self {
            ps,
            selected: None,
            tab: Tab::Overview,
            log: None,
            controls: None,
            gamepad: None,
            graphics: None,
            mods: None,
        }
    }

    pub const fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    /// Back to the covers.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.controls = None;
        self.log = None;
        cx.notify();
    }

    pub fn select(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(dir);
        self.tab = Tab::Overview;
        self.controls = None;
        self.sync_log(cx);
        cx.notify();
    }

    fn select_tab(&mut self, tab: Tab, entry: Option<(Port, String)>, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Gamepad && self.gamepad.is_none() {
            let ps = self.ps.clone();
            self.gamepad = Some(cx.new(|cx| GamepadView::new(&ps, cx)));
        }
        if tab == Tab::Graphics && self.graphics.is_none() {
            let ps = self.ps.clone();
            self.graphics = Some(cx.new(|cx| GraphicsView::new(&ps, cx)));
        }
        if tab == Tab::Mods && self.mods.is_none() {
            let ps = self.ps.clone();
            self.mods = Some(cx.new(|cx| ModsView::new(ps, cx)));
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
        let (target, path) = match port {
            Port::Bbport => (Target::Bbport, Bbport::ini_path()),
            Port::AnyPs5 => (Target::AnyPs5, AnyPs5::binds_path(&title_id)),
        };
        let selected = self.selected.clone();
        let read_path = path.clone();
        let task = cx.background_spawn(async move {
            let store =
                decanter_ps::layouts::Store::load().map_err(|err| format!("No se pudieron leer los layouts: {err}"));
            decanter_ps::binds::read(target, &read_path).and_then(|mapping| store.map(|store| (mapping, store)))
        });
        cx.spawn(async move |this, cx| {
            let mapping = task.await;
            let _ = this.update(cx, |this, cx| {
                // Another game picked meanwhile: its own tab reads its own file.
                if this.selected != selected {
                    return;
                }
                this.controls = Some(
                    mapping.map(|(mapping, store)| cx.new(|cx| KeyboardView::new(target, path, mapping, store, cx))),
                );
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
                    .child(hero_stat(game.version.clone(), "VERSIÓN"))
                    .child(hero_stat(status_text(&entry.status).to_owned(), "ESTADO")),
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
        let running = ps.is_running(&key, cx);
        let preparing = entry
            .port
            .and_then(|port| ps.port(port))
            .is_some_and(|state| state.is_busy());
        let playable = entry.status.playable();
        let action = if running {
            button("ps-stop", ButtonKind::PlaySecondary, "DETENER", false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    this.ps.update(cx, |ps, cx| ps.stop(&key, cx));
                }))
                .into_any_element()
        } else {
            let label = if preparing { "PREPARANDO…" } else { "JUGAR" };
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
                button("ps-open-folder", ButtonKind::Icon, "Abrir carpeta", false)
                    .tooltip(tooltip("Abrir la carpeta del volcado"))
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
        let fact = |label: &'static str, value: String| {
            div()
                .flex()
                .gap_4()
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(label),
                )
                .child(theme::mono().min_w_0().truncate().child(value))
        };
        let explanation = match &entry.status {
            Status::Ready => None,
            Status::NeedsConvert => Some("La primera vez se convierte con AnyPS5 antes de arrancar.".to_owned()),
            Status::NeedsMerge(_) => Some(
                "Hay una actualización 1.09 junto a este volcado. Fusionarla crea una copia jugable al lado, \
                 con enlaces duros: no ocupa espacio extra y el volcado original no cambia."
                    .to_owned(),
            ),
            Status::HasPlayable(dir) => Some(format!("Ya tenés la copia 1.09 en {}; jugá esa.", dir.display())),
            Status::OtherEdition => {
                Some("bbport solo ejecuta la edición CUSA03173 de Bloodborne en su versión 1.09.".to_owned())
            }
            Status::NeedsUpdate => {
                Some("bbport necesita la versión 1.09: añadí su actualización a una de tus carpetas.".to_owned())
            }
            Status::Experimental => Some(
                "bbport solo está hecho para Bloodborne. Este juego arranca con lo que le falte al port \
                 sustituido, sin parches ni mods: mira hasta dónde llega en Registro."
                    .to_owned(),
            ),
            Status::NeedsVr => Some("Este juego solo arranca con un casco PS VR, y el port no tiene uno.".to_owned()),
            Status::Encrypted => Some("El eboot.bin sigue cifrado; hay que descifrarlo en la consola.".to_owned()),
            Status::NoEboot => Some("Este volcado no tiene eboot.bin.".to_owned()),
        };
        let merge = match &entry.status {
            Status::NeedsMerge(update) => {
                let (base, update) = (game.dir.clone(), update.clone());
                Some(
                    button("ps-merge", ButtonKind::Primary, "Fusionar 1.09", false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            let (base, update) = (base.clone(), update.clone());
                            this.ps.update(cx, |ps, cx| ps.merge(base, update, cx));
                        },
                    )),
                )
            }
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(explanation.map(theme::muted))
            .children(merge.map(|merge| div().flex().child(merge)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(fact("Title ID", game.title_id.clone()))
                    .child(fact("Versión", game.version.clone()))
                    .child(fact("Carpeta", game.dir.display().to_string())),
            )
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
        let Some(entry) = entry else {
            return div()
                .p_8()
                .child(theme::muted("Elegí un juego de la lista."))
                .into_any_element();
        };
        let entity = cx.entity();
        let port = entry.port.map(|port| (port, entry.game.title_id.clone()));
        let tabs: &[(Tab, &str)] = match entry.port {
            Some(Port::Bbport) if bbport::is_bloodborne(&entry.game) => &BBPORT_TABS,
            Some(Port::Bbport) => &OTHER_PS4_TABS,
            Some(Port::AnyPs5) => &TABS,
            None => &UNSUPPORTED_TABS,
        };
        let tabs = crate::widgets::tabs("ps-detail-tabs", tabs, self.tab, move |tab, _, cx| {
            let port = port.clone();
            entity.update(cx, |this, cx| this.select_tab(tab, port, cx));
        });
        let body = match self.tab {
            Tab::Log => match &self.log {
                Some(log) => div().h(px(460.)).child(log.clone()).into_any_element(),
                None => theme::muted("Todavía no se ejecutó nada para este juego.").into_any_element(),
            },
            Tab::Gamepad => self
                .gamepad
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Keyboard => match &self.controls {
                Some(Ok(controls)) => controls.clone().into_any_element(),
                Some(Err(err)) => theme::error(err.clone()).into_any_element(),
                None => theme::muted("Leyendo el teclado…").into_any_element(),
            },
            Tab::Graphics => self
                .graphics
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Mods => self
                .mods
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Tab::Overview => self.overview(&entry, cx),
        };
        div()
            .flex()
            .flex_col()
            .child(Self::hero(&entry))
            .child(self.action_bar(&entry, cx))
            .children(notice.map(|(text, error)| {
                div()
                    .mx(px(32.))
                    .mt(px(16.))
                    .child(if error { theme::error(text) } else { theme::muted(text) })
            }))
            .child(div().mx(px(32.)).mt(px(24.)).child(tabs))
            .child(div().px(px(32.)).pt(px(20.)).child(body))
            .into_any_element()
    }
}

/// `.hero-stat`, as Biblioteca draws it.
fn hero_stat(value: String, label: &'static str) -> gpui::Div {
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
                .text_size(px(17.))
                .font_weight(gpui::FontWeight::BOLD)
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
