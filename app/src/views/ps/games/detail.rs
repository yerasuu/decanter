//! The selected game: what it is, the play button with what stands in its
//! way, and its tabs.

use std::path::PathBuf;

use decanter_ps::status::{Entry, Status};
use gpui::{AnyElement, Context, Entity, Window, div, prelude::*, px, rgb};

use super::{display_title, platform_label};
use crate::state::{JobKey, Ps};
use crate::theme;
use crate::views::ps::log::LogView;
use crate::widgets::{ButtonKind, button};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Log,
}

const TABS: [(Tab, &str); 2] = [(Tab::Overview, "Resumen"), (Tab::Log, "Registro")];

pub struct DetailView {
    ps: Entity<Ps>,
    /// By folder: the list is rebuilt on every scan, and a folder is what
    /// stays the same game across one.
    selected: Option<PathBuf>,
    tab: Tab,
    log: Option<Entity<LogView>>,
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
        }
    }

    pub const fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    pub fn select(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(dir);
        self.tab = Tab::Overview;
        self.sync_log(cx);
        cx.notify();
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

    fn hero(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
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
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(theme::h1(display_title(game)))
                    .child(theme::badge(platform_label(game), false)),
            )
            .child(div().flex().items_center().gap_3().child(action).child(
                button("ps-open-folder", ButtonKind::Secondary, "Abrir carpeta", false).on_click(move |_, _, _| {
                    if let Err(err) = crate::desktop::open(&folder) {
                        tracing::warn!(dir = %folder.display(), %err, "could not open the game folder");
                    }
                }),
            ))
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
            Status::Encrypted => Some("El eboot.bin sigue cifrado; hay que descifrarlo en la consola.".to_owned()),
            Status::NoEboot => Some("Este volcado no tiene eboot.bin.".to_owned()),
            Status::Unsupported => Some("Ningún port ejecuta este juego todavía.".to_owned()),
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
        let tabs = crate::widgets::tabs("ps-detail-tabs", &TABS, self.tab, move |tab, _, cx| {
            entity.update(cx, |this, cx| {
                this.tab = tab;
                cx.notify();
            });
        });
        let body = match (self.tab, &self.log) {
            (Tab::Log, Some(log)) => div().h(px(460.)).child(log.clone()).into_any_element(),
            (Tab::Log, None) => theme::muted("Todavía no se ejecutó nada para este juego.").into_any_element(),
            (Tab::Overview, _) => self.overview(&entry, cx),
        };
        div()
            .id("ps-detail")
            .size_full()
            .overflow_y_scroll()
            .p_8()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.hero(&entry, cx))
            .children(notice.map(|(text, error)| if error { theme::error(text) } else { theme::muted(text) }))
            .child(tabs)
            .child(body)
            .into_any_element()
    }
}
