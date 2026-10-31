//! The display decanter's windows open on, in Ajustes › General: gpui's
//! displays by uuid where gpui places windows itself, KDE's outputs by
//! connector name in a KDE Wayland session, where KWin does it instead
//! (see [`crate::platform::place_windows_on`]).

use gpui::{Context, Div, IntoElement, SharedString, Window, prelude::*};
use rust_i18n::t;

use super::{column, field_row};
use crate::platform::{self, KwinOutput};
use crate::theme;
use crate::widgets::Select;

const AUTOMATIC: &str = "auto";

pub struct WindowDisplaySection {
    /// The saved display's uuid; `None` lets the system place windows.
    display: Option<String>,
    /// The connector name KWin sends windows to in a KDE Wayland session.
    output: Option<String>,
    /// `None` until listed, and for good outside a KDE Wayland session.
    kwin_outputs: Option<Vec<KwinOutput>>,
}

impl WindowDisplaySection {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let compositor = cx.compositor_name();
        cx.spawn(async move |this, cx| {
            let outputs = platform::kwin_outputs(compositor).await;
            let _ = this.update(cx, |this, cx| {
                this.kwin_outputs = outputs;
                cx.notify();
            });
        })
        .detach();
        let mut this = Self {
            display: None,
            output: None,
            kwin_outputs: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        self.display = crate::core::db()
            .and_then(|db| db.window_display().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read window_display setting, defaulting to automatic"),
            )
            .ok()
            .flatten();
        self.output = crate::core::db()
            .and_then(|db| db.window_output().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read window_output setting, defaulting to automatic"),
            )
            .ok()
            .flatten();
    }

    fn set_display(&mut self, uuid: Option<&str>, cx: &mut Context<Self>) {
        let saved =
            crate::core::db().and_then(|db| db.set_window_display(uuid).map_err(crate::core::StoreError::Query));
        if let Err(err) = saved {
            tracing::warn!(uuid, error = %err, "failed to save window_display setting");
            return;
        }
        tracing::info!(uuid, "window display setting changed");
        self.reload();
        cx.notify();
    }

    /// Unlike the uuid, KWin applies this one at once: its script moves the
    /// windows already open too.
    fn set_output(&mut self, output: Option<&str>, cx: &mut Context<Self>) {
        let saved =
            crate::core::db().and_then(|db| db.set_window_output(output).map_err(crate::core::StoreError::Query));
        if let Err(err) = saved {
            tracing::warn!(output, error = %err, "failed to save window_output setting");
            return;
        }
        tracing::info!(output, "window output setting changed");
        let compositor = cx.compositor_name();
        let output = output.map(str::to_owned);
        cx.spawn(async move |_, _| platform::place_windows_on(compositor, output.as_deref()).await)
            .detach();
        self.reload();
        cx.notify();
    }

    /// One entry per display gpui can name by uuid, numbered in gpui's order.
    fn display_select(&self, cx: &Context<Self>) -> impl IntoElement {
        let primary = cx.primary_display().map(|display| display.id());
        let displays = cx.displays();
        let entries = (1_usize..).zip(&displays).filter_map(|(index, display)| {
            let uuid = display.uuid().ok()?.to_string();
            let size = display.bounds().size;
            let label = t!(
                if primary == Some(display.id()) {
                    "settings.general.window_display.primary"
                } else {
                    "settings.general.window_display.display"
                },
                index = index,
                width = f32::from(size.width).round(),
                height = f32::from(size.height).round()
            );
            Some((uuid.into(), label.into()))
        });
        let entity = cx.entity();
        select(
            "window-display",
            displays.len(),
            entries,
            self.display.as_deref(),
            move |uuid, cx| entity.update(cx, |this, cx| this.set_display(uuid, cx)),
        )
    }

    /// KDE's own list, by connector name.
    fn output_select(&self, outputs: &[KwinOutput], cx: &Context<Self>) -> impl IntoElement {
        let entries = (1_usize..).zip(outputs).map(|(index, output)| {
            let label = t!(
                if output.is_primary {
                    "settings.general.window_display.kde_primary"
                } else {
                    "settings.general.window_display.kde_display"
                },
                index = index,
                width = output.width,
                height = output.height,
                name = output.name.as_str()
            );
            (output.name.as_str().to_owned().into(), label.into())
        });
        let entity = cx.entity();
        select(
            "window-output",
            outputs.len(),
            entries,
            self.output.as_deref(),
            move |output, cx| entity.update(cx, |this, cx| this.set_output(output, cx)),
        )
    }
}

impl Render for WindowDisplaySection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(outputs) = &self.kwin_outputs {
            return column()
                .child(self.output_select(outputs, cx))
                .child(theme::muted(t!("settings.general.window_display.note_kde")));
        }
        let notes = if cx.compositor_name() == "Wayland" {
            column()
                .child(theme::muted(t!("settings.general.window_display.note_wayland")))
                .child(theme::muted(t!("settings.general.window_display.note_wayland_rules")))
        } else {
            column().child(theme::muted(t!("settings.general.window_display.note")))
        };
        column().child(self.display_select(cx)).child(notes)
    }
}

/// Automática first, then `entries`, then the saved value while it's not
/// among them (unplugged), so the picker still shows what is saved.
fn select(
    id: &'static str,
    count: usize,
    entries: impl Iterator<Item = (SharedString, SharedString)>,
    saved: Option<&str>,
    on_change: impl Fn(Option<&str>, &mut gpui::App) + 'static,
) -> Div {
    let mut options = Vec::with_capacity(count + 2);
    options.push((AUTOMATIC.into(), t!("settings.general.window_display.auto").into()));
    options.extend(entries);
    if let Some(saved) = saved
        && !options.iter().any(|(value, _)| value == saved)
    {
        options.push((
            saved.to_owned().into(),
            t!("settings.general.window_display.missing").into(),
        ));
    }
    field_row().child(
        Select::new(id, options, saved.unwrap_or(AUTOMATIC)).on_change(move |value, _, cx| {
            on_change((value != AUTOMATIC).then_some(value.as_ref()), cx);
        }),
    )
}
