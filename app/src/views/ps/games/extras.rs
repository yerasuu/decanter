//! What bbport's settings table can't hold: building FSR 4.1.1 from an AMD
//! DLL, and, for the shown game alone, free-form environment and whether it
//! loads the user's system modules dump.

use decanter_ps::bbport::tools::{self, Fsr411Progress};
use decanter_ps::bbport::values::Values;
use decanter_ps::bbport::{LauncherSettings, SYS_MODULES};
use decanter_ps::process::Job;
use gpui::{AnyElement, Context, Entity, Subscription, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::runners;
use crate::state::{DllOutcome, JobKey, Ps};
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button, checkbox};

pub struct ExtrasView {
    ps: Entity<Ps>,
    /// The first FSR 4.1.1 asset missing or damaged, `None` when all are
    /// there; outer `None` while it's being checked.
    fsr_problem: Option<Option<String>>,
    checked_at: Option<u32>,
    message: Option<(Text, bool)>,
    env: Entity<TextInput>,
    /// Whether the title loads the system modules dump, and whether Ajustes has its folder;
    /// `None` while they are read.
    sys_modules: Option<(bool, bool)>,
    /// The build being watched, so its progress line follows its output.
    fsr_job: Option<(Entity<Job>, Subscription)>,
    /// Whose output size and preset the FSR check reads, and whose upscaler a build chooses.
    title_id: Option<String>,
}

