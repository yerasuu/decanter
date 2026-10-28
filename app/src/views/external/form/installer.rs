use std::borrow::Cow;

use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::{GameForm, Rows};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub(super) enum Installer {
    Idle,
    Running,
    /// Finished: the executable picker starts where it most likely put the game.
    Done,
    Failed(Text),
}

/// Under Proton only a Windows program; run natively, Windows' own
/// installer packages too.
fn installer_extensions() -> &'static [&'static str] {
    if crate::launch::in_prefixes() {
        &["exe"]
    } else {
        &["exe", "msi"]
    }
}

impl GameForm {
    pub(super) fn run_installer(&mut self, cx: &mut Context<Self>) {
        let (profile_id, id, proton, library_root) = (
            self.profile_id.clone(),
            self.id.clone(),
            self.wine.proton.clone(),
            self.library_root.clone(),
        );
        self._work = Some(cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(t!("external.form.installer.pick_title"))
                .add_filter(t!("external.form.installer.filter"), installer_extensions())
                .pick_file()
                .await
            else {
                return;
            };
            let installer = file.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                this.installer = Installer::Running;
                this.error = None;
                cx.notify();
            });
            let result =
                crate::external::run_installer(&profile_id, &id, &installer, proton, library_root.as_deref()).await;
            let _ = this.update(cx, |this, cx| {
                this.installer = match result {
                    Ok(()) => Installer::Done,
                    Err(err) => Installer::Failed(err),
                };
                cx.notify();
                if matches!(this.installer, Installer::Done) {
                    this.pick_executable(cx);
                }
            });
        }));
    }

    /// Where the executable picker opens: Program Files once an installer
    /// ran, since installers don't say where they put the game.
    pub(super) fn native_executable_start(&self) -> Option<std::path::PathBuf> {
        matches!(self.installer, Installer::Done)
            .then(|| std::env::var_os("ProgramFiles"))
            .flatten()
            .map(std::path::PathBuf::from)
    }

    /// Whether the form is busy, its save button's label and its rows, for a
    /// game run as the system's own program: no prefix to pick or import.
    pub(super) fn native_rows(&self, cx: &Context<Self>) -> (bool, Cow<'static, str>, Rows) {
        (
            matches!(self.installer, Installer::Running),
            if self.editing {
                t!("external.form.save.edit")
            } else {
                t!("external.form.save.add")
            },
            Rows {
                installer: (!self.editing).then(|| self.native_installer_row(cx)),
                ..Rows::default()
            },
        )
    }

    fn native_installer_row(&self, cx: &Context<Self>) -> Div {
        let running = matches!(self.installer, Installer::Running);
        let status = match &self.installer {
            Installer::Idle => theme::muted(t!("external.form.installer.hint_native")),
            Installer::Running => theme::muted(t!("external.form.installer.running")),
            Installer::Done => theme::muted(t!("external.form.installer.done")),
            Installer::Failed(err) => theme::error(err),
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .mb_3()
            .child(
                button(
                    "external-run-installer",
                    ButtonKind::Secondary,
                    t!("external.form.installer.run"),
                    running,
                )
                .when(!running, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.run_installer(cx)))
                }),
            )
            .child(div().flex_1().min_w_0().child(status))
    }
}
