//! Teclado: one game's keyboard and mouse bindings, captured by pressing
//! them, kept as named layouts. Any layout can be copied into a new
//! editable one; the one picked is the one in the port's file. The controller is the Mando tab's, for bbport; AnyPS5 reads it
//! as it is.

use std::path::PathBuf;

use gpui::{
    Context, Entity, FocusHandle, KeyDownEvent, ModifiersChangedEvent, MouseButton as GpuiMouse, MouseDownEvent,
    NavigationDirection, ScrollDelta, ScrollWheelEvent, SharedString, Window, div, prelude::*, px, rgb,
};

use decanter_ps::keys::sdl_key_name;
use decanter_ps::layouts::{self, KeyLayout, Store};
use decanter_ps::mapping::{Action, Mapping, MouseButton, Source, Target, Wheel};

use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, Select, TextInput, button, select::SelectStyle};

const GROUPS: [(&str, &[Action]); 5] = [
    (
        "Botones",
        &[
            Action::Cross,
            Action::Circle,
            Action::Square,
            Action::Triangle,
            Action::L1,
            Action::R1,
            Action::L2,
            Action::R2,
            Action::L3,
            Action::R3,
            Action::Options,
        ],
    ),
    ("Cruceta", &[Action::Up, Action::Down, Action::Left, Action::Right]),
    (
        "Stick izquierdo",
        &[
            Action::LeftStickUp,
            Action::LeftStickDown,
            Action::LeftStickLeft,
            Action::LeftStickRight,
        ],
    ),
    (
        "Stick derecho",
        &[
            Action::RightStickUp,
            Action::RightStickDown,
            Action::RightStickLeft,
            Action::RightStickRight,
        ],
    ),
    (
        "Táctil y ventana",
        &[
            Action::TouchLeft,
            Action::TouchRight,
            Action::ToggleMouse,
            Action::ToggleFullscreen,
        ],
    ),
];

fn label(action: Action) -> &'static str {
    match action {
        Action::Cross => "✕  Cruz",
        Action::Circle => "○  Círculo",
        Action::Square => "□  Cuadrado",
        Action::Triangle => "△  Triángulo",
        Action::Options => "Options",
        Action::Up | Action::LeftStickUp | Action::RightStickUp => "Arriba",
        Action::Down | Action::LeftStickDown | Action::RightStickDown => "Abajo",
        Action::Left | Action::LeftStickLeft | Action::RightStickLeft => "Izquierda",
        Action::Right | Action::LeftStickRight | Action::RightStickRight => "Derecha",
        Action::TouchLeft => "Táctil izquierdo",
        Action::TouchRight => "Táctil derecho",
        Action::ToggleMouse => "Capturar ratón",
        Action::ToggleFullscreen => "Pantalla completa",
        other => other.name(),
    }
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Key(name) => name.clone(),
        Source::Mouse(button) => format!("Ratón {button:?}"),
        Source::Wheel(Wheel::Up) => "Rueda ↑".into(),
        Source::Wheel(Wheel::Down) => "Rueda ↓".into(),
    }
}

/// The layout select's value while the file matches no layout.
const CUSTOM: &str = "custom";

pub struct KeyboardView {
    target: Target,
    path: PathBuf,
    defaults: Mapping,
    mapping: Mapping,
    saved: Mapping,
    store: Store,
    /// The id of the layout in the port's file; `None` when it matches none.
    current: Option<String>,
    name: Entity<TextInput>,
    /// The action waiting for its next key, mouse button or wheel step.
    capturing: Option<Action>,
    status: Option<(SharedString, bool)>,
    focus: FocusHandle,
}

impl KeyboardView {
    pub fn new(target: Target, path: PathBuf, mapping: Mapping, store: Store, cx: &mut Context<Self>) -> Self {
        let current = layouts::current_keys(target, &mapping, &store.key_layouts(target));
        let name = cx.new(|cx| TextInput::new("Nombre del layout", cx));
        cx.subscribe(&name, |this, name, event, cx| {
            if matches!(event, InputEvent::Submit) {
                let text = name.read(cx).text().trim().to_owned();
                this.rename(text, cx);
            }
        })
        .detach();
        let mut this = Self {
            target,
            path,
            defaults: target.defaults(),
            saved: mapping.clone(),
            mapping,
            store,
            current,
            name,
            capturing: None,
            status: None,
            focus: cx.focus_handle(),
        };
        this.sync_name(cx);
        this
    }

