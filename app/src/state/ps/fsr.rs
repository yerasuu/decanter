//! Building FSR 4.1.1 from the user's AMD DLL: checked by bbport's own
//! inspector first, recorded under Proton by bbport's script, and chosen as
//! the upscaler once it succeeds.

use std::path::PathBuf;

use decanter_ps::bbport::tools::{self, DllCheck};
use decanter_ps::bbport::{GROUPS, values};
use decanter_ps::port::Port;
use gpui::{AppContext as _, Context};

use super::Ps;
use super::jobs::{self, JobKey};

/// How a DLL check ended, for the view to act on.
pub enum DllOutcome {
    Building,
    /// No FidelityFX loader beside the DLL: the view asks for one.
    NeedsLoader(PathBuf),
    Refused(String),
}

impl Ps {
    pub fn build_fsr411(
        &mut self,
        upscaler: PathBuf,
        loader: Option<PathBuf>,
        cx: &mut Context<Self>,
        on_checked: impl FnOnce(DllOutcome, &mut gpui::App) + 'static,
    ) {
        let prepared = self.prepare(Port::Bbport, cx);
        self.notice = Some(("Comprobando la DLL…".to_owned(), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let toolchain = match prepared.await {
                Ok(toolchain) => toolchain,
                Err(err) => {
                    cx.update(|cx| on_checked(DllOutcome::Refused(format!("bbport no está listo: {err}")), cx));
                    return;
                }
            };
            let checked = tools::check_dll(&toolchain, &upscaler, loader.as_deref()).await;
            let found_loader = match checked {
                Ok(DllCheck::Ready { loader: found }) => loader.or(found),
                Ok(DllCheck::NeedsLoader) => {
                    cx.update(|cx| on_checked(DllOutcome::NeedsLoader(upscaler), cx));
                    return;
                }
                Ok(DllCheck::Unsupported(why) | DllCheck::BadLoader(why)) => {
                    cx.update(|cx| on_checked(DllOutcome::Refused(why), cx));
                    return;
                }
                Err(err) => {
                    let why = format!("No se pudo comprobar la DLL: {err}");
                    cx.update(|cx| on_checked(DllOutcome::Refused(why), cx));
                    return;
                }
            };
            cx.update(|cx| on_checked(DllOutcome::Building, cx));
            let command = match tools::fsr411_build_command(&toolchain, &upscaler, found_loader.as_deref()).await {
                Ok(command) => command,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.notice = Some((format!("No se pudo preparar el entorno: {err}"), true));
                        cx.notify();
                    });
                    return;
                }
            };
            let finished = jobs::run(&this, JobKey::Fsr411, "compilación de FSR 4.1.1", command, None, cx).await;
            let stopped = this
                .update(cx, |this, cx| {
                    this.jobs
                        .get(&JobKey::Fsr411)
                        .is_some_and(|job| job.read(cx).stop_requested)
                })
                .unwrap_or(false);
            let notice = if finished.succeeded() {
                let upscaler = GROUPS
                    .iter()
                    .flat_map(|group| group.settings)
                    .find(|setting| setting.key == "upscaler");
                let chosen = match upscaler {
                    Some(setting) => {
                        cx.background_spawn(async move { values::save(setting, "fsr411", None) })
                            .await
                    }
                    None => Ok(()),
                };
                match chosen {
                    Ok(()) => ("FSR 4.1.1 compilado y elegido como reescalado".to_owned(), false),
                    Err(err) => (format!("FSR 4.1.1 compilado, pero no se pudo elegir: {err}"), true),
                }
            } else if stopped {
                ("Compilación de FSR 4.1.1 cancelada".to_owned(), false)
            } else {
                (tools::fsr411_build_error(finished.code), true)
            };
            let _ = this.update(cx, |this, cx| {
                this.notice = Some(notice);
                this.fsr_revision = this.fsr_revision.wrapping_add(1);
                cx.notify();
            });
        })
        .detach();
    }
}
