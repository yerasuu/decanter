//! Mods: bbport's mods in load order and its external patches, each
//! switchable, from folders that can be moved. Read with bbport's own
//! discovery rules, so bbport is made ready first if it isn't.

use std::path::PathBuf;

use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::tools::{self, ExternalPatch, ModProfile, PatchProfile};
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

struct Found {
    mods_dir: PathBuf,
    patches_dir: PathBuf,
    mods: Result<Vec<String>, String>,
    patches: Result<Vec<ExternalPatch>, String>,
    mod_profile: ModProfile,
    patch_profile: PatchProfile,
}

pub struct ModsView {
    ps: Entity<Ps>,
    found: Option<Result<Found, String>>,
    error: Option<String>,
}

impl ModsView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            ps,
            found: None,
            error: None,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = decanter_ps::paths::active();
        let prepared = self.ps.update(cx, |ps, cx| ps.prepare(Port::Bbport, &root, cx));
        cx.spawn(async move |this, cx| {
            let toolchain = match prepared.await {
                Ok(toolchain) => toolchain,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.found = Some(Err(t!("ps.errors.bbport_not_ready", error = err).into_owned()));
                        cx.notify();
                    });
                    return;
                }
            };
            let read = cx.background_spawn({
                let root = root.clone();
                async move {
                    let settings = LauncherSettings::load(&root);
                    let data = root.bbport_data();
                    let dir = |key: &str, fallback: &str| {
                        settings.text(key).map_or_else(|| data.join(fallback), PathBuf::from)
                    };
                    (
                        dir("mods_dir", "mods"),
                        dir("patches_dir", "patches"),
                        ModProfile::load(&ModProfile::path(&root)),
                        PatchProfile::load(&PatchProfile::path(&root)),
                    )
                }
            });
            let (mods_dir, patches_dir, mod_profile, patch_profile) = read.await;
            let mods = tools::discover_mods(&root, &toolchain, &mods_dir).await;
            let patches = tools::discover_patches(&root, &toolchain, &patches_dir).await;
            let _ = this.update(cx, |this, cx| {
                this.found = Some(Ok(Found {
                    mods_dir,
                    patches_dir,
                    mods,
                    patches,
                    mod_profile,
                    patch_profile,
                }));
                cx.notify();
            });
        })
        .detach();
    }

    fn save_mods(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(found)) = &self.found else {
            return;
        };
        let profile = found.mod_profile.clone();
        let task = cx.background_spawn(async move { profile.save(&ModProfile::path(&decanter_ps::paths::active())) });
        self.after_save(task, "mods.json", cx);
    }

    fn save_patches(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(found)) = &self.found else {
            return;
        };
        let profile = found.patch_profile.clone();
        let task = cx.background_spawn(async move { profile.save(&PatchProfile::path(&decanter_ps::paths::active())) });
        self.after_save(task, "patches.json", cx);
    }

    fn after_save(&self, task: gpui::Task<std::io::Result<()>>, file: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| t!("ps.mods.save_error", file = file, error = err).into_owned());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn move_mod(&mut self, name: &str, delta: isize, cx: &mut Context<Self>) {
        let Some(Ok(found)) = &mut self.found else {
            return;
        };
        let Ok(available) = &found.mods else {
            return;
        };
        let mut order = found.mod_profile.ordered(available);
        let Some(at) = order.iter().position(|other| other == name) else {
            return;
        };
        let Some(to) = at.checked_add_signed(delta).filter(|&to| to < order.len()) else {
            return;
        };
        order.swap(at, to);
        found.mod_profile.order = order;
        self.save_mods(cx);
    }

    fn toggle_mod(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(Ok(found)) = &mut self.found else {
            return;
        };
        let disabled = &mut found.mod_profile.disabled;
        if disabled.contains(&name) {
            disabled.retain(|other| *other != name);
        } else {
            disabled.push(name);
        }
        self.save_mods(cx);
    }

    fn toggle_patch(&mut self, patch: &ExternalPatch, cx: &mut Context<Self>) {
        let Some(Ok(found)) = &mut self.found else {
            return;
        };
        let active = found.patch_profile.is_active(patch);
        found.patch_profile.set(patch, !active);
        self.save_patches(cx);
    }

    fn pick_dir(&self, key: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return;
            };
            let path = folder.path().to_string_lossy().into_owned();
            let saved = cx
                .background_spawn(async move {
                    LauncherSettings::edit(&decanter_ps::paths::active(), |settings| settings.set_text(key, &path))
                })
                .await;
            let _ = this.update(cx, |this, cx| match saved {
                Ok(()) => this.refresh(cx),
                Err(err) => {
                    this.error = Some(t!("ps.mods.folder_error", error = err).into_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn folder_row(&self, label: SharedString, dir: PathBuf, key: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let shown = dir.display().to_string();
        theme::compact_row(false)
            .child(div().flex_none().child(label))
            .child(
                theme::mono()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(shown),
            )
            .child(
                button(
                    SharedString::from(format!("open-{key}")),
                    ButtonKind::Secondary,
                    t!("ps.mods.open"),
                    false,
                )
                .on_click(move |_, _, _| {
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(err) = crate::desktop::open(&dir) {
                        tracing::warn!(dir = %dir.display(), %err, "could not open the folder");
                    }
                }),
            )
            .child(
                button(
                    SharedString::from(format!("change-{key}")),
                    ButtonKind::Secondary,
                    t!("ps.mods.change"),
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.pick_dir(key, cx))),
            )
            .into_any_element()
    }

    fn mods_rows(&self, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match &found.mods {
            Err(err) => vec![row_text(t!("ps.mods.mods.read_error", error = err).into_owned())],
            Ok(available) if available.is_empty() => vec![row_text(t!("ps.mods.mods.empty").into_owned())],
            Ok(available) => {
                let ordered = found.mod_profile.ordered(available);
                let last = ordered.len().saturating_sub(1);
                ordered
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let on = !found.mod_profile.disabled.contains(&name);
                        let (up, down, toggle) = (name.clone(), name.clone(), name.clone());
                        theme::compact_row(index == last)
                            .gap_2()
                            .child(
                                div()
                                    .id(("mod-on", index))
                                    .p_1()
                                    .cursor_pointer()
                                    .child(checkbox(on))
                                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_mod(toggle.clone(), cx))),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .child(
                                button(("mod-up", index), ButtonKind::Secondary, "↑", false)
                                    .on_click(cx.listener(move |this, _, _, cx| this.move_mod(&up, -1, cx))),
                            )
                            .child(
                                button(("mod-down", index), ButtonKind::Secondary, "↓", false)
                                    .on_click(cx.listener(move |this, _, _, cx| this.move_mod(&down, 1, cx))),
                            )
                            .into_any_element()
                    })
                    .collect()
            }
        }
    }

    fn patch_rows(&self, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match &found.patches {
            Err(err) => vec![row_text(t!("ps.mods.patches.read_error", error = err).into_owned())],
            Ok(patches) if patches.is_empty() => vec![row_text(t!("ps.mods.patches.empty").into_owned())],
            Ok(patches) => {
                let last = patches.len().saturating_sub(1);
                patches
                    .iter()
                    .enumerate()
                    .map(|(index, patch)| {
                        let on = found.patch_profile.is_active(patch);
                        let file = patch.key.split('/').next().unwrap_or_default();
                        let detail = if patch.author.is_empty() {
                            file.to_owned()
                        } else {
                            format!("{file} · {}", patch.author)
                        };
                        let toggled = patch.clone();
                        theme::compact_row(index == last)
                            .child(
                                div()
                                    .id(("patch-on", index))
                                    .p_1()
                                    .cursor_pointer()
                                    .child(checkbox(on))
                                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_patch(&toggled, cx))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(patch.name.clone())
                                    .child(theme::muted(detail).text_size(px(12.)))
                                    .when(!patch.note.is_empty(), |text| {
                                        text.child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(rgb(theme::TEXT_DIM))
                                                .child(patch.note.clone()),
                                        )
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect()
            }
        }
    }
}

fn row_text(text: String) -> AnyElement {
    theme::compact_row(true)
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text)
        .into_any_element()
}

impl Render for ModsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let found = match &self.found {
            None => return theme::muted(t!("ps.mods.loading")).into_any_element(),
            Some(Err(err)) => return theme::error(err.clone()).into_any_element(),
            Some(Ok(found)) => found,
        };
        let mods = self.mods_rows(found, cx);
        let patches = self.patch_rows(found, cx);
        let mods_folder = self.folder_row(t!("ps.mods.mods.folder").into(), found.mods_dir.clone(), "mods_dir", cx);
        let patches_folder = self.folder_row(
            t!("ps.mods.patches.folder").into(),
            found.patches_dir.clone(),
            "patches_dir",
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(theme::muted(t!("ps.mods.description")).flex_1())
                    .child(
                        button("mods-refresh", ButtonKind::Secondary, t!("ps.mods.refresh"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .children(self.error.clone().map(theme::error))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(theme::h3(t!("ps.mods.mods.title")))
                    .child(theme::muted(t!("ps.mods.mods.note")).text_size(px(12.)))
                    .child(theme::card().child(mods_folder).children(mods)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(theme::h3(t!("ps.mods.patches.title")))
                    .child(theme::muted(t!("ps.mods.patches.note")).text_size(px(12.)))
                    .child(theme::card().child(patches_folder).children(patches)),
            )
            .into_any_element()
    }
}
