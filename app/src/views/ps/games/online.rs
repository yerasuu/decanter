//! Online: the title's co-op extension, when the port declares one for it (`psport
//! --online-overrides`), with its switch and where its game server runs. Online play itself, its
//! servers and the account are in the PS section's Ajustes.

use decanter_core::capabilities::Feature;
use decanter_ps::bbport::online::{self, LocalServer};
use decanter_ps::bbport::{Bbport, LauncherSettings};
use decanter_ps::port::Port;
use gpui::{Context, Entity, SharedString, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::capabilities::Caps;
use crate::state::Ps;
use crate::theme;
use crate::widgets::{Select, checkbox, select::SelectStyle};

/// The title's co-op extension, as far as the port has said.
enum Extension {
    Asking,
    Declared(Option<String>),
}

pub struct OnlineTitleView {
    ps: Entity<Ps>,
    title_id: Option<String>,
    extension: Extension,
    is_online: bool,
    is_bbsc: bool,
    local_server: LocalServer,
    error: Option<String>,
}

impl OnlineTitleView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        Self::load_settings(cx);
        Self {
            ps,
            title_id: None,
            extension: Extension::Asking,
            is_online: false,
            is_bbsc: false,
            local_server: LocalServer::Off,
            error: None,
        }
    }

    /// Again at each title shown, since Ajustes may have turned online play on or off meanwhile.
    fn load_settings(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let settings = cx
                .background_spawn(async { LauncherSettings::load(&decanter_ps::paths::active()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.is_online = settings.flag(online::ONLINE, false);
                this.is_bbsc = settings.flag(online::BBSC, false);
                this.local_server = LocalServer::of(&settings);
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks the port about `title_id` once it is built; a port that can't say shows nothing.
    pub fn show_title(&mut self, title_id: &str, cx: &mut Context<Self>) {
        Self::load_settings(cx);
        if self.title_id.as_deref() == Some(title_id) && matches!(self.extension, Extension::Declared(_)) {
            return;
        }
        self.title_id = Some(title_id.to_owned());
        self.extension = Extension::Asking;
        cx.notify();
        let Some(toolchain) = self
            .ps
            .read(cx)
            .port(Port::Bbport)
            .and_then(|state| state.toolchain().cloned())
        else {
            self.extension = Extension::Declared(None);
            return;
        };
        let title_id = title_id.to_owned();
        cx.spawn(async move |this, cx| {
            let bbport = Bbport::new(decanter_ps::paths::active());
            let title_online = online::title_online(&bbport, &toolchain, &title_id).await;
            let extension = title_online.and_then(|title| title.extension().map(str::to_owned));
            let _ = this.update(cx, |this, cx| {
                if this.title_id.as_deref() == Some(title_id.as_str()) {
                    this.extension = Extension::Declared(extension);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save(change: impl FnOnce(&mut LauncherSettings) + Send + 'static, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move { LauncherSettings::edit(&decanter_ps::paths::active(), change) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.error = saved
                    .err()
                    .map(|err| t!("ps.online.save_error", error = err).into_owned());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_bbsc(&mut self, is_on: bool, cx: &mut Context<Self>) {
        self.is_bbsc = is_on;
        Self::save(move |settings| settings.set_flag(online::BBSC, is_on), cx);
    }

    fn set_local_server(&mut self, mode: LocalServer, cx: &mut Context<Self>) {
        self.local_server = mode;
        Self::save(
            move |settings| settings.set_text(online::BBSC_LOCAL_SERVER, mode.value()),
            cx,
        );
    }

    fn bbsc(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let is_bbsc = self.is_bbsc;
        let toggle = div()
            .id("ps-online-bbsc")
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .child(checkbox(is_bbsc))
            .child(SharedString::from(t!("ps.online.bbsc.label")))
            .on_click(cx.listener(move |this, _, _, cx| this.set_bbsc(!is_bbsc, cx)));
        let entity = cx.entity();
        let select = Select::new(
            "ps-online-bbsc-local",
            LocalServer::ALL.map(|mode| {
                let label = match mode {
                    LocalServer::Off => t!("ps.online.bbsc.local.off"),
                    LocalServer::On => t!("ps.online.bbsc.local.on"),
                    LocalServer::Auto => t!("ps.online.bbsc.local.auto"),
                };
                (SharedString::from(mode.value()), SharedString::from(label))
            }),
            self.local_server.value(),
        )
        .style(SelectStyle::Compact)
        .on_change(move |value, _, cx| {
            if let Some(mode) = LocalServer::ALL.into_iter().find(|mode| mode.value() == value.as_ref()) {
                entity.update(cx, |this, cx| this.set_local_server(mode, cx));
            }
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                theme::card().child(theme::compact_row(false).child(toggle)).child(
                    theme::compact_row(true)
                        .child(
                            div()
                                .flex_1()
                                .child(SharedString::from(t!("ps.online.bbsc.local.label"))),
                        )
                        .when(is_bbsc, |row| row.child(select)),
                ),
            )
            .child(theme::muted(t!("ps.online.bbsc.note")))
            .child(theme::muted(t!("ps.online.bbsc.local.note")))
            .into_any_element()
    }
}

impl Render for OnlineTitleView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let caps = Caps::get(cx);
        if let Some(reason) = caps.reason(Feature::PsOnline) {
            return theme::muted(reason).into_any_element();
        }
        let body = match &self.extension {
            Extension::Asking => theme::muted(t!("ps.online.title_querying")).into_any_element(),
            Extension::Declared(Some(extension)) if extension == online::BBSC_EXTENSION => self.bbsc(cx),
            Extension::Declared(_) => theme::muted(t!("ps.online.no_extension")).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(760.))
            .when(!self.is_online, |column| {
                column.child(theme::muted(t!("ps.online.off_in_settings")))
            })
            .child(body)
            .children(self.error.clone().map(theme::error))
            .into_any_element()
    }
}
