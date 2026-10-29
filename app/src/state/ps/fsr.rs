//! Building FSR 4.1.1 from the user's AMD DLL: checked by bbport's own
//! inspector first, recorded under Proton by bbport's script, and chosen as
//! the upscaler of the game it was asked from once it succeeds.
//!
//! The recording runs under decanter's own GE-Proton and `umu-run`, prepared
//! here first, so the user never installs a Proton build or opens Steam for it.

use std::path::{Path, PathBuf};

use decanter_core::runner::{Progress, Runner, Stage};
use decanter_ps::bbport::tools::{self, BadLoader, DllCheck, Fsr411Failure, Unsupported};
use decanter_ps::bbport::{Bbport, GROUPS, values};
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::JobKind;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use futures::future::{self, Either};
use gpui::{AppContext as _, AsyncApp, Context, WeakEntity};

use super::Ps;
use super::jobs::{self, JobKey};
use crate::i18n::Text;
use crate::runners;

/// How a DLL check ended, for the view to act on.
pub enum DllOutcome {
    Building,
    /// No FidelityFX loader beside the DLL: the view asks for one.
    NeedsLoader(PathBuf),
    Refused(Text),
}

impl Ps {
    pub fn build_fsr411(
        &mut self,
        upscaler: PathBuf,
        loader: Option<PathBuf>,
        title_id: String,
        cx: &mut Context<Self>,
        on_checked: impl FnOnce(DllOutcome, &mut gpui::App) + 'static,
    ) {
        let root = decanter_ps::paths::active();
        let prepared = self.prepare(Port::Bbport, &root, cx);
        self.notice = Some((Text::new("ps.fsr.checking"), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let toolchain = match prepared.await {
                Ok(toolchain) => toolchain,
                Err(err) => {
                    let why = Text::new("ps.errors.bbport_not_ready").text("error", err);
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
                    let why = Text::new("ps.fsr.check_error").arg("error", err);
                    cx.update(|cx| on_checked(DllOutcome::Refused(why), cx));
                    return;
                }
            };
            cx.update(|cx| on_checked(DllOutcome::Building, cx));
            let recorder = match prepare_recorder(&this, &root, cx).await {
                Ok(recorder) => recorder,
                Err(notice) => {
                    let _ = this.update(cx, |this, cx| {
                        this.notice = Some(notice);
                        cx.notify();
                    });
                    return;
                }
            };
            let command = match tools::fsr411_build_command(&root, &toolchain, &upscaler, found_loader.as_deref()).await
            {
                Ok(command) => with_recorder(recorder.as_ref(), command),
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.notice = Some((Text::new("ps.errors.environment").arg("error", err), true));
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
                        cx.background_spawn(async move {
                            Bbport::title_ini(&root, &title_id)
                                .and_then(|port_ini| values::save(&root, &port_ini.path, setting, "fsr411", None))
                        })
                        .await
                    }
                    None => Ok(()),
                };
                match chosen {
                    Ok(()) => (Text::new("ps.fsr.built"), false),
                    Err(err) => (Text::new("ps.fsr.built_not_chosen").arg("error", err), true),
                }
            } else if stopped {
                (Text::new("ps.fsr.cancelled"), false)
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

impl Ps {
    /// The runner an FSR 4.1.1 build is getting before it records, and how far it got.
    pub const fn fsr_runner(&self) -> Option<&(Runner, Progress)> {
        self.fsr_runner.as_ref()
    }

    /// Stops getting the runners, dropping the download or unpacking it is in.
    pub fn cancel_fsr_runner(&mut self, cx: &mut Context<Self>) {
        self.fsr_cancel = None;
        self.fsr_runner = None;
        cx.notify();
    }
}

/// What the recording runs under: psport's script takes `PROTONPATH` as it is
/// and the first `umu-run` on `PATH`, which decanter's own copy is not
/// without the folder of [`decanter_runtime::umu_run_dir`].
struct Recorder {
    umu_dir: PathBuf,
    proton: PathBuf,
}

fn with_recorder(recorder: Option<&Recorder>, mut command: tokio::process::Command) -> tokio::process::Command {
    let Some(recorder) = recorder else {
        return command;
    };
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let search = std::iter::once(recorder.umu_dir.clone()).chain(std::env::split_paths(&inherited));
    match std::env::join_paths(search) {
        Ok(search) => {
            command.env("PATH", search);
        }
        Err(err) => {
            tracing::warn!(%err, dir = %recorder.umu_dir.display(), "FSR 4.1.1: umu-run's folder can't go on PATH");
        }
    }
    command.env("PROTONPATH", &recorder.proton);
    command
}

/// decanter's `umu-run` and GE-Proton, each installed unless it is there, with
/// their progress shown in the FSR row; `None` where the recording needs no
/// Proton, and otherwise what the user is told, a cancel included.
async fn prepare_recorder(
    this: &WeakEntity<Ps>,
    root: &Root,
    cx: &mut AsyncApp,
) -> Result<Option<Recorder>, (Text, bool)> {
    if !runners::support(Runner::GeProton).is_offered() {
        return Ok(None);
    }
    let (cancel, cancelled) = oneshot::channel();
    let (sender, mut receiver) = mpsc::unbounded::<(Runner, Progress)>();
    let _ = this.update(cx, |this, _| this.fsr_cancel = Some(cancel));
    let shown = this.clone();
    let _shown = cx.spawn(async move |cx| {
        while let Some(update) = receiver.next().await {
            let _ = shown.update(cx, |ps, cx| {
                ps.fsr_runner = Some(update);
                cx.notify();
            });
        }
    });
    // A step per mebibyte, since a download reports every chunk and each step redraws the row.
    let mut last = None;
    let report = move |runner: Runner, progress: Progress| {
        let step = (
            matches!(runner, Runner::GeProton),
            matches!(progress.stage, Stage::Unpacking),
            progress.received >> 20,
        );
        if last != Some(step) {
            last = Some(step);
            let _ = sender.unbounded_send((runner, progress));
        }
    };
    let prepared = future::select(Box::pin(get_recorder(root, report)), cancelled).await;
    let _ = this.update(cx, |this, cx| {
        this.fsr_cancel = None;
        this.fsr_runner = None;
        cx.notify();
    });
    match prepared {
        Either::Left((Ok(recorder), _)) => Ok(Some(recorder)),
        Either::Left((Err(err), _)) => {
            tracing::warn!(error = %err, "FSR 4.1.1: decanter's Proton is not ready");
            Err((err.text(), true))
        }
        Either::Right(_) => Err((Text::new("ps.fsr.cancelled"), false)),
    }
}

async fn get_recorder(
    root: &Root,
    mut report: impl FnMut(Runner, Progress) + Send,
) -> Result<Recorder, runners::Error> {
    let umu = runners::update(
        root,
        Runner::Umu,
        &mut |progress| report(Runner::Umu, progress),
        &mut |_| {},
    )
    .await?;
    let umu_dir = decanter_runtime::umu_run_dir(&umu.path)
        .await
        .map_err(|err| runners::Error::Prepare(Runner::Umu, err.into()))?;
    let proton = runners::update(
        root,
        Runner::GeProton,
        &mut |progress| report(Runner::GeProton, progress),
        &mut |_| {},
    )
    .await?;
    Ok(Recorder {
        umu_dir,
        proton: proton.path,
    })
}

fn unsupported_text(unsupported: &Unsupported, upscaler: &Path) -> Text {
    let name = upscaler.file_name().unwrap_or_default().to_string_lossy();
    match unsupported {
        Unsupported::Fsr40 { version } => Text::new("ps.fsr.unsupported.fsr40")
            .arg("name", name)
            .arg("version", version),
        Unsupported::NotFsr4 => Text::new("ps.fsr.unsupported.not_fsr4").arg("name", name),
        Unsupported::Other { version } => Text::new("ps.fsr.unsupported.other")
            .arg("name", name)
            .arg("version", version),
    }
}

fn bad_loader_text(bad: &BadLoader) -> Text {
    match bad {
        BadLoader::IsUpscaler => Text::new("ps.fsr.bad_loader.is_upscaler"),
        BadLoader::WrongVersion { loader, version } => Text::new("ps.fsr.bad_loader.wrong_version")
            .arg("loader", loader.display())
            .arg("version", version),
    }
}

fn failure_text(failure: &Fsr411Failure) -> Text {
    match *failure {
        Fsr411Failure::BadDll => Text::new("ps.fsr.failures.bad_dll"),
        Fsr411Failure::NoProton => Text::new("ps.fsr.failures.no_proton"),
        Fsr411Failure::MissingTools => Text::new("ps.fsr.failures.missing_tools"),
        Fsr411Failure::NoProtonRan => Text::new("ps.fsr.failures.no_proton_ran"),
        Fsr411Failure::Mismatch => Text::new("ps.fsr.failures.mismatch"),
        Fsr411Failure::NoLoader => Text::new("ps.fsr.failures.no_loader"),
        Fsr411Failure::NixosNeedsRecorder => Text::new("ps.fsr.failures.nixos"),
        Fsr411Failure::Failed(code) => Text::new("ps.fsr.failures.failed").arg("code", code),
        Fsr411Failure::Stopped => Text::new("ps.fsr.failures.stopped"),
    }
}