impl ExtrasView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| {
            this.check_fsr(cx);
            this.watch_fsr_job(cx);
        })
        .detach();
        let env = cx.new(|cx| TextInput::new(Text::new("ps.extras.env.placeholder"), cx));
        cx.subscribe(&env, |this, env, event, cx| {
            if matches!(event, InputEvent::Submit) {
                let value = env.read(cx).text().trim().to_owned();
                this.save_env(value, cx);
            }
        })
        .detach();
        let mut this = Self {
            ps,
            fsr_problem: None,
            checked_at: None,
            message: None,
            env,
            sys_modules: None,
            fsr_job: None,
            title_id: None,
        };
        this.check_fsr(cx);
        this
    }

    pub fn show_title(&mut self, title_id: &str, cx: &mut Context<Self>) {
        if self.title_id.as_deref() == Some(title_id) {
            return;
        }
        self.title_id = Some(title_id.to_owned());
        self.checked_at = None;
        self.fsr_problem = None;
        self.message = None;
        self.check_fsr(cx);
        self.load_settings(title_id, cx);
    }

    /// Another title's text and switch go at once, so they are never saved as this one's.
    fn load_settings(&mut self, title_id: &str, cx: &mut Context<Self>) {
        self.env.update(cx, |input, cx| input.set_text(String::new(), cx));
        self.sys_modules = None;
        let title_id = title_id.to_owned();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn({
                    let title_id = title_id.clone();
                    async move {
                        LauncherSettings::of_title(&decanter_ps::paths::active(), &title_id).map(|settings| {
                            let env = settings.text("extra_env").map(str::to_owned).unwrap_or_default();
                            let sys_modules = (
                                settings.uses_sys_modules(&title_id),
                                settings.text("sys_modules_dir").is_some(),
                            );
                            (env, sys_modules)
                        })
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.title_id.as_deref() != Some(title_id.as_str()) {
                    return;
                }
                match loaded {
                    Ok((env, sys_modules)) => {
                        this.sys_modules = Some(sys_modules);
                        this.env.update(cx, |input, cx| input.set_text(env, cx));
                        cx.notify();
                    }
                    Err(err) => {
                        this.message = Some((Text::new("ps.scope.read_error").arg("error", err), true));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
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
        let Some(title_id) = self.title_id.clone() else {
            return;
        };
        let revision = self.ps.read(cx).fsr_revision();
        if self.checked_at == Some(revision) {
            return;
        }
        self.checked_at = Some(revision);
        let task = cx.background_spawn(async move {
            let values = Values::load(&decanter_ps::paths::active(), &title_id)
                .inspect_err(|err| tracing::warn!(title_id, %err, "FSR 4.1.1 check without the title's settings"))
                .ok();
            let setting = |key| {
                decanter_ps::bbport::GROUPS
                    .iter()
                    .flat_map(|group| group.settings)
                    .find(|setting| setting.key == key)
                    .and_then(|setting| values.as_ref()?.get(setting))
            };
            let output = setting("output_res").unwrap_or_else(|| "1920x1080".to_owned());
            let preset = setting("preset").unwrap_or_else(|| "4".to_owned());
            tools::fsr411_problem(&decanter_ps::paths::active().fsr411(), &output, &preset)
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
        let Some(title_id) = self.title_id.clone() else {
            return;
        };
        let task = cx.background_spawn(async move {
            LauncherSettings::edit_title(&decanter_ps::paths::active(), &title_id, |settings| {
                settings.set_text("extra_env", &value);
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.message = Some(match result {
                    Ok(()) => (Text::new("ps.extras.env.saved"), false),
                    Err(err) => (Text::new("ps.extras.env.save_error").arg("error", err), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn set_sys_modules(&mut self, is_on: bool, cx: &mut Context<Self>) {
        let Some(title_id) = self.title_id.clone() else {
            return;
        };
        if let Some((uses, _)) = &mut self.sys_modules {
            *uses = is_on;
        }
        let task = cx.background_spawn(async move {
            LauncherSettings::edit_title(&decanter_ps::paths::active(), &title_id, |settings| {
                settings.set_flag(SYS_MODULES, is_on);
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.message = result
                    .err()
                    .map(|err| (Text::new("ps.extras.sys_modules.save_error").arg("error", err), true));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn sys_modules_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (is_on, has_folder) = self.sys_modules?;
        let hint = if has_folder {
            t!("ps.extras.sys_modules.hint")
        } else {
            t!("ps.extras.sys_modules.no_folder")
        };
        let toggle = div()
            .id("ps-extras-sys-modules")
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .child(checkbox(is_on))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(t!("ps.extras.sys_modules.title"))
                    .child(theme::muted(hint).text_size(px(12.))),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.set_sys_modules(!is_on, cx)));
        Some(theme::compact_row(false).child(toggle).into_any_element())
    }

    /// Asks for the DLL, or for its loader when the DLL needs one.
    fn pick_dll(&mut self, upscaler: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        let title = if upscaler.is_none() {
            t!("ps.extras.fsr.pick_upscaler")
        } else {
            t!("ps.extras.fsr.pick_loader")
        };
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(title.as_ref())
                .add_filter("DLL", &["dll"])
                .pick_file()
                .await
            else {
                return;
            };
            let chosen = file.path().to_owned();
            let _ = this.update(cx, |this, cx| {
                let Some(title_id) = this.title_id.clone() else {
                    return;
                };
                let (upscaler, loader) = match upscaler {
                    Some(upscaler) => (upscaler, Some(chosen)),
                    None => (chosen, None),
                };
                this.message = Some((Text::new("ps.fsr.checking"), false));
                let view = cx.entity().downgrade();
                this.ps.update(cx, |ps, cx| {
                    ps.build_fsr411(upscaler, loader, title_id, cx, move |outcome, cx| {
                        let _ = view.update(cx, |this, cx| {
                            match outcome {
                                DllOutcome::Building => this.message = None,
                                DllOutcome::NeedsLoader(upscaler) => {
                                    this.message = Some((Text::new("ps.extras.fsr.needs_loader"), false));
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
        let preparing = self
            .ps
            .read(cx)
            .fsr_runner()
            .map(|(runner, progress)| runners::progress(*runner, progress).0);
        let is_preparing = preparing.is_some();
        let status = match (&job, preparing, &self.fsr_problem) {
            (Some(job), _, _) => match tools::fsr411_progress(job.read(cx)) {
                Fsr411Progress::Preparing => t!("ps.extras.fsr.progress.preparing"),
                Fsr411Progress::Recording { done, total } => {
                    t!("ps.extras.fsr.progress.recording", done = done, total = total)
                }
                Fsr411Progress::Translating => t!("ps.extras.fsr.progress.translating"),
            },
            (None, Some(preparing), _) => preparing,
            (None, None, None) => t!("ps.extras.fsr.checking"),
            (None, None, Some(None)) => t!("ps.extras.fsr.ready"),
            (None, None, Some(Some(file))) => t!(
                "ps.extras.fsr.missing",
                problem = t!("ps.extras.fsr.missing_file", file = file)
            ),
        };
        let action = if job.is_some() {
            button("fsr411", ButtonKind::Secondary, t!("common.cancel"), false)
                .on_click(cx.listener(|this, _, _, cx| this.ps.update(cx, |ps, cx| ps.stop(&JobKey::Fsr411, cx))))
        } else if is_preparing {
            button("fsr411", ButtonKind::Secondary, t!("common.cancel"), false)
                .on_click(cx.listener(|this, _, _, cx| this.ps.update(cx, Ps::cancel_fsr_runner)))
        } else {
            button("fsr411", ButtonKind::Secondary, t!("ps.extras.fsr.pick"), false)
                .on_click(cx.listener(|this, _, _, cx| this.pick_dll(None, cx)))
        };
        theme::compact_row(false)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(t!("ps.extras.fsr.title"))
                    .child(theme::muted(status).text_size(px(12.))),
            )
            .child(action)
            .into_any_element()
    }
}

impl Render for ExtrasView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fsr_offered = !Caps::get(cx).ps_option_hidden("upscaler", "fsr411");
        let fsr_row = fsr_offered.then(|| self.fsr_row(cx));
        let sys_modules_row = self.sys_modules_row(cx);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::h3(t!("ps.extras.title")))
            .children(
                self.message
                    .as_ref()
                    .map(|(text, error)| if *error { theme::error(text) } else { theme::muted(text) }),
            )
            .child(
                theme::card().children(fsr_row).children(sys_modules_row).child(
                    theme::compact_row(true)
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .child(t!("ps.extras.env.title"))
                                .child(theme::muted(t!("ps.extras.env.hint")).text_size(px(12.))),
                        )
                        .child(div().flex_1().min_w_0().child(self.env.clone())),
                ),
            )
    }
}
