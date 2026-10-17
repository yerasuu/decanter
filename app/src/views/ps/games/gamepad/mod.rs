//! Mando: which controller bbport reads and which layout its buttons
//! follow. Any layout can be copied into a new editable one; the one
//! picked is the one in `bbport.ini`, and an edit to it is written there
//! at once. Everything applies on the next start. The bindings show as a
//! list or on a drawing of the pad ([`visual`]), whichever the user keeps.

mod visual;

use decanter_ps::bbport::pad::{self, BUTTONS, INPUTS, PadLayout};
use decanter_ps::bbport::values::{self, Values};
use decanter_ps::bbport::{Bbport, Control, GROUPS, Setting};
use decanter_ps::layouts::Store;
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Div, Entity, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::layout_name;
use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, Select, TextInput, button, select::SelectStyle};

/// The layout select's value while `bbport.ini` holds `pad.` lines no
/// layout has.
const CUSTOM: &str = "custom";

/// The app setting that keeps the list or the drawing between sessions.
const MODE_SETTING: &str = "ps_gamepad_mode";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Visual,
}

impl Mode {
    const fn id(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Visual => "visual",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        [Self::List, Self::Visual].into_iter().find(|mode| mode.id() == id)
    }
}

struct Loaded {
    store: Store,
    /// The id of the layout in `bbport.ini`; `None` when it matches none.
    current: Option<String>,
    /// `bbport.ini`'s own `pad.` lines, for copying when they match none.
    custom: PadLayout,
    values: Values,
}

impl Loaded {
    fn layout(&self) -> Option<PadLayout> {
        let id = self.current.as_deref()?;
        self.store.pad_layouts().into_iter().find(|layout| layout.id == id)
    }
}

pub struct GamepadView {
    loaded: Option<Result<Loaded, String>>,
    /// Connected controllers `(GUID, name)`, read when the view opens.
    gamepads: Vec<(String, String)>,
    name: Entity<TextInput>,
    error: Option<String>,
    mode: Mode,
    /// The input picked on the drawing, whose binding the side panel edits.
    selected: Option<&'static str>,
}

fn gamepad_setting() -> Option<&'static Setting> {
    GROUPS
        .iter()
        .flat_map(|group| group.settings)
        .find(|setting| matches!(setting.control, Control::Gamepad))
}

