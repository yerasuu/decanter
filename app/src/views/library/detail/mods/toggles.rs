//! Switches over many mods at once: *Mods activos*, the account's switch for every mod of the game,
//! which leaves each mod's own state alone so switching back restores the same selection;
//! *Activar todos* and *Desactivar todos*; whether a mod starts on when installed; and an installed
//! mod's checkbox where a provider lists it. Like every change in the tab, they apply at the next
//! launch and are locked while the game runs.

use decanter_mods::{ModId, OrderChange};
use gpui::{Context, Div, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use super::packs::change_lines;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox, checkbox_large, confirm_panel};

/// A mod the game has from a provider, found by the origin its *installed* mark matches.
pub(super) struct Installed {
    pub(super) version: String,
    pub(super) id: ModId,
    pub(super) enabled: bool,
    pub(super) locked: bool,
}

/// A conflict or DLL line of a mod while the game's mods are switched off.
pub(super) fn no_effect(line: &str) -> SharedString {
    t!("mods.toggles.no_effect", line = line).into_owned().into()
}

impl ModsTab {
    /// On until the set file says otherwise, as a game without one plays its mods.
    pub(super) fn is_active(&self) -> bool {
        self.loaded.as_ref().is_none_or(|loaded| loaded.is_active)
    }

    /// `None` until the set file was read.
    pub(in crate::views::library::detail) fn is_switched_off(&self) -> Option<bool> {
        self.loaded.as_ref().map(|loaded| !loaded.is_active)
    }

    pub(super) fn enables_on_install(&self) -> bool {
        self.enable_on_install.unwrap_or(true)
    }

    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.run(
            Text::new("library.detail.mods.applying"),
            move |mods, account| mods.set_active(account, active),
            cx,
        );
        self.notice = Some(Text::new("mods.toggles.applies_at_launch"));
    }

    /// Enabling every mod asks first when it takes a file or a DLL from a mod on now, as a move
    /// does; otherwise it applies at once.
    fn request_enable_all(&mut self, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        if self.is_locked() {
            return;
        }
        let account_id = self.target.account_id.clone();
        self.busy = Some(Text::new("mods.order.checking"));
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let preview = tokio::task::spawn_blocking(move || mods.preview_all_enabled(&account_id)).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match preview {
                    Ok(Ok(change)) if change.is_empty() => this.set_all_enabled(true, cx),
                    Ok(Ok(change)) => {
                        tracing::info!(
                            files = change.files.len(),
                            dlls = change.dlls.len(),
                            "enabling every mod changes winners, asking first"
                        );
                        this.confirm_enable_all = Some(change);
                    }
                    Ok(Err(err)) => {
                        tracing::error!(error = %err, "could not check enabling every mod");
                        this.error = Some(Text::verbatim(err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "checking enabling every mod stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_all_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.confirm_enable_all = None;
        self.run(
            Text::new("library.detail.mods.applying"),
            move |mods, account| mods.set_all_enabled(account, enabled).map(|_| ()),
            cx,
        );
        self.notice = Some(Text::new("mods.toggles.applies_at_launch"));
    }

    /// The Instalados row's checkbox, from a provider's result or detail.
    fn toggle_from_results(&mut self, id: ModId, enabled: bool, cx: &mut Context<Self>) {
        if self.is_locked() {
            return;
        }
        self.toggle(id, enabled, cx);
        self.notice = Some(Text::new("mods.toggles.applies_at_launch"));
    }

    fn show_in_installed(&mut self, id: ModId, cx: &mut Context<Self>) {
        self.highlight = Some(id);
        self.show_installed(cx);
    }

    /// Kept for the next install of this game, written off the UI; a failure only costs the memory.
    pub(super) fn set_enable_on_install(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.enable_on_install = Some(enabled);
        cx.notify();
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let account_id = self.target.account_id.clone();
        cx.spawn(async move |_, _| {
            match tokio::task::spawn_blocking(move || mods.set_enables_on_install(&account_id, enabled)).await {
                Ok(Ok(())) => tracing::info!(enabled, "mods install choice kept"),
                Ok(Err(err)) => tracing::warn!(%err, "mods install choice not kept"),
                Err(err) => tracing::warn!(%err, "keeping the mods install choice stopped"),
            }
        })
        .detach();
    }

    /// *Mods activos* with *Activar todos* and *Desactivar todos*, over the mod list.
    pub(super) fn switch_panel(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let loaded = self.loaded.as_ref()?;
        let active = loaded.is_active;
        let any_off = loaded.rows.iter().any(|row| !row.enabled);
        let any_on = loaded.rows.iter().any(|row| row.enabled);
        let state = if active {
            t!("mods.toggles.switch_on")
        } else {
            t!("mods.toggles.switch_off")
        };
        let switch = div()
            .id("mods-switch")
            .flex()
            .items_center()
            .gap_3()
            .when(!locked, |switch| {
                switch
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_active(!active, cx)))
            })
            .child(checkbox_large(active).when(locked, |b| b.opacity(0.4)))
            .child(theme::h3(t!("mods.toggles.switch_label")));
        let enable_disabled = locked || !any_off;
        let disable_disabled = locked || !any_on;
        Some(
            theme::card()
                .gap_2()
                .px(px(18.))
                .py(px(14.))
                .child(switch)
                .child(theme::muted(state))
                .child(theme::muted(t!("mods.toggles.applies_at_launch")).text_size(px(12.5)))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(10.))
                        .pt_1()
                        .child(
                            button(
                                "mods-enable-all",
                                ButtonKind::Secondary,
                                t!("mods.toggles.enable_all"),
                                enable_disabled,
                            )
                            .when(!enable_disabled, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.request_enable_all(cx)))
                            }),
                        )
                        .child(
                            button(
                                "mods-disable-all",
                                ButtonKind::Secondary,
                                t!("mods.toggles.disable_all"),
                                disable_disabled,
                            )
                            .when(!disable_disabled, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.set_all_enabled(false, cx)))
                            }),
                        ),
                ),
        )
    }

    pub(super) fn enable_all_confirmation(&self, cx: &Context<Self>) -> Option<Div> {
        let change: &OrderChange = self.confirm_enable_all.as_ref()?;
        let names = &self.loaded.as_ref()?.names;
        let name_of = |id: &ModId| names.get(id).cloned().unwrap_or_else(|| id.to_string());
        Some(confirm_panel(
            "mods-enable-all-confirm",
            t!("mods.toggles.enable_all_title"),
            change_lines(change, &name_of).join("\n"),
            t!("mods.toggles.enable_all"),
            cx.listener(|this, _, _, cx| this.set_all_enabled(true, cx)),
            cx.listener(|this, _, _, cx| {
                this.confirm_enable_all = None;
                cx.notify();
            }),
        ))
    }

    /// The mod of the last row in the load order whose origin `matches` accepts, for a provider's
    /// result or detail.
    pub(super) fn installed_mod(
        &self,
        matches: impl Fn(&decanter_mods::ModOrigin) -> bool,
        version: impl FnOnce(&decanter_mods::ModOrigin) -> String,
    ) -> Option<Installed> {
        let row = self
            .loaded
            .as_ref()?
            .rows
            .iter()
            .filter(|row| row.manifest.origin.as_ref().is_some_and(&matches))
            .last()?;
        Some(Installed {
            version: version(row.manifest.origin.as_ref()?),
            id: row.manifest.id.clone(),
            enabled: row.enabled,
            locked: self.is_locked(),
        })
    }
}