    fn layout(&self) -> Option<KeyLayout> {
        let id = self.current.as_deref()?;
        self.store
            .key_layouts(self.target)
            .into_iter()
            .find(|layout| layout.id == id)
    }

    /// Only the user's own layouts change; the shipped ones are copied first.
    fn editable(&self) -> bool {
        self.layout().is_some_and(|layout| !layout.builtin)
    }

    fn sync_name(&mut self, cx: &mut Context<Self>) {
        let name = self.layout().map(|layout| layout.name).unwrap_or_default();
        self.name.update(cx, |input, cx| input.set_text(name, cx));
    }

    /// Writes `self.mapping` into the port's file, and the store first when
    /// it changed; both off the UI thread.
    fn persist(&mut self, store_changed: bool, cx: &mut Context<Self>) {
        let (target, path, mapping) = (self.target, self.path.clone(), self.mapping.clone());
        let store = store_changed.then(|| self.store.clone());
        let task = cx.background_spawn(async move {
            if let Some(store) = store
                && let Err(err) = store.save()
            {
                return Err(err);
            }
            decanter_ps::binds::save(target, &path, &mapping).map(|()| mapping)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.status = Some(match result {
                    Ok(saved) => {
                        this.saved = saved;
                        ("Guardado".into(), false)
                    }
                    Err(err) => (
                        format!("No se pudo guardar {}: {err}", this.path.display()).into(),
                        true,
                    ),
                });
                cx.notify();
            });
        })
        .detach();
        self.sync_name(cx);
        cx.notify();
    }

    fn choose(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(layout) = self
            .store
            .key_layouts(self.target)
            .into_iter()
            .find(|layout| layout.id == id)
        else {
            return;
        };
        self.mapping = layout.mapping(self.target);
        self.current = Some(layout.id);
        self.capturing = None;
        self.persist(false, cx);
    }

    /// A new editable layout from what is shown, edits not yet saved
    /// included, so nothing typed so far is lost.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let mut source = self.layout().unwrap_or_else(|| KeyLayout {
            id: String::new(),
            name: "Personalizado".to_owned(),
            text: String::new(),
            builtin: true,
        });
        source.set_mapping(self.target, &self.mapping);
        let copy = self.store.copy_keys(self.target, &source);
        self.current = Some(copy.id);
        self.persist(true, cx);
    }

    fn rename(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(mut layout) = self.layout().filter(|layout| !layout.builtin && !name.is_empty()) else {
            return;
        };
        layout.name = name;
        self.store.update_keys(self.target, &layout);
        self.persist(true, cx);
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(layout) = self.layout().filter(|layout| !layout.builtin) else {
            return;
        };
        self.store.remove_keys(self.target, &layout.id);
        self.current = Some("default".to_owned());
        self.mapping = self.defaults.clone();
        self.persist(true, cx);
    }

    fn capture(&mut self, source: Source, cx: &mut Context<Self>) {
        let Some(action) = self.capturing.take() else { return };
        match self.target.check(action, &source) {
            Ok(()) => {
                self.mapping.add(action, source);
                self.status = None;
            }
            Err(reason) => self.status = Some((reason.into(), true)),
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.capturing.is_none() || event.is_held {
            return;
        }
        match sdl_key_name(&event.keystroke.key) {
            Some(name) => self.capture(Source::Key(name), cx),
            None => {
                let key = &event.keystroke.key;
                self.status = Some((
                    format!("La tecla '{key}' no tiene nombre SDL fiable; probá otra").into(),
                    true,
                ));
                cx.notify();
            }
        }
    }

    /// Modifier keys never arrive as key-down events, only as modifier changes. GPUI does not tell
    /// left from right, so the left key is bound.
    fn on_modifiers(&mut self, event: &ModifiersChangedEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.capturing.is_none() {
            return;
        }
        let m = event.modifiers;
        let name = if m.shift {
            "Left Shift"
        } else if m.control {
            "Left Ctrl"
        } else if m.alt {
            "Left Alt"
        } else if m.platform {
            "Left GUI"
        } else {
            return;
        };
        self.capture(Source::Key(name.into()), cx);
    }

    fn on_mouse(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let button = match event.button {
            GpuiMouse::Left => MouseButton::Left,
            GpuiMouse::Middle => MouseButton::Middle,
            GpuiMouse::Right => MouseButton::Right,
            GpuiMouse::Navigate(NavigationDirection::Back) => MouseButton::X1,
            GpuiMouse::Navigate(NavigationDirection::Forward) => MouseButton::X2,
        };
        self.capture(Source::Mouse(button), cx);
    }

    fn on_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let y = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
        };
        if y != 0.0 {
            self.capture(Source::Wheel(if y > 0.0 { Wheel::Up } else { Wheel::Down }), cx);
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(mut layout) = self.layout().filter(|layout| !layout.builtin) else {
            return;
        };
        layout.set_mapping(self.target, &self.mapping);
        self.store.update_keys(self.target, &layout);
        self.persist(true, cx);
    }

    fn row(&self, action: Action, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sources = self.mapping.get(action);
        let changed = sources != self.defaults.get(action);
        let editable = self.editable();
        let removable = editable && (self.target.allows_unbound() || sources.len() > 1);
        let capturing = self.capturing == Some(action);
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py(px(6.))
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .when(capturing, |row| row.bg(rgb(theme::BG_PANEL_LIGHT)))
            .child(
                div()
                    .w(px(170.))
                    .text_color(rgb(if changed {
                        theme::ACCENT_GOLD
                    } else {
                        theme::TEXT_PRIMARY
                    }))
                    .child(label(action)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .flex_wrap()
                    .gap_1()
                    .when(sources.is_empty(), |chips| {
                        chips.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(theme::TEXT_DIM))
                                .child("Sin asignar"),
                        )
                    })
                    .children(sources.iter().enumerate().map(|(i, source)| {
                        div()
                            .id(SharedString::from(format!("{}-{i}", action.name())))
                            .px_2()
                            .py(px(2.))
                            .rounded(px(theme::RADIUS_SM))
                            .bg(rgb(theme::BG_PANEL_3))
                            .border_1()
                            .border_color(rgb(theme::LINE))
                            .font_family(theme::MONO)
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(source_label(source))
                            .when(removable, |chip| {
                                chip.cursor_pointer()
                                    .hover(|chip| {
                                        chip.border_color(rgb(theme::ERROR_TEXT))
                                            .text_color(rgb(theme::ERROR_TEXT))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.mapping.remove(this.target, action, i);
                                        cx.notify();
                                    }))
                            })
                    })),
            )
            .when(editable, |row| {
                row.child(
                    button(
                        SharedString::from(format!("add-{}", action.name())),
                        ButtonKind::Secondary,
                        "+ Asignar",
                        false,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.capturing = Some(action);
                        this.status = None;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    })),
                )
                .child(
                    button(
                        SharedString::from(format!("reset-{}", action.name())),
                        ButtonKind::Secondary,
                        "↺",
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mapping.reset(action, &this.defaults);
                        cx.notify();
                    })),
                )
            })
    }
}

