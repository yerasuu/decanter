//! Mando: which controller bbport reads and which configuration its
//! buttons follow ([`config`]). The bindings show as a list or on a drawing
//! of the pad ([`visual`]), whichever the user keeps: the list writes an
//! edit to the game's port ini at once, the drawing stages edits until they are
//! applied ([`staged`]). Everything applies on the next start.

mod config;
mod staged;
mod visual;

use decanter_core::capabilities::Feature;
use decanter_ps::bbport::pad::{self, BUTTONS, INPUTS, MENU, MENU_CHORDS, PadLayout};
use decanter_ps::bbport::per_game::DEFAULTS;
use decanter_ps::bbport::values::{self, Values};
use decanter_ps::bbport::{Bbport, Control, GROUPS, Setting};
use decanter_ps::layouts::Store;
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Div, Entity, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;
use staged::Scope;

use super::{layout_name, scope_note};
use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::state::Ps;
use crate::theme;
use crate::widgets::{InputEvent, Select, TextInput, select::SelectStyle, tooltip};

/// The layout select's value while the port ini holds `pad.` lines no
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

/// One title's: [`Values::ini_path`] is the file its layout is written to.
struct Loaded {
    store: Store,
    /// The id of the layout in the port ini; `None` when it matches none.
    current: Option<String>,
    /// The port ini's own `pad.` lines, for copying when they match none.
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
    /// The shown title, whose settings `loaded` holds once read.
    title_id: Option<String>,
    loaded: Option<Result<Loaded, Text>>,
    /// Connected controllers `(GUID, name)`, read when the view opens.
    gamepads: Vec<(String, String)>,
    name: Entity<TextInput>,
    error: Option<Text>,
    mode: Mode,
    /// The input picked on the drawing, whose binding the editor under it changes.
    selected: Option<&'static str>,
    /// The drawing's edits not yet applied; `None` when there are none.
    draft: Option<PadLayout>,
    scope: Scope,
}

fn gamepad_setting() -> Option<&'static Setting> {
    GROUPS
        .iter()
        .flat_map(|group| group.settings)
        .find(|setting| matches!(setting.control, Control::Gamepad))
}

impl GamepadView {
    pub fn new(ps: &Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(Text::new("ps.layouts.name_placeholder"), cx));
        cx.subscribe(&name, |this, name, event, cx| {
            if matches!(event, InputEvent::Submit) {
                let text = name.read(cx).text().trim().to_owned();
                this.rename(text, cx);
            }
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
            title_id: None,
            loaded: None,
            gamepads: Vec::new(),
            name,
            error: None,
            mode: Mode::List,
            selected: None,
            draft: None,
            scope: Scope::Game,
        }
    }

    /// The defaults new games copy instead of a title's settings, for the defaults page; shown under
    /// [`DEFAULTS`] where a title id goes.
    pub fn show_defaults(&mut self, cx: &mut Context<Self>) {
        self.show_title(DEFAULTS, cx);
    }

