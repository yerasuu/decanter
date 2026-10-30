//! Gráficos: bbport's settings, grouped as its own launcher grouped them.
//! Every row is the selected title's own: its port ini ([`Bbport::title_ini`]) and launch
//! settings ([`LauncherSettings::of_title`]). They apply on the next start. The game effects are
//! the selected title's, as the port lists them when it can. The same rows edit the defaults new
//! titles copy ([`GraphicsView::show_defaults`]), on the PS section's Ajustes.

use std::collections::HashMap;
use std::io;

use decanter_core::capabilities::Feature;
use decanter_nix::validation::{self, Unavailable};
use decanter_ps::bbport::effects::{self, Section};
use decanter_ps::bbport::frame_gen::{self, FrameGen};
use decanter_ps::bbport::values::{self, Values};
use decanter_ps::bbport::{
    Bbport, Control, GROUPS, Group, LauncherSettings, MEMORY_MODEL_SETTINGS, Setting, Store,
    single_memory_model_blocking,
};
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Entity, SharedString, Task, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::extras::ExtrasView;
use super::scope_note;
use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::state::{Ps, vk_layer_unavailable_text};
use crate::theme;
use crate::widgets::{Select, checkbox, select::SelectStyle};

/// The group of [`GROUPS`] the port's own list of the title's effects replaces.
const EFFECTS_GROUP: &str = "ps.graphics.groups.effects";
/// The group of [`GROUPS`] only psport builds listing `key_stick` read.
const KEY_STICK_GROUP: &str = "ps.graphics.groups.keyboard";

/// The port's answer for one title, with the build that gave it.
struct PortEffects {
    build: Option<String>,
    /// `None` when the port cannot list them: [`GROUPS`]' Bloodborne table stays.
    sections: Option<Vec<Section>>,
}