/// What ticking an install choice does: [`ModsTab::set_enable_on_install`], or that and more.
pub(super) type OnChoice = fn(&mut ModsTab, bool, &mut Context<ModsTab>);

/// The *Activar al instalar* box of an install dialog: one choice per game and account, shared by
/// every dialog of the tab.
pub(super) fn install_choice(
    id: &'static str,
    enabled: bool,
    label: SharedString,
    on_choice: OnChoice,
    cx: &Context<ModsTab>,
) -> Div {
    div().flex().child(
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| on_choice(this, !enabled, cx)))
            .child(checkbox(enabled))
            .child(theme::muted(label).text_size(px(12.5))),
    )
}

/// The default label of [`install_choice`].
pub(super) fn install_label() -> SharedString {
    t!("mods.toggles.install_enabled").into_owned().into()
}

/// An installed mod's checkbox and *Ver en Instalados*, inside a provider's result or detail; the
/// clicks stay here so that a result row does not also open the mod.
pub(super) fn installed_controls(index: usize, installed: &Installed, cx: &Context<ModsTab>) -> Div {
    let (toggle_id, show_id) = (installed.id.clone(), installed.id.clone());
    let enabled = installed.enabled;
    let locked = installed.locked;
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(
            div()
                .id(("mod-installed-toggle", index))
                .flex()
                .items_center()
                .gap_1()
                .when(!locked, |toggle| {
                    toggle.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_from_results(toggle_id.clone(), !enabled, cx);
                    }))
                })
                .child(checkbox(enabled).when(locked, |b| b.opacity(0.4)))
                .child(theme::muted(t!("mods.toggles.installed_enabled")).text_size(px(12.5))),
        )
        .child(
            div()
                .id(("mod-installed-show", index))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(gpui::rgb(theme::ACCENT_BLUE))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.show_in_installed(show_id.clone(), cx);
                }))
                .child(t!("mods.toggles.show_installed")),
        )
}
