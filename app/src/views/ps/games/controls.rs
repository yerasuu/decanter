//! Controles: one game's keyboard and mouse bindings, captured by pressing
//! them. A controller needs no setup; both ports read it directly.

use std::path::PathBuf;

use gpui::{
    Context, FocusHandle, KeyDownEvent, ModifiersChangedEvent, MouseButton as GpuiMouse, MouseDownEvent,
    NavigationDirection, ScrollDelta, ScrollWheelEvent, SharedString, Window, div, prelude::*, px, rgb,
};

use decanter_ps::keys::sdl_key_name;
use decanter_ps::mapping::{Action, Mapping, MouseButton, Source, Target, Wheel};

use crate::theme;
use crate::widgets::{ButtonKind, button};

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

pub struct ControlsView {
    target: Target,
    path: PathBuf,
    defaults: Mapping,
    mapping: Mapping,
    saved: Mapping,
    /// The action waiting for its next key, mouse button or wheel step.
    capturing: Option<Action>,
    status: Option<(SharedString, bool)>,
    focus: FocusHandle,
}

impl ControlsView {
    pub fn new(target: Target, path: PathBuf, mapping: Mapping, cx: &mut Context<Self>) -> Self {
        Self {
            target,
            path,
            defaults: target.defaults(),
            saved: mapping.clone(),
            mapping,
            capturing: None,
            status: None,
            focus: cx.focus_handle(),
        }
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
        let (target, path, mapping) = (self.target, self.path.clone(), self.mapping.clone());
        let task =
            cx.background_spawn(async move { decanter_ps::binds::save(target, &path, &mapping).map(|()| mapping) });
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
    }

    fn row(&self, action: Action, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sources = self.mapping.get(action);
        let changed = sources != self.defaults.get(action);
        let removable = self.target.allows_unbound() || sources.len() > 1;
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
            .child(
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
    }
}

impl Render for ControlsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = self.mapping != self.saved;
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
                    ),
            )
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
