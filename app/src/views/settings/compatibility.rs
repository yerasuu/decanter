//! The Proton every game runs on unless it pins one of its own. Per-game
//! pinning and prefix resets live in each game's own Propiedades instead
//! (Steam's own shape: that's a per-game concern, not this screen's).
//! Below it, the tools decanter fetches for running games and ports,
//! checked against GitHub when the user asks.

use std::borrow::Cow;
use std::path::PathBuf;

use decanter_core::capabilities::Feature;
use decanter_core::runner::{Pin, Runner, Status};
use gpui::{Context, Div, Entity, IntoElement, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::game_tools::GameToolDefaults;
use super::{column, field_row, inline};
use crate::capabilities::Caps;
use crate::releases::{self, Release, ReleaseError};
use crate::runners;
use crate::theme;
use crate::widgets::{ButtonKind, Select, button};

type Latest = Result<Option<Release>, ReleaseError>;

struct ToolChecks {
    umu_run: Latest,
    nix_portable: Latest,
    umu_proton: Latest,
    /// Whether umu already downloaded `umu_proton`'s release into Steam's compat folder.
    umu_proton_installed: bool,
}

struct GeProtonBuild {
    pin: Pin,
    path: PathBuf,
    /// Measured off the UI thread, since the folder holds thousands of files.
    size: Option<u64>,
}

pub struct CompatibilityTab {
    runners: Vec<String>,
    current: Option<String>,
    checking: bool,
    tools: Option<ToolChecks>,
    game_tools: Entity<GameToolDefaults>,
    ge_proton: Vec<GeProtonBuild>,
    removing_ge_proton: bool,
    ge_proton_error: Option<String>,
}

impl CompatibilityTab {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            game_tools: cx.new(|_| GameToolDefaults::new()),
            runners: Vec::new(),
            current: None,
            checking: false,
            tools: None,
            ge_proton: Vec::new(),
            removing_ge_proton: false,
            ge_proton_error: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.runners = decanter_runtime::runner::status(Runner::Proton)
            .installed()
            .iter()
            .map(|installed| installed.pin.to_string())
            .collect();
        self.current = crate::core::db()
            .inspect_err(|err| tracing::warn!(error = %err, "failed to open db, default proton unknown"))
            .ok()
            .and_then(|db| {
                db.default_proton()
                    .inspect_err(|err| tracing::warn!(error = %err, "failed to read default proton setting"))
                    .ok()
                    .flatten()
            });
        self.ge_proton = match decanter_runtime::runner::status(Runner::GeProton) {
            Status::Installed(installed) => installed
                .into_iter()
                .map(|build| GeProtonBuild {
                    pin: build.pin,
                    path: build.path,
                    size: None,
                })
                .collect(),
            Status::Unsupported | Status::Absent => Vec::new(),
        };
        for build in &self.ge_proton {
            let (pin, path) = (build.pin.to_string(), build.path.clone());
            cx.spawn(async move |this, cx| {
                let size = cx
                    .background_spawn(async move { decanter_core::directory_size(&path) })
                    .await
                    .inspect_err(|err| tracing::warn!(%pin, %err, "could not measure GE-Proton"))
                    .ok();
                let _ = this.update(cx, |this, cx| {
                    if let Some(build) = this.ge_proton.iter_mut().find(|build| build.pin.to_string() == pin) {
                        build.size = size;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn remove_ge_proton(&mut self, pin: Pin, cx: &mut Context<Self>) {
        if self.removing_ge_proton {
            return;
        }
        self.removing_ge_proton = true;
        self.ge_proton_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let removed = decanter_runtime::runner::remove(Runner::GeProton, &pin).await;
            let _ = this.update(cx, |this, cx| {
                this.removing_ge_proton = false;
                match removed {
                    Ok(()) => tracing::info!(%pin, "GE-Proton removed by the user"),
                    Err(err) => {
                        tracing::warn!(%pin, %err, "could not remove GE-Proton");
                        this.ge_proton_error =
                            Some(t!("settings.compatibility.ge_proton.error", error = err).into_owned());
                    }
                }
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_default(&mut self, chosen: &str, cx: &mut Context<Self>) {
        let chosen = (!chosen.is_empty()).then_some(chosen);
        match crate::core::db() {
            Ok(db) => match db.set_default_proton(chosen) {
                Ok(()) => {
                    tracing::info!(proton = chosen, "default proton pin changed");
                    self.reload(cx);
                    cx.notify();
                }
                Err(err) => tracing::warn!(proton = chosen, error = %err, "failed to save default proton setting"),
            },
            Err(err) => {
                tracing::warn!(proton = chosen, error = %err, "failed to open db, default proton not changed");
            }
        }
    }

    /// Also reads Steam's compat folder again, so a Proton installed since the tab opened shows up.
    fn check_tools(&mut self, cx: &mut Context<Self>) {
        self.checking = true;
        self.reload(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (umu_run, nix_portable, umu_proton) = futures::join!(
                runners::latest(Runner::Umu),
                releases::latest(decanter_nix::NIX_PORTABLE_REPO),
                runners::latest(Runner::Proton),
            );
            for (tool, result) in [
                ("umu-run", &umu_run),
                ("nix-portable", &nix_portable),
                ("UMU-Proton", &umu_proton),
            ] {
                match result {
                    Ok(latest) => tracing::info!(
                        tool,
                        latest = latest.as_ref().map(|release| release.tag.as_str()),
                        "checked tool release"
                    ),
                    Err(err) => tracing::warn!(tool, error = %err, "could not check tool release"),
                }
            }
            let umu_proton_installed = umu_proton.as_ref().is_ok_and(|latest| {
                latest.as_ref().is_some_and(|release| {
                    decanter_runtime::runner::status(Runner::Proton)
                        .installed()
                        .iter()
                        .any(|installed| installed.pin.to_string() == release.tag)
                })
            });
            let _ = this.update(cx, |this, cx| {
                this.tools = Some(ToolChecks {
                    umu_run,
                    nix_portable,
                    umu_proton,
                    umu_proton_installed,
                });
                this.checking = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn ge_proton_section(&self, cx: &Context<Self>) -> Div {
        column()
            .child(theme::h2(t!("settings.compatibility.ge_proton.title")))
            .child(theme::muted(t!("settings.compatibility.ge_proton.intro")))
            .children(self.ge_proton.iter().enumerate().map(|(index, build)| {
                let pin = build.pin.clone();
                let removing = self.removing_ge_proton;
                let text = match build.size {
                    Some(size) => t!(
                        "settings.compatibility.ge_proton.installed",
                        tag = build.pin,
                        size = crate::format::bytes(size)
                    ),
                    None => t!("settings.compatibility.ge_proton.measuring", tag = build.pin),
                };
                field_row().items_center().child(div().flex_1().child(text)).child(
                    button(
                        ("ge-proton-remove", index),
                        ButtonKind::Secondary,
                        t!("settings.compatibility.ge_proton.remove"),
                        removing,
                    )
                    .when(!removing, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.remove_ge_proton(pin.clone(), cx)))
                    }),
                )
            }))
            .children(self.ge_proton_error.clone().map(theme::error))
    }

    fn tools_section(&self, cx: &Context<Self>) -> Div {
        let checking = self.checking;
        let tools = self.tools.as_ref();
        column()
            .child(theme::h2(t!("settings.compatibility.tools.title")))
            .child(theme::muted(t!("settings.compatibility.tools.intro")))
            .child(pinned_line(
                "umu-run",
                decanter_runtime::UMU_LAUNCHER_VERSION,
                tools.map(|tools| &tools.umu_run),
            ))
            .child(pinned_line(
                "nix-portable",
                decanter_nix::NIX_PORTABLE_VERSION,
                tools.map(|tools| &tools.nix_portable),
            ))
            .child(proton_line(tools))
            .child(
                inline(
                    button(
                        "check-tools",
                        ButtonKind::Secondary,
                        if checking {
                            t!("settings.compatibility.tools.checking")
                        } else {
                            t!("settings.compatibility.tools.check")
                        },
                        checking,
                    )
                    .when(!checking, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.check_tools(cx)))
                    }),
                )
                .mt_2(),
            )
    }
}

fn line(text: Cow<'static, str>, is_error: bool) -> Div {
    if is_error {
        theme::error(text)
    } else {
        theme::muted(text)
    }
    .mt_1()
}

/// A tool decanter downloads at a version bumped only after testing, so a newer
/// release is news, not something the user can apply from here.
fn pinned_line(tool: &'static str, pinned: &'static str, latest: Option<&Latest>) -> Div {
    match latest {
        None => line(
            t!("settings.compatibility.tools.pinned", tool = tool, version = pinned),
            false,
        ),
        Some(Ok(Some(release))) if release.tag == pinned => line(
            t!("settings.compatibility.tools.current", tool = tool, version = pinned),
            false,
        ),
        Some(Ok(Some(release))) => line(
            t!(
                "settings.compatibility.tools.newer",
                tool = tool,
                version = pinned,
                latest = release.tag
            ),
            false,
        ),
        Some(Ok(None)) => line(
            t!(
                "settings.compatibility.tools.unpublished",
                tool = tool,
                version = pinned
            ),
            false,
        ),
        Some(Err(err)) => line(
            t!(
                "settings.compatibility.tools.error",
                tool = tool,
                version = pinned,
                error = err.localized()
            ),
            true,
        ),
    }
}

fn proton_line(tools: Option<&ToolChecks>) -> Div {
    match tools.map(|tools| (&tools.umu_proton, tools.umu_proton_installed)) {
        None => line(t!("settings.compatibility.tools.proton.unchecked"), false),
        Some((Ok(Some(release)), true)) => line(
            t!("settings.compatibility.tools.proton.installed", tag = release.tag),
            false,
        ),
        Some((Ok(Some(release)), false)) => line(
            t!("settings.compatibility.tools.proton.pending", tag = release.tag),
            false,
        ),
        Some((Ok(None), _)) => line(t!("settings.compatibility.tools.proton.unpublished"), false),
        Some((Err(err), _)) => line(
            t!("settings.compatibility.tools.proton.error", error = err.localized()),
            true,
        ),
    }
}

impl Render for CompatibilityTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(reason) = Caps::get(cx).reason(Feature::Proton) {
            return column().child(theme::muted(reason));
        }
        // A pin naming a Proton that's since been uninstalled matches no
        // option, and a native select falls back to its first one then —
        // so does this, rather than showing an empty trigger.
        let selected = self
            .current
            .as_deref()
            .filter(|current| self.runners.iter().any(|runner| runner == current))
            .unwrap_or_default()
            .to_owned();
        let options = std::iter::once((SharedString::default(), t!("settings.compatibility.automatic").into())).chain(
            self.runners
                .iter()
                .map(|runner| (SharedString::from(runner.clone()), SharedString::from(runner.clone()))),
        );
        let entity = cx.entity();

        column()
            .child(theme::muted(t!("settings.compatibility.intro")))
            .child(theme::h2(t!("settings.compatibility.default")))
            .child(
                field_row().child(
                    Select::new("default-proton", options, selected).on_change(move |chosen, _, cx| {
                        entity.update(cx, |this, cx| this.set_default(chosen, cx));
                    }),
                ),
            )
            .when(self.runners.is_empty(), |column| {
                column.child(theme::muted(t!("settings.compatibility.none_installed")))
            })
            .child(self.game_tools.clone())
            .when(!self.ge_proton.is_empty(), |column| {
                column.child(self.ge_proton_section(cx))
            })
            .child(self.tools_section(cx))
    }
}
