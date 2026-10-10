//! Mando: which controller bbport reads and which layout its buttons
//! follow. Any layout can be copied into a new editable one; the one
//! picked is the one in `bbport.ini`, and an edit to it is written there
//! at once. Everything applies on the next start.

use decanter_ps::bbport::pad::{self, BUTTONS, INPUTS, PadLayout};
use decanter_ps::bbport::values::{self, Values};
use decanter_ps::bbport::{Bbport, Control, GROUPS, Setting};
use decanter_ps::layouts::Store;
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, Select, TextInput, button, select::SelectStyle};

/// The layout select's value while `bbport.ini` holds `pad.` lines no
/// layout has.
const CUSTOM: &str = "custom";

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
}

fn gamepad_setting() -> Option<&'static Setting> {
    GROUPS
        .iter()
        .flat_map(|group| group.settings)
        .find(|setting| matches!(setting.control, Control::Gamepad))
}

impl GamepadView {
    pub fn new(ps: &Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new("Nombre del layout", cx));
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
                Err(err) => return Err(format!("No se pudieron leer los layouts: {err}")),
            };
            Ok(Loaded {
                current: pad::current(&store.pad_layouts()),
                custom: pad::from_ini(),
                values: Values::load(),
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
        Self {
            loaded: None,
            gamepads: Vec::new(),
            name,
            error: None,
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
            layout.save_to_ini()
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err().map(|err| format!("No se pudo guardar: {err}"));
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
        let copy = loaded.store.copy_pad(&source);
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
        let current = values.launch().text("button_icons").unwrap_or("auto").to_owned();
        let entity = cx.entity();
        Select::new(
            "ps-button-icons",
            [
                (SharedString::from("pad"), SharedString::from("Del mando (A/B/X/Y)")),
                (
                    SharedString::from("original"),
                    SharedString::from("Originales del juego"),
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
            decanter_ps::bbport::LauncherSettings::edit(|settings| settings.set_text("button_icons", &style))
        });
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
        let task = cx.background_spawn(async move { values::save(setting, &guid, Some(&name)) });
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

    fn gamepad_select(&self, values: &Values, cx: &mut Context<Self>) -> AnyElement {
        let Some(setting) = gamepad_setting() else {
            return div().into_any_element();
        };
        let guid = values.get(setting).unwrap_or_default();
        let unplugged = (!guid.is_empty() && !self.gamepads.iter().any(|(known, _)| *known == guid)).then(|| {
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
        Select::new("ps-gamepad", options, guid)
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| {
                let value = value.to_string();
                entity.update(cx, |this, cx| this.choose_gamepad(value, cx));
            })
            .into_any_element()
    }

    /// One row per PS4 input: its button, picked from a list when the
    /// layout is the user's own. A pad can't be read from here to capture
    /// a press, so it is picked rather than pressed.
    fn table(&self, layout: &PadLayout, cx: &mut Context<Self>) -> AnyElement {
        let count = INPUTS.len();
        theme::card()
            .children(INPUTS.iter().enumerate().map(|(index, &(input, _, label, action))| {
                let buttons = layout.buttons(input);
                let control = if layout.builtin {
                    theme::mono().child(button_label(buttons)).into_any_element()
                } else {
                    let entity = cx.entity();
                    Select::new(
                        SharedString::from(format!("ps-pad-{input}")),
                        BUTTONS
                            .iter()
                            .map(|(name, label)| (SharedString::from(*name), SharedString::from(*label)))
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
                };
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_4()
                    .py(px(6.))
                    .when(index + 1 < count, |row| row.border_b_1().border_color(rgb(theme::LINE)))
                    .child(theme::mono().w(px(100.)).flex_none().child(label))
                    .child(div().flex_1().min_w_0().truncate().child(action))
                    .child(control)
            }))
            .into_any_element()
    }
}

/// `back,touchpad` as `Select / Back + Panel táctil`.
fn button_label(buttons: &str) -> String {
    buttons
        .split(',')
        .map(|name| {
            BUTTONS
                .iter()
                .find(|(known, _)| *known == name)
                .map_or(name, |(_, label)| label)
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

impl Render for GamepadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loaded = match &self.loaded {
            None => return theme::muted("Leyendo la configuración del mando…").into_any_element(),
            Some(Err(err)) => return theme::error(err.clone()).into_any_element(),
            Some(Ok(loaded)) => loaded,
        };
        let layouts = loaded.store.pad_layouts();
        let current = loaded.layout();
        let shown = current.clone().unwrap_or_else(|| loaded.custom.clone());
        let values = loaded.values.clone();

        let entity = cx.entity();
        let selected = current.as_ref().map_or(CUSTOM.to_owned(), |layout| layout.id.clone());
        let options = layouts
            .iter()
            .map(|layout| {
                (
                    SharedString::from(layout.id.clone()),
                    SharedString::from(layout.name.clone()),
                )
            })
            .chain(
                current
                    .is_none()
                    .then(|| (SharedString::from(CUSTOM), SharedString::from("Personalizado"))),
            )
            .collect::<Vec<_>>();
        let layout_select = Select::new("ps-pad-layout", options, selected)
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| {
                let id = value.to_string();
                entity.update(cx, |this, cx| this.choose(&id, cx));
            });
        let own = current.as_ref().is_some_and(|layout| !layout.builtin);
        let row = |label: AnyElement, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .child(div().flex_1().min_w_0().child(label))
                .child(control)
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                button("ps-pad-copy", ButtonKind::Secondary, "Copiar como nuevo", false)
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
            .when(own, |actions| {
                actions.child(div().w(px(220.)).child(self.name.clone())).child(
                    button("ps-pad-remove", ButtonKind::Secondary, "Borrar layout", false)
                        .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                )
            });
        let hint = if own {
            "Tu layout: elegí el botón de cada entrada; si ya lo usaba otra, se intercambian. Enter guarda el nombre."
        } else if current.is_some() {
            "Los layouts de fábrica no se editan: copialo como nuevo para cambiarlo."
        } else {
            "bbport.ini tiene líneas pad. editadas a mano; copialas como nuevo para guardarlas como layout."
        };
        let table = self.table(&shown, cx);
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(theme::muted(
                "Ajustes de bbport, comunes a todas tus copias de Bloodborne. Se aplican en el próximo arranque. \
                 Los sticks mueven al personaje y la cámara en cualquier layout.",
            ))
            .children(self.error.clone().map(theme::error))
            .child(
                theme::card()
                    .child(
                        row(
                            div()
                                .flex()
                                .flex_col()
                                .child("Mando")
                                .child(theme::muted("Con varios conectados, cuál usa el juego").text_size(px(12.)))
                                .into_any_element(),
                            self.gamepad_select(&values, cx),
                        )
                        .border_b_1()
                        .border_color(rgb(theme::LINE)),
                    )
                    .child(
                        row(
                            div().child("Layout").into_any_element(),
                            layout_select.into_any_element(),
                        )
                        .border_b_1()
                        .border_color(rgb(theme::LINE)),
                    )
                    .child(row(
                        div()
                            .flex()
                            .flex_col()
                            .child("Iconos de botones")
                            .child(theme::muted("En menús y avisos; siguen al layout elegido").text_size(px(12.)))
                            .into_any_element(),
                        self.icons_select(&values, cx),
                    )),
            )
            .child(actions)
            .child(theme::muted(hint).text_size(px(12.)))
            .child(table)
            .into_any_element()
    }
}