    /// Reads `title_id`'s settings again, since its in-game menu writes the same file. Another
    /// title's go at once, with the drawing's unapplied edits, so none is shown or written for the
    /// wrong one.
    pub fn show_title(&mut self, title_id: &str, cx: &mut Context<Self>) {
        if self.title_id.as_deref() != Some(title_id) {
            self.title_id = Some(title_id.to_owned());
            self.loaded = None;
            self.draft = None;
            self.selected = None;
        }
        let title_id = title_id.to_owned();
        let load = cx.background_spawn({
            let title_id = title_id.clone();
            async move {
                let store = match Store::load() {
                    Ok(store) => store,
                    Err(err) => return Err(Text::new("ps.layouts.read_error").arg("error", err)),
                };
                let root = decanter_ps::paths::active();
                let values = if title_id == DEFAULTS {
                    Values::load_defaults(&root)
                } else {
                    Values::load(&root, &title_id)
                };
                let values = match values {
                    Ok(values) => values,
                    Err(err) => return Err(Text::new("ps.scope.read_error").arg("error", err)),
                };
                Ok(Loaded {
                    current: pad::current(values.ini_path(), &store.pad_layouts()),
                    custom: pad::from_ini(values.ini_path()),
                    values,
                    store,
                })
            }
        });
        cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                if this.title_id.as_deref() != Some(title_id.as_str()) {
                    return;
                }
                this.loaded = Some(loaded);
                this.sync_name(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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

    /// `Lista` / `Visual`: two ways to show the same bindings. Locked while
    /// the drawing holds unapplied edits, since the list writes at once and
    /// would write under them.
    fn mode_switch(&self, cx: &Context<Self>) -> Div {
        let locked = self.draft.is_some();
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
                        .when(!active && !locked, |item| {
                            item.hover(|item| item.text_color(rgb(theme::TEXT_BRIGHT)))
                                .on_click(cx.listener(move |this, _, _, cx| this.choose_mode(mode, cx)))
                        })
                        .when(!active && locked, |item| {
                            item.opacity(0.4)
                                .cursor_not_allowed()
                                .tooltip(tooltip(t!("ps.gamepad.visual.locked")))
                        })
                        .child(SharedString::from(label))
                }),
            )
    }

    const fn loaded(&self) -> Option<&Loaded> {
        match &self.loaded {
            Some(Ok(loaded)) => Some(loaded),
            _ => None,
        }
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

    /// Makes `layout` the one in use: written into the shown title's port ini, and the
    /// store saved first when it changed. Both off the UI thread.
    fn apply(&mut self, layout: PadLayout, store_changed: bool, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        loaded.current = Some(layout.id.clone());
        let store = store_changed.then(|| loaded.store.clone());
        let ini = loaded.values.ini_path().to_owned();
        let task = cx.background_spawn(async move {
            if let Some(store) = store
                && let Err(err) = store.save()
            {
                return Err(err);
            }
            layout.save_to_ini(&ini)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("ps.gamepad.save_error").arg("error", err));
                cx.notify();
            });
        })
        .detach();
        self.sync_name(cx);
        cx.notify();
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
        let title_id = loaded.values.title_id().to_owned();
        let is_defaults = loaded.values.is_defaults();
        let task = cx.background_spawn(async move {
            let root = decanter_ps::paths::active();
            let change =
                |settings: &mut decanter_ps::bbport::LauncherSettings| settings.set_text("button_icons", &style);
            if is_defaults {
                decanter_ps::bbport::LauncherSettings::edit_defaults(&root, change)
            } else {
                decanter_ps::bbport::LauncherSettings::edit_title(&root, &title_id, change)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("ps.gamepad.save_error").arg("error", err));
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
        let ini = loaded.values.ini_path().to_owned();
        let title_id = loaded.values.title_id().to_owned();
        let is_defaults = loaded.values.is_defaults();
        let name = self
            .gamepads
            .iter()
            .find(|(known, _)| *known == guid)
            .map_or_else(String::new, |(_, name)| name.clone());
        let task = cx.background_spawn(async move {
            let root = decanter_ps::paths::active();
            if is_defaults {
                values::save_default(&root, setting, &guid, Some(&name))
            } else {
                values::save(&root, &title_id, &ini, setting, &guid, Some(&name))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("ps.gamepad.save_error").arg("error", err));
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

    /// `input`'s button, picked from a list when `editable`, and handed to
    /// `pick` (written at once, or staged). A pad can't be read from here
    /// to capture a press, so it is picked rather than pressed. The port's
    /// menu also offers its chords.
    fn binding_control(
        layout: &PadLayout,
        input: &'static str,
        editable: bool,
        pick: fn(&mut Self, &str, &str, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> AnyElement {
        let buttons = layout.buttons(input);
        if !editable {
            return theme::mono().child(button_label(buttons)).into_any_element();
        }
        let entity = cx.entity();
        let choices = MENU_CHORDS
            .iter()
            .filter(move |_| input == MENU.0)
            .chain(BUTTONS.iter());
        let known = choices.clone().any(|(name, _)| *name == buttons);
        Select::new(
            SharedString::from(format!("ps-pad-{input}")),
            choices
                .map(|(name, label)| (SharedString::from(*name), SharedString::from(t!(*label))))
                .chain((!known).then(|| {
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
            entity.update(cx, |this, cx| pick(this, input, &value, cx));
        })
        .into_any_element()
    }

    /// One row per PS4 input, and the port's menu last for a port build that reads its binding.
    fn table(layout: &PadLayout, cx: &Context<Self>) -> AnyElement {
        let menu = Caps::get(cx).is_offered(Feature::PsMenuBinding).then_some(&MENU);
        let count = INPUTS.len().saturating_add(usize::from(menu.is_some()));
        theme::card()
            .children(
                INPUTS
                    .iter()
                    .chain(menu)
                    .enumerate()
                    .map(|(index, &(input, _, label, action))| {
                        theme::compact_row(index + 1 == count)
                            .gap_4()
                            .py(px(6.))
                            .child(theme::mono().w(px(100.)).flex_none().child(t!(label)))
                            .child(div().flex_1().min_w_0().truncate().child(t!(action)))
                            .child(Self::binding_control(layout, input, !layout.builtin, Self::assign, cx))
                    }),
            )
            .into_any_element()
    }
}

/// `back,touchpad` as `Select / Back + Touchpad`.
fn button_label(buttons: &str) -> String {
    buttons
        .split(',')
        .map(|name| {
            BUTTONS
                .iter()
                .chain(&MENU_CHORDS)
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
            Some(Err(err)) => return theme::error(err).into_any_element(),
            Some(Ok(loaded)) => loaded,
        };
        let layouts = loaded.store.pad_layouts();
        let current = loaded.layout();
        let shown = current.clone().unwrap_or_else(|| loaded.custom.clone());
        let values = loaded.values.clone();

        let layout_select = self.layout_select(&layouts, current.as_ref(), cx);
        let own = current.as_ref().is_some_and(|layout| !layout.builtin);
        let actions = self.layout_actions(own, cx);
        // The drawing says how its own staged edits land, so this hint is the list's.
        let hint = if own {
            t!("ps.gamepad.hint.own")
        } else if current.is_some() {
            t!("ps.layouts.hint_builtin")
        } else {
            t!("ps.gamepad.hint.custom")
        };
        let list = self.mode == Mode::List;
        let bindings = if list {
            Self::table(&shown, cx)
        } else {
            self.visual(&values, cx)
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
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(theme::muted(t!("ps.gamepad.description")))
                            .child(theme::muted(if values.is_defaults() {
                                t!("ps.defaults.graphics_note")
                            } else {
                                scope_note(values.scope())
                            })),
                    )
                    .child(self.mode_switch(cx)),
            )
            .children(self.error.as_ref().map(theme::error))
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
                        layout_select,
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
            .when(list, |view| view.child(theme::muted(hint).text_size(px(12.))))
            .child(bindings)
            .into_any_element()
    }
}
