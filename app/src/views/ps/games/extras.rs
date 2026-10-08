//! What bbport's settings table can't hold: building FSR 4.1.1 from an AMD
//! DLL, and free-form environment for the game.

use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::tools;
use decanter_ps::bbport::values::Values;
use decanter_ps::process::Job;
use gpui::{AnyElement, Context, Entity, Subscription, Window, div, prelude::*, px, rgb};

use crate::state::{DllOutcome, JobKey, Ps};
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

pub struct ExtrasView {
    ps: Entity<Ps>,
    /// Why the FSR 4.1.1 assets can't be used yet, `None` when they can;
    /// outer `None` while it's being checked.
    fsr_problem: Option<Option<String>>,
    checked_at: Option<u32>,
    message: Option<(String, bool)>,
    env: Entity<TextInput>,
    /// The build being watched, so its progress line follows its output.
    fsr_job: Option<(Entity<Job>, Subscription)>,
}

impl ExtrasView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| {
            this.check_fsr(cx);
            this.watch_fsr_job(cx);
        })
        .detach();
        let env = cx.new(|cx| TextInput::new("NOMBRE=valor separadas por espacios", cx));
        cx.subscribe(&env, |this, env, event, cx| {
            if matches!(event, InputEvent::Submit) {
                let value = env.read(cx).text().trim().to_owned();
                this.save_env(value, cx);
            }
        })
        .detach();
        let load = cx.background_spawn(async { LauncherSettings::load().text("extra_env").map(str::to_owned) });
        cx.spawn(async move |this, cx| {
            let current = load.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.env.update(cx, |input, cx| input.set_text(current, cx));
            });
        })
        .detach();
        let mut this = Self {
            ps,
            fsr_problem: None,
            checked_at: None,
            message: None,
            env,
            fsr_job: None,
        };
        this.check_fsr(cx);
        this
    }

    fn watch_fsr_job(&mut self, cx: &mut Context<Self>) {
        let job = self.ps.read(cx).job(&JobKey::Fsr411).cloned();
        let watched = self.fsr_job.as_ref().map(|(watched, _)| watched);
        if job.as_ref() == watched {
            return;
        }
        self.fsr_job = job.map(|job| {
            let subscription = cx.observe(&job, |_, _, cx| cx.notify());
            (job, subscription)
        });
        cx.notify();
    }

    /// Against the output size and preset chosen now, whenever a build ends.
    fn check_fsr(&mut self, cx: &mut Context<Self>) {
        let revision = self.ps.read(cx).fsr_revision();
        if self.checked_at == Some(revision) {
            return;
        }
        self.checked_at = Some(revision);
        let task = cx.background_spawn(async {
            let values = Values::load();
            let setting = |key| {
                decanter_ps::bbport::GROUPS
                    .iter()
                    .flat_map(|group| group.settings)
                    .find(|setting| setting.key == key)
                    .and_then(|setting| values.get(setting))
            };
            let output = setting("output_res").unwrap_or_else(|| "1920x1080".to_owned());
            let preset = setting("preset").unwrap_or_else(|| "4".to_owned());
            tools::fsr411_problem(&decanter_ps::paths::fsr411(), &output, &preset)
        });
        cx.spawn(async move |this, cx| {
            let problem = task.await;
            let _ = this.update(cx, |this, cx| {
                this.fsr_problem = Some(problem);
                cx.notify();
            });
        })
        .detach();
    }

    fn save_env(&mut self, value: String, cx: &mut Context<Self>) {
        let task = cx
            .background_spawn(async move { LauncherSettings::edit(|settings| settings.set_text("extra_env", &value)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.message = Some(match result {
                    Ok(()) => (
                        "Variables guardadas; se aplican en el próximo arranque".to_owned(),
                        false,
                    ),
                    Err(err) => (format!("No se pudieron guardar las variables: {err}"), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks for the DLL, or for its loader when the DLL needs one.
    fn pick_dll(&mut self, upscaler: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        let title = if upscaler.is_none() {
            "DLL del reescalado de AMD (amd_fidelityfx_upscaler_dx12.dll)"
        } else {
            "Cargador FidelityFX (amd_fidelityfx_loader_dx12.dll)"
        };
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(title)
                .add_filter("DLL", &["dll"])
                .pick_file()
                .await
            else {
                return;
            };
            let chosen = file.path().to_owned();
            let _ = this.update(cx, |this, cx| {
                let (upscaler, loader) = match upscaler {
                    Some(upscaler) => (upscaler, Some(chosen)),
                    None => (chosen, None),
                };
                this.message = Some(("Comprobando la DLL…".to_owned(), false));
                let view = cx.entity().downgrade();
                this.ps.update(cx, |ps, cx| {
                    ps.build_fsr411(upscaler, loader, cx, move |outcome, cx| {
                        let _ = view.update(cx, |this, cx| {
                            match outcome {
                                DllOutcome::Building => this.message = None,
                                DllOutcome::NeedsLoader(upscaler) => {
                                    this.message = Some((
                                        "Junto a la DLL no hay cargador: elegí amd_fidelityfx_loader_dx12.dll de la \
                                         carpeta del juego"
                                            .to_owned(),
                                        false,
                                    ));
                                    this.pick_dll(Some(upscaler), cx);
                                }
                                DllOutcome::Refused(why) => this.message = Some((why, true)),
                            }
                            cx.notify();
                        });
                    });
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn fsr_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let job = self
            .ps
            .read(cx)
            .job(&JobKey::Fsr411)
            .filter(|job| job.read(cx).is_running())
            .cloned();
        let status = match (&job, &self.fsr_problem) {
            (Some(job), _) => tools::fsr411_progress(job.read(cx)),
            (None, None) => "Comprobando…".to_owned(),
            (None, Some(None)) => "Recursos listos para esta resolución y preset".to_owned(),
            (None, Some(Some(problem))) => format!(
                "Sin recursos ({problem}). Se compilan desde amd_fidelityfx_upscaler_dx12.dll 4.1.x (FSR4_LATEST \
                 de OptiScaler o de un juego con FSR 4.1); necesita GE-Proton 10+, Proton Experimental o \
                 Proton-CachyOS y tarda 2–5 minutos"
            ),
        };
        let action = if job.is_some() {
            button("fsr411", ButtonKind::Secondary, "Cancelar", false)
                .on_click(cx.listener(|this, _, _, cx| this.ps.update(cx, |ps, cx| ps.stop(&JobKey::Fsr411, cx))))
        } else {
            button("fsr411", ButtonKind::Secondary, "Elegir DLL…", false)
                .on_click(cx.listener(|this, _, _, cx| this.pick_dll(None, cx)))
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child("FSR 4.1.1 desde tu DLL de AMD")
                    .child(theme::muted(status).text_size(px(12.))),
            )
            .child(action)
            .into_any_element()
    }
}

impl Render for ExtrasView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::h3("Extras"))
            .children(
                self.message
                    .clone()
                    .map(|(text, error)| if error { theme::error(text) } else { theme::muted(text) }),
            )
            .child(
                theme::card().child(self.fsr_row(cx)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .py_2()
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .child("Variables extra")
                                .child(theme::muted("Enter guarda").text_size(px(12.))),
                        )
                        .child(div().flex_1().min_w_0().child(self.env.clone())),
                ),
            )
    }
}