impl Render for KeyboardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = self.mapping != self.saved;
        let editable = self.editable();
        let layouts = self.store.key_layouts(self.target);
        let entity = cx.entity();
        let selected = self.current.clone().unwrap_or_else(|| CUSTOM.to_owned());
        let options: Vec<(SharedString, SharedString)> = layouts
            .iter()
            .map(|layout| {
                (
                    SharedString::from(layout.id.clone()),
                    SharedString::from(layout.name.clone()),
                )
            })
            .chain(
                self.current
                    .is_none()
                    .then(|| (SharedString::from(CUSTOM), SharedString::from("Personalizado"))),
            )
            .collect();
        let layout_bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child("Layout")
            .child(
                Select::new("ps-key-layout", options, selected)
                    .style(SelectStyle::Compact)
                    .on_change(move |value, _, cx| {
                        let id = value.to_string();
                        entity.update(cx, |this, cx| this.choose(&id, cx));
                    }),
            )
            .child(
                button("ps-key-copy", ButtonKind::Secondary, "Copiar como nuevo", false)
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
            .when(editable, |bar| {
                bar.child(div().w(px(220.)).child(self.name.clone())).child(
                    button("ps-key-remove", ButtonKind::Secondary, "Borrar layout", false)
                        .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                )
            });
        let hint = if editable {
            "Tu layout: + Asignar y pulsá la tecla; Guardar lo escribe. Enter guarda el nombre."
        } else if self.current.is_some() {
            "Los layouts de fábrica no se editan: copialo como nuevo para cambiarlo."
        } else {
            "El archivo tiene teclas editadas a mano; copialas como nuevo para guardarlas como layout."
        };
        let status = self.status.clone();
        let mut groups = div().flex().flex_col().gap_5();
        for (title, actions) in GROUPS {
            groups = groups.child(
                div().flex().flex_col().gap_2().child(theme::h3(title)).child(
                    div()
                        .bg(rgb(theme::BG_PANEL_2))
                        .border_1()
                        .border_color(rgb(theme::LINE))
                        .rounded(px(theme::RADIUS))
                        .children(
                            actions
                                .iter()
                                .filter(|&&action| self.target.supports(action))
                                .map(|&action| self.row(action, cx)),
                        ),
                ),
            );
        }
        div()
            .id("binds")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_modifiers_changed(cx.listener(Self::on_modifiers))
            .relative()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div().text_color(rgb(theme::TEXT_MUTED)).child(match self.target {
                                    Target::Bbport => {
                                        "Teclado para bbport (líneas key. de bbport.ini). Quitar todas las \
                                         teclas de una acción la deja sin asignar."
                                            .to_owned()
                                    }
                                    Target::AnyPs5 => {
                                        "Teclado y ratón para AnyPS5. El mando funciona siempre sin configurar."
                                            .to_owned()
                                    }
                                }),
                            )
                            .child(
                                div()
                                    .font_family(theme::MONO)
                                    .text_size(px(11.))
                                    .text_color(rgb(theme::TEXT_DIM))
                                    .child(self.path.display().to_string()),
                            ),
                    )
                    .children(status.map(|(text, is_error)| {
                        div()
                            .text_color(rgb(if is_error {
                                theme::ERROR_TEXT
                            } else {
                                theme::ACCENT_GREEN
                            }))
                            .child(text)
                    }))
                    .when(editable, |header| {
                        header
                            .child(
                                button("binds-reset-all", ButtonKind::Secondary, "Restaurar todo", false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.mapping = this.defaults.clone();
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                button(
                                    "binds-save",
                                    ButtonKind::Primary,
                                    if dirty { "Guardar" } else { "Guardado" },
                                    !dirty,
                                )
                                .when(dirty, |b| b.on_click(cx.listener(|this, _, _, cx| this.save(cx)))),
                            )
                    }),
            )
            .child(layout_bar)
            .child(theme::muted(hint).text_size(px(12.)))
            .child(groups)
            // While capturing, an overlay takes every click and wheel step so they bind instead of
            // pressing the buttons underneath; only its Cancel button escapes.
            .when_some(self.capturing, |root, action| {
                root.child(
                    div()
                        .id("capture")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(120.))
                        .bg(theme::alpha(theme::BG_DEEP, 0.85))
                        .on_any_mouse_down(cx.listener(Self::on_mouse))
                        .on_scroll_wheel(cx.listener(Self::on_wheel))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_3()
                                .px_8()
                                .py_6()
                                .bg(rgb(theme::BG_PANEL))
                                .border_1()
                                .border_color(rgb(theme::ACCENT_BLUE_DIM))
                                .rounded(px(theme::RADIUS))
                                .child(theme::h3(label(action)))
                                .child(
                                    div()
                                        .text_color(rgb(theme::TEXT_MUTED))
                                        .child("Pulsá una tecla, un botón del ratón o mové la rueda"),
                                )
                                .child(
                                    button("binds-cancel", ButtonKind::Secondary, "Cancelar", false).on_mouse_down(
                                        GpuiMouse::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.capturing = None;
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        ),
                )
            })
    }
}
