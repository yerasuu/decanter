//! Building FSR 4.1.1 from the user's AMD DLL: checked by bbport's own
//! inspector first, recorded under Proton by bbport's script, and chosen as
//! the upscaler once it succeeds.

use std::path::{Path, PathBuf};

use decanter_ps::bbport::tools::{self, BadLoader, DllCheck, Fsr411Failure, Unsupported};
use decanter_ps::bbport::{GROUPS, values};
use decanter_ps::port::Port;
use decanter_ps::process::JobKind;
use gpui::{AppContext as _, Context};
use rust_i18n::t;

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
        let root = decanter_ps::paths::active();
        let prepared = self.prepare(Port::Bbport, &root, cx);
        self.notice = Some((t!("ps.fsr.checking").into_owned(), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let toolchain = match prepared.await {
                Ok(toolchain) => toolchain,
                Err(err) => {
                    let why = t!("ps.errors.bbport_not_ready", error = err).into_owned();
                    cx.update(|cx| on_checked(DllOutcome::Refused(why), cx));
                    return;
                }
            };
            let checked = tools::check_dll(&root, &toolchain, &upscaler, loader.as_deref()).await;
            let found_loader = match checked {
                Ok(DllCheck::Ready { loader: found }) => loader.or(found),
                Ok(DllCheck::NeedsLoader) => {
                    cx.update(|cx| on_checked(DllOutcome::NeedsLoader(upscaler), cx));
                    return;
                }
                Ok(DllCheck::Unsupported(unsupported)) => {
                    let why = unsupported_text(&unsupported, &upscaler);
                    cx.update(|cx| on_checked(DllOutcome::Refused(why), cx));
                    return;
                }
                Ok(DllCheck::BadLoader(bad)) => {
                    cx.update(|cx| on_checked(DllOutcome::Refused(bad_loader_text(&bad)), cx));
                    return;
                }
                Err(err) => {
                    let why = t!("ps.fsr.check_error", error = err).into_owned();
                    cx.update(|cx| on_checked(DllOutcome::Refused(why), cx));
                    return;
                }
            };
            cx.update(|cx| on_checked(DllOutcome::Building, cx));
            let command = match tools::fsr411_build_command(&root, &toolchain, &upscaler, found_loader.as_deref()).await
            {
                Ok(command) => command,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.notice = Some((t!("ps.errors.environment", error = err).into_owned(), true));
                        cx.notify();
                    });
                    return;
                }
            };
            let facts = vec![("toolchain", toolchain.id().to_owned())];
            let finished = jobs::run(&this, &root, JobKey::Fsr411, JobKind::Fsr411, facts, command, None, cx).await;
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
                        let root = root.clone();
                        cx.background_spawn(async move { values::save(&root, setting, "fsr411", None) })
                            .await
                    }
                    None => Ok(()),
                };
                match chosen {
                    Ok(()) => (t!("ps.fsr.built").into_owned(), false),
                    Err(err) => (t!("ps.fsr.built_not_chosen", error = err).into_owned(), true),
                }
            } else if stopped {
                (t!("ps.fsr.cancelled").into_owned(), false)
            } else {
                (failure_text(&tools::fsr411_failure(finished.code)), true)
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

fn unsupported_text(unsupported: &Unsupported, upscaler: &Path) -> String {
    let name = upscaler.file_name().unwrap_or_default().to_string_lossy();
    match unsupported {
        Unsupported::Fsr40 { version } => t!("ps.fsr.unsupported.fsr40", name = name, version = version),
        Unsupported::NotFsr4 => t!("ps.fsr.unsupported.not_fsr4", name = name),
        Unsupported::Other { version } => t!("ps.fsr.unsupported.other", name = name, version = version),
    }
    .into_owned()
}

fn bad_loader_text(bad: &BadLoader) -> String {
    match bad {
        BadLoader::IsUpscaler => t!("ps.fsr.bad_loader.is_upscaler"),
        BadLoader::WrongVersion { loader, version } => {
            t!(
                "ps.fsr.bad_loader.wrong_version",
                loader = loader.display(),
                version = version
            )
        }
    }
    .into_owned()
}

fn failure_text(failure: &Fsr411Failure) -> String {
    match *failure {
        Fsr411Failure::BadDll => t!("ps.fsr.failures.bad_dll"),
        Fsr411Failure::NoProton => t!("ps.fsr.failures.no_proton"),
        Fsr411Failure::MissingTools => t!("ps.fsr.failures.missing_tools"),
        Fsr411Failure::NoProtonRan => t!("ps.fsr.failures.no_proton_ran"),
        Fsr411Failure::Mismatch => t!("ps.fsr.failures.mismatch"),
        Fsr411Failure::NoLoader => t!("ps.fsr.failures.no_loader"),
        Fsr411Failure::NixosNeedsRecorder => t!("ps.fsr.failures.nixos"),
        Fsr411Failure::Failed(code) => t!("ps.fsr.failures.failed", code = code),
        Fsr411Failure::Stopped => t!("ps.fsr.failures.stopped"),
    }
    .into_owned()
}
