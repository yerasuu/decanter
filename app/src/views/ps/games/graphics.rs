//! Gráficos: bbport's settings, grouped as its own launcher grouped them.
//! They are bbport's and not the game folder's, so every Bloodborne copy
//! shares them; they apply on the next start.

use decanter_ps::bbport::values::{self, Values};
use decanter_ps::bbport::{Bbport, Control, GROUPS, Setting};
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};

use super::extras::ExtrasView;
use crate::state::Ps;
use crate::theme;
use crate::widgets::{Select, checkbox, select::SelectStyle};

pub struct GraphicsView {
    values: Option<Values>,
    /// Connected controllers `(GUID, name)`, read when the view opens.
    gamepads: Vec<(String, String)>,
    error: Option<String>,
    extras: Entity<ExtrasView>,
}

impl GraphicsView {
    pub fn new(ps: &Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let load = cx.background_spawn(async { Values::load() });
        cx.spawn(async move |this, cx| {
            let values = load.await;
            let _ = this.update(cx, |this, cx| {
                this.values = Some(values);
                cx.notify();
            });
        })
        .detach();
        // Only once bbport is built: the list comes from a tool it builds.
        let toolchain = ps
            .read(cx)
            .port(Port::Bbport)
            .and_then(|state| state.toolchain().cloned());
        if let Some(toolchain) = toolchain {
            cx.spawn(async move |this, cx| {
                let found = Bbport::new(Port::Bbport.checkout())
                    .connected_gamepads(&toolchain)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.gamepads = found;
                    cx.notify();
                });
            })
            .detach();
        }
        let ps = ps.clone();
        Self {
            values: None,
            gamepads: Vec::new(),
            error: None,
            extras: cx.new(|cx| ExtrasView::new(ps, cx)),
        }
    }

    fn change(&mut self, setting: &'static Setting, value: String, cx: &mut Context<Self>) {
        let Some(values) = &mut self.values else {
            return;
        };
        values.set(setting, &value);
        let gamepad_name = matches!(setting.control, Control::Gamepad).then(|| {
            self.gamepads
                .iter()
                .find(|(guid, _)| *guid == value)
                .map_or_else(String::new, |(_, name)| name.clone())
        });
        let task = cx.background_spawn(async move { values::save(setting, &value, gamepad_name.as_deref()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err().map(|err| format!("No se pudo guardar: {err}"));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn control(
        &self,
        values: &Values,
        setting: &'static Setting,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<&'static str>) {
        let current = values.get(setting);
        match &setting.control {
            Control::Toggle { default } => {
                let on = current.map_or(*default, |value| value == "1");
                let next = if on { "0" } else { "1" };
                let toggle = div()
                    .id(SharedString::from(setting.key))
                    .p_1()
                    .cursor_pointer()
                    .child(checkbox(on))
                    .on_click(cx.listener(move |this, _, _, cx| this.change(setting, next.to_owned(), cx)));
                (toggle.into_any_element(), setting.note)
            }
            Control::Choice { options, default } => {
                let selected = current.unwrap_or_else(|| (*default).to_owned());
                let note = options
                    .iter()
                    .find(|option| option.1 == selected)
                    .and_then(|option| option.2)
                    .or(setting.note);
                let entity = cx.entity();
                let select = Select::new(
                    SharedString::from(setting.key),
                    options
                        .iter()
                        .map(|option| (SharedString::from(option.1), SharedString::from(option.0))),
                    selected,
                )
                .style(SelectStyle::Compact)
                .on_change(move |value, _, cx| {
                    let value = value.to_string();
                    entity.update(cx, |this, cx| this.change(setting, value, cx));
                });
                (select.into_any_element(), note)
            }
            Control::Number { default, decimals, .. } => {
                let now = current.and_then(|value| value.parse::<f64>().ok()).unwrap_or(*default);
                let shown = if *decimals == 0 && now == 0.0 {
                    "Sin límite".to_owned()
                } else {
                    format!("{now:.decimals$}")
                };
                let (down, up) = (values.stepped(setting, -1, &[]), values.stepped(setting, 1, &[]));
                (self.stepper(setting, shown, down, up, cx), setting.note)
            }
            Control::Gamepad => {
                let guid = current.unwrap_or_default();
                let unplugged =
                    (!guid.is_empty() && !self.gamepads.iter().any(|(known, _)| *known == guid)).then(|| {
                        let name = values.launch().text("gamepad_name").unwrap_or(&guid);
                        (
                            SharedString::from(guid.clone()),
                            SharedString::from(format!("{name} (desconectado)")),
                        )
                    });
                let options = std::iter::once((SharedString::default(), SharedString::from("Automático (el primero)")))
                    .chain(
                        self.gamepads
                            .iter()
                            .map(|(guid, name)| (SharedString::from(guid.clone()), SharedString::from(name.clone()))),
                    )
                    .chain(unplugged);
                let entity = cx.entity();
                let select = Select::new(SharedString::from(setting.key), options, guid)
                    .style(SelectStyle::Compact)
                    .on_change(move |value, _, cx| {
                        let value = value.to_string();
                        entity.update(cx, |this, cx| this.change(setting, value, cx));
                    });
                (select.into_any_element(), setting.note)
            }
        }
    }

    /// ‹ value ›, for numbers.
    fn stepper(
        &self,
        setting: &'static Setting,
        shown: String,
        down: String,
        up: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let arrow = |id: String, text: &'static str, value: String| {
            div()
                .id(SharedString::from(id))
                .w(px(28.))
                .h(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|arrow| arrow.text_color(rgb(theme::TEXT_BRIGHT)).bg(rgb(theme::BG_PANEL_LIGHT)))
                .on_click(cx.listener(move |this, _, _, cx| this.change(setting, value.clone(), cx)))
                .child(text)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .bg(rgb(theme::BG_PANEL_3))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .child(arrow(format!("{}-down", setting.key), "‹", down))
            .child(
                div()
                    .w(px(110.))
                    .flex()
                    .justify_center()
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .child(shown),
            )
            .child(arrow(format!("{}-up", setting.key), "›", up))
            .into_any_element()
    }
}

impl Render for GraphicsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(values) = self.values.clone() else {
            return theme::muted("Leyendo los ajustes…").into_any_element();
        };
        let groups: Vec<AnyElement> = GROUPS
            .iter()
            .map(|group| {
                let count = group.settings.len();
                let rows: Vec<AnyElement> = group
                    .settings
                    .iter()
                    .enumerate()
                    .map(|(index, setting)| {
                        let (control, note) = self.control(&values, setting, cx);
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_4()
                            .py_2()
                            .when(index + 1 < count, |row| row.border_b_1().border_color(rgb(theme::LINE)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(setting.label)
                                    .children(note.map(|note| theme::muted(note).text_size(px(12.)))),
                            )
                            .child(control)
                            .into_any_element()
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(theme::h3(group.title))
                    .children(group.note.map(|note| theme::muted(note).text_size(px(12.))))
                    .child(theme::card().children(rows))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(theme::muted(
                "Ajustes de bbport, comunes a todas tus copias de Bloodborne. Se aplican en el próximo arranque.",
            ))
            .children(self.error.clone().map(theme::error))
            .children(groups)
            .child(self.extras.clone())
            .into_any_element()
    }
}
