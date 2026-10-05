//! Window behaviour that isn't tied to any profile or game, plus the
//! Steam export toggle and its pending-restart callout.

use gpui::{Context, IntoElement, Window, div, prelude::*};

use super::{column, field_row, inline};
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

pub struct GeneralTab {
    close_to_tray: bool,
    steam_export_enabled: bool,
    steam_pending: bool,
    restarting_steam: bool,
    restart_error: Option<String>,
}

impl GeneralTab {
    pub fn new(_: &mut Context<Self>) -> Self {
        let mut this = Self {
            close_to_tray: false,
            steam_export_enabled: true,
            steam_pending: false,
            restarting_steam: false,
            restart_error: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        self.close_to_tray = crate::core::db()
            .and_then(|db| db.close_to_tray().map_err(|err| err.to_string()))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read close_to_tray setting, defaulting to false"),
            )
            .unwrap_or(false);
        self.steam_export_enabled = crate::core::db()
            .and_then(|db| db.steam_export_enabled().map_err(|err| err.to_string()))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read steam_export_enabled setting, defaulting to true"),
            )
            .unwrap_or(true);
        self.steam_pending = crate::core::steam_shortcuts_pending();
    }

    fn set_close_to_tray(&mut self, close_to_tray: bool, cx: &mut Context<Self>) {
        match crate::core::db() {
            Ok(db) => match db.set_close_to_tray(close_to_tray) {
                Ok(()) => {
                    tracing::info!(close_to_tray, "close-to-tray setting changed");
                    self.reload();
                    cx.notify();
                }
                Err(err) => tracing::warn!(close_to_tray, error = %err, "failed to save close_to_tray setting"),
            },
            Err(err) => {
                tracing::warn!(close_to_tray, error = %err, "failed to open db, close_to_tray setting not saved");
            }
        }
    }

    fn set_steam_export(&mut self, enabled: bool, cx: &mut Context<Self>) {
        match crate::core::db() {
            Ok(db) => match db.set_steam_export_enabled(enabled) {
                Ok(()) => {
                    tracing::info!(enabled, "steam export setting changed");
                    self.reload();
                    cx.notify();
                }
                Err(err) => tracing::warn!(enabled, error = %err, "failed to save steam_export_enabled setting"),
            },
            Err(err) => {
                tracing::warn!(enabled, error = %err, "failed to open db, steam_export_enabled setting not saved");
            }
        }
    }

    fn restart_steam(&mut self, cx: &mut Context<Self>) {
        self.restarting_steam = true;
        self.restart_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = crate::core::restart_steam_to_apply_pending().await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => {
                        tracing::info!("steam restarted to apply pending shortcuts");
                        this.reload();
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to restart steam");
                        this.restart_error = Some(err);
                    }
                }
                this.restarting_steam = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// `label.field-row` around a checkbox: the whole row toggles it, the
    /// way clicking a `<label>`'s text does.
    fn checkbox_row(
        id: &'static str,
        checked: bool,
        text: &'static str,
        on_toggle: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        field_row()
            .id(id)
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, !checked, cx)))
            .child(checkbox(checked))
            .child(div().min_w_0().child(text))
    }
}

impl Render for GeneralTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let restarting = self.restarting_steam;
        let callout = self.steam_pending.then(|| {
            column()
                .child(div().child(
                    "Hay juegos exportados a Steam esperando a que se reinicie — Steam estaba abierto cuando se \
                     agregaron, asi que todavia no los pisa su propia copia en memoria.",
                ))
                .child(inline(
                    button(
                        "restart-steam",
                        ButtonKind::Primary,
                        if restarting {
                            "Reiniciando Steam..."
                        } else {
                            "Reiniciar Steam ahora"
                        },
                        restarting,
                    )
                    .when(!restarting, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.restart_steam(cx)))
                    }),
                ))
                .children(self.restart_error.clone().map(theme::error))
        });

        column()
            .child(theme::h2("Al cerrar la ventana"))
            .child(Self::checkbox_row(
                "close-to-tray",
                self.close_to_tray,
                "Dejar Decanter corriendo en la bandeja en vez de cerrarlo",
                Self::set_close_to_tray,
                cx,
            ))
            .child(theme::muted(
                "Requiere reiniciar Decanter para que el cambio tenga efecto.",
            ))
            .child(theme::h2("Exportar a Steam"))
            .child(Self::checkbox_row(
                "steam-export",
                self.steam_export_enabled,
                "Agregar cada juego instalado a la biblioteca real de Steam, con su portada y banner",
                Self::set_steam_export,
                cx,
            ))
            .child(theme::muted(
                "Hay que reiniciar Steam para ver los cambios — Steam solo lee sus shortcuts al arrancar.",
            ))
            .children(callout)
    }
}
