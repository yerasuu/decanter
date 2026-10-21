use gpui::Context;
#[cfg(not(proton))]
use gpui::{Div, div, prelude::*, px};
use rust_i18n::t;

use super::GameForm;
use crate::i18n::Text;
#[cfg(not(proton))]
use crate::theme;
#[cfg(not(proton))]
use crate::widgets::{ButtonKind, button};

pub(super) enum Installer {
    Idle,
    Running,
    /// Finished: the executable picker starts where it most likely put the game.
    Done,
    Failed(Text),
}

#[cfg(proton)]
const INSTALLER_EXTENSIONS: &[&str] = &["exe"];
#[cfg(not(proton))]
const INSTALLER_EXTENSIONS: &[&str] = &["exe", "msi"];

impl GameForm {
    pub(super) fn run_installer(&mut self, cx: &mut Context<Self>) {
        #[cfg(proton)]
        let (profile_id, id, proton, library_root) = (
            self.profile_id.clone(),
            self.id.clone(),
            self.wine.proton.clone(),
            self.library_root.clone(),
        );
        self._work = Some(cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(t!("external.form.installer.pick_title"))
                .add_filter(t!("external.form.installer.filter"), INSTALLER_EXTENSIONS)
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
            #[cfg(proton)]
            let result =
                crate::external::run_installer(&profile_id, &id, &installer, proton, library_root.as_deref()).await;
            #[cfg(not(proton))]
            let result = crate::external::run_installer(&installer).await;
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
    #[cfg(not(proton))]
    pub(super) fn executable_start(&self) -> Option<std::path::PathBuf> {
        matches!(self.installer, Installer::Done)
            .then(|| std::env::var_os("ProgramFiles"))
            .flatten()
            .map(std::path::PathBuf::from)
    }

    #[cfg(not(proton))]
    pub(super) fn installer_row(&self, cx: &Context<Self>) -> Div {
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