impl GamepadView {
    pub fn new(ps: &Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(t!("ps.layouts.name_placeholder"), cx));
        cx.subscribe(&name, |this, name, event, cx| {
            if matches!(event, InputEvent::Submit) {
                let text = name.read(cx).text().trim().to_owned();
                this.rename(text, cx);
            }
        })
        .detach();
        let load = cx.background_spawn(async {
            let store = match Store::load() {
                Ok(store) => store,
                Err(err) => return Err(t!("ps.layouts.read_error", error = err).into_owned()),
            };
            Ok(Loaded {
                current: pad::current(&decanter_ps::paths::active(), &store.pad_layouts()),
                custom: pad::from_ini(&decanter_ps::paths::active()),
                values: Values::load(&decanter_ps::paths::active()),
                store,
            })
        });
        cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = Some(loaded);
                this.sync_name(cx);
                cx.notify();
            });
        })
        .detach();
        let mode = cx.background_spawn(async {
            crate::core::db()
                .inspect_err(|err| tracing::warn!(%err, "could not open the database for the gamepad view mode"))
                .ok()?
                .setting(MODE_SETTING)
                .inspect_err(|err| tracing::warn!(%err, "could not read the gamepad view mode"))
                .ok()
                .flatten()
        });
        cx.spawn(async move |this, cx| {
            let mode = mode.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(mode) = mode.as_deref().and_then(Mode::from_id) {
                    this.mode = mode;
                    cx.notify();
                }
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
                let found = Bbport::new(decanter_ps::paths::active())
                    .connected_gamepads(&toolchain)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.gamepads = found;
                    cx.notify();
                });
            })
            .detach();
        }
        Self {
            loaded: None,
            gamepads: Vec::new(),
            name,
            error: None,
            mode: Mode::List,
            selected: None,
        }
    }

    fn choose_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        cx.background_spawn(async move {
            let saved = crate::core::db()
                .map_err(|err| err.to_string())
                .and_then(|db| db.set_setting(MODE_SETTING, mode.id()).map_err(|err| err.to_string()));
            if let Err(err) = saved {
                tracing::warn!(%err, mode = mode.id(), "could not save the gamepad view mode");
            }
        })
        .detach();
        cx.notify();
    }

    /// `Lista` / `Visual`: two ways to show the same bindings.
    fn mode_switch(&self, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .bg(rgb(theme::BG_PANEL_3))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS))
            .children(
                [
                    (Mode::List, t!("ps.gamepad.mode.list")),
                    (Mode::Visual, t!("ps.gamepad.mode.visual")),
                ]
                .into_iter()
                .map(|(mode, label)| {
                    let active = mode == self.mode;
                    div()
                        .id(SharedString::from(format!("ps-pad-mode-{}", mode.id())))
                        .px_3()
                        .py(px(4.))
                        .rounded(px(theme::RADIUS_SM))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .text_color(rgb(if active { theme::TEXT_BRIGHT } else { theme::TEXT_MUTED }))
                        .when(active, |item| item.bg(rgb(theme::BG_PANEL_LIGHT)))
                        .when(!active, |item| {
                            item.hover(|item| item.text_color(rgb(theme::TEXT_BRIGHT)))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_mode(mode, cx)))
                        .child(SharedString::from(label))
                }),
            )
    }

    fn loaded_mut(&mut self) -> Option<&mut Loaded> {
        match &mut self.loaded {
            Some(Ok(loaded)) => Some(loaded),
            _ => None,
        }
    }

    fn sync_name(&mut self, cx: &mut Context<Self>) {
        let name = self
            .loaded_mut()
            .and_then(|loaded| loaded.layout())
            .map(|layout| layout.name)
            .unwrap_or_default();
        self.name.update(cx, |input, cx| input.set_text(name, cx));
    }

    /// Makes `layout` the one in use: written into `bbport.ini`, and the
    /// store saved first when it changed. Both off the UI thread.
    fn apply(&mut self, layout: PadLayout, store_changed: bool, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        loaded.current = Some(layout.id.clone());
        let store = store_changed.then(|| loaded.store.clone());
        let task = cx.background_spawn(async move {
            if let Some(store) = store
                && let Err(err) = store.save()
            {
                return Err(err);
            }
            layout.save_to_ini(&decanter_ps::paths::active())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| t!("ps.gamepad.save_error", error = err).into_owned());
                cx.notify();
            });
        })
        .detach();
        self.sync_name(cx);
        cx.notify();
    }

    fn choose(&mut self, id: &str, cx: &mut Context<Self>) {
        let layout = self
            .loaded_mut()
            .and_then(|loaded| loaded.store.pad_layouts().into_iter().find(|layout| layout.id == id));
        if let Some(layout) = layout {
            self.apply(layout, false, cx);
        }
    }

    /// A new editable layout from the one in use, or from `bbport.ini`'s
    /// own lines when they match none.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let source = loaded.layout().unwrap_or_else(|| loaded.custom.clone());
        let name = t!(
            "ps.layouts.copy_name",
            name = layout_name(&source.id, source.builtin, &source.name)
        );
        let copy = loaded.store.copy_pad(&source, name.into_owned());
        self.apply(copy, true, cx);
    }

    fn rename(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let Some(mut layout) = loaded.layout().filter(|layout| !layout.builtin && !name.is_empty()) else {
            return;
        };
        layout.name = name;
        loaded.store.update_pad(&layout);
        self.apply(layout, true, cx);
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let Some(layout) = loaded.layout().filter(|layout| !layout.builtin) else {
            return;
        };
        loaded.store.remove_pad(&layout.id);
        let [default, _] = PadLayout::builtins();
        self.apply(default, true, cx);
    }

    fn assign(&mut self, input: &str, button: &str, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let Some(mut layout) = loaded.layout().filter(|layout| !layout.builtin) else {
            return;
        };
        layout.assign(input, button);
        loaded.store.update_pad(&layout);
        self.apply(layout, true, cx);
    }

    /// The game's own glyphs, or the pad's buttons as the layout puts them,
    /// labelled like the pad in hand (detected) or a fixed style; built at
    /// the next start.
    fn icons_select(&self, values: &Values, cx: &mut Context<Self>) -> AnyElement {
        let current = values.launch().text("button_icons").unwrap_or("pad").to_owned();
        let entity = cx.entity();
        Select::new(
            "ps-button-icons",
            [
                (
                    SharedString::from("pad"),
                    SharedString::from(t!("ps.gamepad.icons.pad")),
                ),
                (
                    SharedString::from("original"),
                    SharedString::from(t!("ps.gamepad.icons.original")),
                ),
            ],
            current,
        )
        .style(SelectStyle::Compact)
        .on_change(move |value, _, cx| {
            let value = value.to_string();
            entity.update(cx, |this, cx| this.choose_icons(value, cx));
        })
        .into_any_element()
    }

    fn choose_icons(&mut self, style: String, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        loaded.values.set_launch_text("button_icons", &style);
        let task = cx.background_spawn(async move {
            decanter_ps::bbport::LauncherSettings::edit(&decanter_ps::paths::active(), |settings| {
                settings.set_text("button_icons", &style);
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| t!("ps.gamepad.save_error", error = err).into_owned());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_gamepad(&mut self, guid: String, cx: &mut Context<Self>) {
        let Some(setting) = gamepad_setting() else {
            return;
        };
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        loaded.values.set(setting, &guid);
        let name = self
            .gamepads
            .iter()
            .find(|(known, _)| *known == guid)
            .map_or_else(String::new, |(_, name)| name.clone());
        let task = cx
            .background_spawn(async move { values::save(&decanter_ps::paths::active(), setting, &guid, Some(&name)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| t!("ps.gamepad.save_error", error = err).into_owned());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn gamepad_select(&self, values: &Values, cx: &mut Context<Self>) -> AnyElement {
        let Some(setting) = gamepad_setting() else {
            return div().into_any_element();
        };
        let guid = values.get(setting).unwrap_or_default();
        let unplugged = (!guid.is_empty() && !self.gamepads.iter().any(|(known, _)| *known == guid)).then(|| {
            let name = values.launch().text("gamepad_name").unwrap_or(&guid);
            (
                SharedString::from(guid.clone()),
                SharedString::from(t!("ps.gamepad.unplugged", name = name)),
            )
        });
        let options = std::iter::once((SharedString::default(), SharedString::from(t!("ps.gamepad.automatic"))))
            .chain(
                self.gamepads
                    .iter()
                    .map(|(guid, name)| (SharedString::from(guid.clone()), SharedString::from(name.clone()))),
            )
            .chain(unplugged);
        let entity = cx.entity();
        Select::new("ps-gamepad", options, guid)
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| {
                let value = value.to_string();
                entity.update(cx, |this, cx| this.choose_gamepad(value, cx));
            })
            .into_any_element()
    }

    /// `input`'s button, picked from a list when the layout is the user's
    /// own. A pad can't be read from here to capture a press, so it is
    /// picked rather than pressed.
    fn binding_control(layout: &PadLayout, input: &'static str, cx: &Context<Self>) -> AnyElement {
        let buttons = layout.buttons(input);
        if layout.builtin {
            return theme::mono().child(button_label(buttons)).into_any_element();
        }
        let entity = cx.entity();
        Select::new(
            SharedString::from(format!("ps-pad-{input}")),
            BUTTONS
                .iter()
                .map(|(name, label)| (SharedString::from(*name), SharedString::from(t!(*label))))
                .chain(buttons.contains(',').then(|| {
                    (
                        SharedString::from(buttons.to_owned()),
                        SharedString::from(button_label(buttons)),
                    )
                })),
            buttons.to_owned(),
        )
        .style(SelectStyle::Compact)
        .on_change(move |value, _, cx| {
            let value = value.to_string();
            entity.update(cx, |this, cx| this.assign(input, &value, cx));
        })
        .into_any_element()
    }

    /// One row per PS4 input.
    fn table(layout: &PadLayout, cx: &Context<Self>) -> AnyElement {
        let count = INPUTS.len();
        theme::card()
            .children(INPUTS.iter().enumerate().map(|(index, &(input, _, label, action))| {
                theme::compact_row(index + 1 == count)
                    .gap_4()
                    .py(px(6.))
                    .child(theme::mono().w(px(100.)).flex_none().child(t!(label)))
                    .child(div().flex_1().min_w_0().truncate().child(t!(action)))
                    .child(Self::binding_control(layout, input, cx))
            }))
            .into_any_element()
    }

    fn layout_select(layouts: &[PadLayout], current: Option<&PadLayout>, cx: &Context<Self>) -> Select {
        let entity = cx.entity();
        let selected = current.map_or(CUSTOM.to_owned(), |layout| layout.id.clone());
        let options = layouts
            .iter()
            .map(|layout| {
                (
                    SharedString::from(layout.id.clone()),
                    SharedString::from(layout_name(&layout.id, layout.builtin, &layout.name).into_owned()),
                )
            })
            .chain(
                current
                    .is_none()
                    .then(|| (SharedString::from(CUSTOM), SharedString::from(t!("ps.layouts.custom")))),
            )
            .collect::<Vec<_>>();
        Select::new("ps-pad-layout", options, selected)
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| {
                let id = value.to_string();
                entity.update(cx, |this, cx| this.choose(&id, cx));
            })
    }

    fn layout_actions(&self, own: bool, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                button("ps-pad-copy", ButtonKind::Secondary, t!("ps.layouts.copy"), false)
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
            .when(own, |actions| {
                actions.child(div().w(px(220.)).child(self.name.clone())).child(
                    button("ps-pad-remove", ButtonKind::Secondary, t!("ps.layouts.remove"), false)
                        .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                )
            })
    }
}

/// `back,touchpad` as `Select / Back + Touchpad`.
fn button_label(buttons: &str) -> String {
    buttons
        .split(',')
        .map(|name| {
            BUTTONS
                .iter()
                .find(|(known, _)| *known == name)
                .map_or_else(|| name.into(), |(_, label)| t!(*label))
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn setting_row(label: AnyElement, control: AnyElement, last: bool) -> Div {
    theme::compact_row(last)
        .child(div().flex_1().min_w_0().child(label))
        .child(control)
}

impl Render for GamepadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loaded = match &self.loaded {
            None => return theme::muted(t!("ps.gamepad.reading")).into_any_element(),
            Some(Err(err)) => return theme::error(err.clone()).into_any_element(),
            Some(Ok(loaded)) => loaded,
        };
        let layouts = loaded.store.pad_layouts();
        let current = loaded.layout();
        let shown = current.clone().unwrap_or_else(|| loaded.custom.clone());
        let values = loaded.values.clone();

        let layout_select = Self::layout_select(&layouts, current.as_ref(), cx);
        let own = current.as_ref().is_some_and(|layout| !layout.builtin);
        let actions = self.layout_actions(own, cx);
        let hint = if own {
            t!("ps.gamepad.hint.own")
        } else if current.is_some() {
            t!("ps.layouts.hint_builtin")
        } else {
            t!("ps.gamepad.hint.custom")
        };
        let bindings = match self.mode {
            Mode::List => Self::table(&shown, cx),
            Mode::Visual => self.visual(&shown, &values, cx),
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_4()
                    .child(theme::muted(t!("ps.gamepad.description")).flex_1().min_w_0())
                    .child(self.mode_switch(cx)),
            )
            .children(self.error.clone().map(theme::error))
            .child(
                theme::card()
                    .child(setting_row(
                        div()
                            .flex()
                            .flex_col()
                            .child(t!("ps.gamepad.controller.title"))
                            .child(theme::muted(t!("ps.gamepad.controller.note")).text_size(px(12.)))
                            .into_any_element(),
                        self.gamepad_select(&values, cx),
                        false,
                    ))
                    .child(setting_row(
                        div().child(t!("ps.layouts.layout")).into_any_element(),
                        layout_select.into_any_element(),
                        false,
                    ))
                    .child(setting_row(
                        div()
                            .flex()
                            .flex_col()
                            .child(t!("ps.gamepad.icons.title"))
                            .child(theme::muted(t!("ps.gamepad.icons.note")).text_size(px(12.)))
                            .into_any_element(),
                        self.icons_select(&values, cx),
                        true,
                    )),
            )
            .child(actions)
            .child(theme::muted(hint).text_size(px(12.)))
            .child(bindings)
            .into_any_element()
    }
}
