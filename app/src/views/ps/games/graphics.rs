//! Gráficos: bbport's settings, grouped as its own launcher grouped them.
//! They are bbport's and not the game folder's, so every Bloodborne copy
//! shares them; they apply on the next start. The game effects are the
//! selected title's, as the port lists them when it can.

use std::collections::HashMap;
use std::io;

use decanter_ps::bbport::effects::{self, Section};
use decanter_ps::bbport::values::{self, Values};
use decanter_ps::bbport::{
    Bbport, Control, GROUPS, Group, MEMORY_MODEL_SETTINGS, Setting, single_memory_model_blocking,
};
use decanter_ps::port::Port;
use decanter_ps::toolchain::validation::{self, Unavailable};
use gpui::{AnyElement, Context, Entity, SharedString, Task, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::extras::ExtrasView;
use crate::state::{Ps, vk_layer_unavailable_text};
use crate::theme;
use crate::widgets::{Select, checkbox, select::SelectStyle};

/// The group of [`GROUPS`] the port's own list of the title's effects replaces.
const EFFECTS_GROUP: &str = "ps.graphics.groups.effects";

/// The port's answer for one title, with the build that gave it.
struct PortEffects {
    build: Option<String>,
    /// `None` when the port cannot list them: [`GROUPS`]' Bloodborne table stays.
    sections: Option<Vec<Section>>,
}

pub struct GraphicsView {
    ps: Entity<Ps>,
    values: Option<Values>,
    /// The checkout's port has only the PC memory model, so the old one's settings are left out.
    single_memory_model: bool,
    /// Why the last launch that asked for the validation layer got none, for its setting's note.
    vk_layer_unavailable: Option<Unavailable>,
    error: Option<String>,
    extras: Entity<ExtrasView>,
    title_id: Option<String>,
    /// By `TITLE_ID`, so going back to a game doesn't run the port again until it is rebuilt.
    effects: HashMap<String, PortEffects>,
    /// Asked for and not answered yet: its effects group waits rather than showing Bloodborne's.
    querying: Option<String>,
}

impl GraphicsView {
    pub fn new(ps: &Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let load = cx.background_spawn(async {
            let root = decanter_ps::paths::active();
            let single_memory_model = single_memory_model_blocking(&Port::Bbport.checkout(&root));
            let vk_layer_unavailable = validation::last_unavailable_blocking(&root);
            (Values::load(&root), single_memory_model, vk_layer_unavailable)
        });
        cx.spawn(async move |this, cx| {
            let (values, single_memory_model, vk_layer_unavailable) = load.await;
            let _ = this.update(cx, |this, cx| {
                this.values = Some(values);
                this.single_memory_model = single_memory_model;
                this.vk_layer_unavailable = vk_layer_unavailable;
                cx.notify();
            });
        })
        .detach();
        Self {
            ps: ps.clone(),
            values: None,
            single_memory_model: false,
            vk_layer_unavailable: None,
            error: None,
            extras: cx.new(|cx| ExtrasView::new(ps.clone(), cx)),
            title_id: None,
            effects: HashMap::new(),
            querying: None,
        }
    }

    /// The game whose effects are shown. Only once bbport is built, since the list comes from its
    /// binary; until then, and with a port that cannot list them, Bloodborne's table stays.
    pub fn show_title(&mut self, title_id: &str, cx: &mut Context<Self>) {
        self.title_id = Some(title_id.to_owned());
        cx.notify();
        let Some(toolchain) = self
            .ps
            .read(cx)
            .port(Port::Bbport)
            .and_then(|state| state.toolchain().cloned())
        else {
            return;
        };
        let title_id = title_id.to_owned();
        let known_build = self.effects.get(&title_id).map(|known| known.build.clone());
        if known_build.is_none() {
            self.querying = Some(title_id.clone());
        }
        cx.spawn(async move |this, cx| {
            let bbport = Bbport::new(decanter_ps::paths::active());
            let build = bbport.build_id().await;
            let sections = if known_build.as_ref() == Some(&build) {
                None
            } else {
                Some(effects::list(&bbport, &toolchain, &title_id).await)
            };
            let _ = this.update(cx, |this, cx| {
                if this.querying.as_ref() == Some(&title_id) {
                    this.querying = None;
                }
                if let Some(sections) = sections {
                    this.effects.insert(title_id, PortEffects { build, sections });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn change(&mut self, setting: &'static Setting, value: String, cx: &mut Context<Self>) {
        let Some(values) = &mut self.values else {
            return;
        };
        values.set(setting, &value);
        let task =
            cx.background_spawn(async move { values::save(&decanter_ps::paths::active(), setting, &value, None) });
        Self::after_save(task, cx);
    }

    fn change_ini(&mut self, key: SharedString, value: &'static str, cx: &mut Context<Self>) {
        let Some(values) = &mut self.values else {
            return;
        };
        values.set_ini(&key, value);
        let task = cx.background_spawn(async move { values::save_ini(&decanter_ps::paths::active(), &key, value) });
        Self::after_save(task, cx);
    }

    fn after_save(task: Task<io::Result<()>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| t!("ps.graphics.save_error", error = err).into_owned());
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
                        .map(|option| (SharedString::from(option.1), SharedString::from(t!(option.0)))),
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
                    t!("ps.graphics.no_limit").into_owned()
                } else {
                    format!("{now:.decimals$}")
                };
                let (down, up) = (values.stepped(setting, -1, &[]), values.stepped(setting, 1, &[]));
                (self.stepper(setting, shown, down, up, cx), setting.note)
            }
            // Its own tab, Mando; `GROUPS` rows of it are skipped below.
            Control::Gamepad => (div().into_any_element(), None),
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
                .rounded(px(theme::RADIUS_SM))
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

    fn setting_row(
        &self,
        values: &Values,
        setting: &'static Setting,
        cx: &mut Context<Self>,
    ) -> (String, Option<String>, AnyElement) {
        let (control, note) = self.control(values, setting, cx);
        let note = match self.vk_layer_unavailable {
            Some(why) if setting.key == "vk_validation" => Some(vk_layer_unavailable_text(why)),
            _ => note.map(|note| t!(note).into_owned()),
        };
        (t!(setting.label).into_owned(), note, control)
    }

    /// The port's sections for the selected title instead of `fallback`, the Bloodborne table,
    /// with its `model_lod` row where a section asks for it.
    fn port_effects(
        &self,
        values: &Values,
        fallback: &'static Group,
        cx: &mut Context<Self>,
    ) -> Option<Vec<AnyElement>> {
        let title_id = self.title_id.as_ref()?;
        if self.querying.as_ref() == Some(title_id) && !self.effects.contains_key(title_id) {
            return Some(Vec::new());
        }
        let sections = self.effects.get(title_id)?.sections.as_ref()?;
        let model_lod = fallback.settings.iter().find(|setting| setting.key == "model_lod");
        let locale = rust_i18n::locale();
        let groups = sections
            .iter()
            .filter_map(|section| {
                let mut rows = Vec::with_capacity(section.effects.len() + 1);
                if section.model_detail
                    && let Some(setting) = model_lod
                {
                    rows.push(self.setting_row(values, setting, cx));
                }
                for effect in &section.effects {
                    let on = values.ini(&effect.key).map_or(effect.default, |value| value == "1");
                    let next = if on { "0" } else { "1" };
                    let key = SharedString::from(effect.key.clone());
                    let toggle = div()
                        .id(key.clone())
                        .p_1()
                        .cursor_pointer()
                        .child(checkbox(on))
                        .on_click(cx.listener(move |this, _, _, cx| this.change_ini(key.clone(), next, cx)));
                    rows.push((
                        effect.name.get(&locale).to_owned(),
                        effect.description.as_ref().map(|text| text.get(&locale).to_owned()),
                        toggle.into_any_element(),
                    ));
                }
                if rows.is_empty() {
                    return None;
                }
                let notes = fallback
                    .note
                    .map(|note| t!(note).into_owned())
                    .into_iter()
                    .chain(section.hint.as_ref().map(|hint| hint.get(&locale).to_owned()))
                    .collect();
                Some(group(t!(fallback.title).into_owned(), notes, rows))
            })
            .collect();
        Some(groups)
    }
}

/// A titled card of settings rows, each `(label, note, control)`.
fn group(title: String, notes: Vec<String>, rows: Vec<(String, Option<String>, AnyElement)>) -> AnyElement {
    let count = rows.len();
    let rows = rows.into_iter().enumerate().map(|(index, (label, note, control))| {
        theme::compact_row(index + 1 == count)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(label)
                    .children(note.map(|note| theme::muted(note).text_size(px(12.)))),
            )
            .child(control)
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(theme::h3(title))
        .children(notes.into_iter().map(|note| theme::muted(note).text_size(px(12.))))
        .child(theme::card().children(rows))
        .into_any_element()
}

impl Render for GraphicsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(values) = self.values.clone() else {
            return theme::muted(t!("ps.graphics.reading")).into_any_element();
        };
        let mut groups = Vec::with_capacity(GROUPS.len());
        for fallback in GROUPS {
            if fallback.title == EFFECTS_GROUP
                && let Some(port_groups) = self.port_effects(&values, fallback, cx)
            {
                groups.extend(port_groups);
                continue;
            }
            // The controller is the Mando tab's; a group left empty goes.
            let rows: Vec<_> = fallback
                .settings
                .iter()
                .filter(|setting| !matches!(setting.control, Control::Gamepad))
                .filter(|setting| !(self.single_memory_model && MEMORY_MODEL_SETTINGS.contains(&setting.key)))
                .map(|setting| self.setting_row(&values, setting, cx))
                .collect();
            if rows.is_empty() {
                continue;
            }
            let notes = fallback.note.map(|note| t!(note).into_owned()).into_iter().collect();
            groups.push(group(t!(fallback.title).into_owned(), notes, rows));
        }
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(theme::muted(t!("ps.graphics.description")))
            .children(self.error.clone().map(theme::error))
            .children(groups)
            .child(self.extras.clone())
            .into_any_element()
    }
}