pub struct GraphicsView {
    ps: Entity<Ps>,
    /// The shown title's, read again each time it is shown, since its in-game menu writes the same
    /// file.
    values: Option<Values>,
    /// The checkout's port has only the PC memory model, so the old one's settings are left out.
    single_memory_model: bool,
    /// Why the last launch that asked for the validation layer got none, for its setting's note.
    vk_layer_unavailable: Option<Unavailable>,
    error: Option<Text>,
    extras: Entity<ExtrasView>,
    title_id: Option<String>,
    frame_gen: FrameGen,
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
            let vk_layer_unavailable = validation::last_unavailable_blocking(&root.toolchain());
            (single_memory_model, vk_layer_unavailable)
        });
        cx.spawn(async move |this, cx| {
            let (single_memory_model, vk_layer_unavailable) = load.await;
            let _ = this.update(cx, |this, cx| {
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
            frame_gen: FrameGen::Off,
            effects: HashMap::new(),
            querying: None,
        }
    }

    /// The game whose effects are shown. Only once bbport is built, since the list comes from its
    /// binary; until then, and with a port that cannot list them, Bloodborne's table stays.
    pub fn show_title(&mut self, title_id: &str, cx: &mut Context<Self>) {
        self.title_id = Some(title_id.to_owned());
        self.load_values(title_id, cx);
        self.extras.update(cx, |extras, cx| extras.show_title(title_id, cx));
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

    /// The defaults new titles copy instead of a title's settings: the same rows, without the
    /// title's own effects list, frame generation or extras, which have no defaults here.
    pub fn show_defaults(&mut self, cx: &mut Context<Self>) {
        self.title_id = None;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async { Values::load_defaults(&decanter_ps::paths::active()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.title_id.is_some() {
                    return;
                }
                match loaded {
                    Ok(values) => {
                        this.values = Some(values);
                        this.error = None;
                    }
                    Err(err) => this.error = Some(Text::new("ps.defaults.read_error").arg("error", err)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Another title's values go at once, so nothing is shown or written for the wrong one while
    /// this title's are read.
    fn load_values(&mut self, title_id: &str, cx: &mut Context<Self>) {
        if self
            .values
            .as_ref()
            .is_some_and(|values| values.is_defaults() || values.title_id() != title_id)
        {
            self.values = None;
        }
        let title_id = title_id.to_owned();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn({
                    let title_id = title_id.clone();
                    async move { Values::load(&decanter_ps::paths::active(), &title_id) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.title_id.as_deref() != Some(title_id.as_str()) {
                    return;
                }
                match loaded {
                    Ok(values) => {
                        this.frame_gen = FrameGen::of_title(values.launch(), &title_id);
                        this.values = Some(values);
                        this.error = None;
                    }
                    Err(err) => this.error = Some(Text::new("ps.scope.read_error").arg("error", err)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Choosing off removes the key, so the file keeps only what was changed.
    fn set_frame_gen(&mut self, frame_gen: FrameGen, cx: &mut Context<Self>) {
        let Some(title_id) = self.title_id.clone() else {
            return;
        };
        self.frame_gen = frame_gen;
        let task = cx.background_spawn(async move {
            LauncherSettings::edit_title(&decanter_ps::paths::active(), &title_id, |settings| {
                let value = if frame_gen == FrameGen::Off {
                    ""
                } else {
                    frame_gen.value()
                };
                settings.set_title_text(&title_id, frame_gen::KEY, value);
            })
        });
        Self::after_save(task, cx);
    }

    fn frame_gen_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let options = FrameGen::ALL.map(|choice| {
            (
                SharedString::from(choice.value()),
                SharedString::from(t!(format!("ps.graphics.frame_gen.options.{}", choice.value()))),
            )
        });
        let select = Select::new("frame_gen", options, SharedString::from(self.frame_gen.value()))
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| {
                if let Some(choice) = FrameGen::parse(value) {
                    entity.update(cx, |this, cx| this.set_frame_gen(choice, cx));
                }
            });
        group(
            t!("ps.graphics.frame_gen.title").into_owned(),
            Vec::new(),
            vec![(
                t!("ps.graphics.frame_gen.label").into_owned(),
                Some(t!("ps.graphics.frame_gen.note").into_owned()),
                select.into_any_element(),
            )],
        )
    }

    fn change(&mut self, setting: &'static Setting, value: String, cx: &mut Context<Self>) {
        let Some(values) = &mut self.values else {
            return;
        };
        values.set(setting, &value);
        let ini = values.ini_path().to_owned();
        let title_id = values.title_id().to_owned();
        let is_defaults = values.is_defaults();
        let task = cx.background_spawn(async move {
            let root = decanter_ps::paths::active();
            if is_defaults {
                values::save_default(&root, setting, &value)
            } else {
                values::save(&root, &title_id, &ini, setting, &value, None)
            }
        });
        Self::after_save(task, cx);
    }

    fn change_ini(&mut self, key: SharedString, value: &'static str, cx: &mut Context<Self>) {
        let Some(values) = &mut self.values else {
            return;
        };
        values.set_ini(&key, value);
        let ini = values.ini_path().to_owned();
        let task = cx.background_spawn(async move { values::save_ini(&ini, &key, value) });
        Self::after_save(task, cx);
    }

    fn after_save(task: Task<io::Result<()>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("ps.graphics.save_error").arg("error", err));
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
                // The chosen one stays even when unsuitable, so the list never shows another as chosen.
                let hidden = Caps::get(cx).ps_hidden_options();
                let offered = options
                    .iter()
                    .filter(|option| option.1 == selected || !hidden.contains(&(setting.key, Some(option.1))))
                    .map(|option| (SharedString::from(option.1), SharedString::from(t!(option.0))));
                let select = Select::new(SharedString::from(setting.key), offered, selected.clone())
                    .style(SelectStyle::Compact)
                    .on_change(move |value, _, cx| {
                        let value = value.to_string();
                        entity.update(cx, |this, cx| this.change(setting, value, cx));
                    });
                (select.into_any_element(), note)
            }
            Control::Number {
                default,
                decimals,
                zero,
                ..
            } => {
                let now = current.and_then(|value| value.parse::<f64>().ok()).unwrap_or(*default);
                let shown = match zero {
                    Some(label) if now == 0.0 => t!(*label).into_owned(),
                    _ => format!("{now:.decimals$}"),
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
            Some(why) if setting.key == "vk_validation" => Some(vk_layer_unavailable_text(why).to_string()),
            _ => note.map(|note| t!(note).into_owned()),
        };
        (t!(setting.label).into_owned(), note, control)
    }

    /// The port's sections for the selected title instead of `fallback`, the Bloodborne table,
    /// with its `model_lod` row where a section asks for it.
    /// The port's own sections in place of `fallback`'s table; its launcher settings (not the
    /// title's patches the port lists) go with the first section. `hidden`: [`Caps::ps_hidden_options`].
    fn port_effects(
        &self,
        values: &Values,
        fallback: &'static Group,
        hidden: &[(&'static str, Option<&'static str>)],
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
            .enumerate()
            .filter_map(|(index, section)| {
                let mut rows = Vec::with_capacity(section.effects.len() + 2);
                if section.model_detail
                    && let Some(setting) = model_lod
                {
                    rows.push(self.setting_row(values, setting, cx));
                }
                if index == 0 {
                    for setting in fallback
                        .settings
                        .iter()
                        .filter(|setting| matches!(setting.store, Store::Launcher))
                        .filter(|setting| !hidden.contains(&(setting.key, None)))
                    {
                        rows.push(self.setting_row(values, setting, cx));
                    }
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
            return self.error.as_ref().map_or_else(
                || theme::muted(t!("ps.graphics.reading")).into_any_element(),
                |error| theme::error(error).into_any_element(),
            );
        };
        let hidden = Caps::get(cx).ps_hidden_options();
        let key_stick_offered = Caps::get(cx).is_offered(Feature::PsKeyStick);
        let mut groups = Vec::with_capacity(GROUPS.len());
        for fallback in GROUPS {
            if fallback.title == KEY_STICK_GROUP && !key_stick_offered {
                continue;
            }
            if fallback.title == EFFECTS_GROUP
                && let Some(port_groups) = self.port_effects(&values, fallback, &hidden, cx)
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
                .filter(|setting| !hidden.contains(&(setting.key, None)))
                .map(|setting| self.setting_row(&values, setting, cx))
                .collect();
            if rows.is_empty() {
                continue;
            }
            let notes = fallback.note.map(|note| t!(note).into_owned()).into_iter().collect();
            groups.push(group(t!(fallback.title).into_owned(), notes, rows));
        }
        if self.title_id.is_some() && Caps::get(cx).is_offered(Feature::PsFrameGen) {
            groups.push(self.frame_gen_group(cx));
        }
        let is_defaults = values.is_defaults();
        let scope = if is_defaults {
            t!("ps.defaults.graphics_note")
        } else {
            scope_note(values.scope())
        };
        div()
            .flex()
            .flex_col()
            .gap_5()
            .when(!is_defaults, |page| {
                page.child(theme::muted(t!("ps.graphics.description")))
            })
            .child(theme::muted(scope))
            .children(self.error.as_ref().map(theme::error))
            .children(groups)
            .when(!is_defaults, |page| page.child(self.extras.clone()))
            .into_any_element()
    }
}
