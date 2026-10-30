//! Mods: bbport's mods in load order and its external patches, each
//! switchable, from folders that can be moved, all of them the title's own
//! ([`ModFiles`]). Read with bbport's own discovery rules, so bbport is made
//! ready first if it isn't; where `psport.exe` runs directly, the one Play
//! would start answers instead.

use std::path::PathBuf;

use decanter_ps::bbport::mods::{FOLDER_KEYS, ModFiles};
use decanter_ps::bbport::tools::{self, ExternalPatch, Lister, ModProfile, PatchProfile};
use decanter_ps::bbport::{LauncherSettings, direct};
#[cfg(windows)]
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::scope_note;
use crate::i18n::Text;
use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

struct Found {
    files: ModFiles,
    mods: Result<Vec<String>, String>,
    patches: Result<Vec<ExternalPatch>, String>,
    mod_profile: ModProfile,
    patch_profile: PatchProfile,
}

pub struct ModsView {
    ps: Entity<Ps>,
    title_id: Option<String>,
    found: Option<Result<Found, Text>>,
    error: Option<Text>,
}

impl ModsView {
    pub const fn new(ps: Entity<Ps>) -> Self {
        Self {
            ps,
            title_id: None,
            found: None,
            error: None,
        }
    }

    /// Reads `title_id`'s mods and patches unless they are the ones shown; another title's go at
    /// once, so nothing is shown or saved for the wrong one meanwhile.
    pub fn show_title(&mut self, title_id: &str, cx: &mut Context<Self>) {
        if self.title_id.as_deref() == Some(title_id) && self.found.is_some() {
            return;
        }
        self.title_id = Some(title_id.to_owned());
        self.found = None;
        self.error = None;
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(title_id) = self.title_id.clone() else {
            return;
        };
        let root = decanter_ps::paths::active();
        let prepared = (!direct::IN_USE).then(|| self.ps.update(cx, |ps, cx| ps.prepare(Port::Bbport, &root, cx)));
        cx.spawn(async move |this, cx| {
            let toolchain = match prepared {
                Some(prepared) => match prepared.await {
                    Ok(toolchain) => Some(toolchain),
                    Err(err) => {
                        let _ = this.update(cx, |this, cx| {
                            if this.title_id.as_deref() == Some(title_id.as_str()) {
                                this.found = Some(Err(Text::new("ps.errors.bbport_not_ready").text("error", err)));
                                cx.notify();
                            }
                        });
                        return;
                    }
                },
                None => None,
            };
            let read = cx.background_spawn({
                let root = root.clone();
                let title_id = title_id.clone();
                async move {
                    let settings = LauncherSettings::of_title(&root, &title_id)?;
                    let files = ModFiles::of_title(&root, &title_id, &settings)?;
                    let profiles = (
                        ModProfile::load(&files.mods_config),
                        PatchProfile::load(&files.patches_config),
                    );
                    std::io::Result::Ok((settings, files, profiles))
                }
            });
            #[cfg_attr(
                not(windows),
                expect(
                    unused_variables,
                    reason = "only a direct launch picks its psport.exe by the settings"
                )
            )]
            let (settings, files, (mod_profile, patch_profile)) = match read.await {
                Ok(read) => read,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.title_id.as_deref() == Some(title_id.as_str()) {
                            this.found = Some(Err(Text::new("ps.scope.read_error").arg("error", err)));
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let lister = match toolchain {
                Some(toolchain) => Ok(Lister::Toolchain(toolchain)),
                #[cfg(windows)]
                None => direct_lister(&root, &settings).await,
                #[cfg(not(windows))]
                None => Err(Text::new("ps.errors.builds_unavailable")),
            };
            let lister = match lister {
                Ok(lister) => lister,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.title_id.as_deref() == Some(title_id.as_str()) {
                            this.found = Some(Err(err));
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let mods = tools::discover_mods(&root, &lister, &files.mods_dir).await;
            let patches = tools::discover_patches(&root, &lister, &files.patches_dir).await;
            let _ = this.update(cx, |this, cx| {
                if this.title_id.as_deref() != Some(title_id.as_str()) {
                    return;
                }
                this.found = Some(Ok(Found {
                    files,
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
        let (profile, path) = (found.mod_profile.clone(), found.files.mods_config.clone());
        let task = cx.background_spawn(async move { profile.save(&path) });
        self.after_save(task, "mods.json", cx);
    }

    fn save_patches(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(found)) = &self.found else {
            return;
        };
        let (profile, path) = (found.patch_profile.clone(), found.files.patches_config.clone());
        let task = cx.background_spawn(async move { profile.save(&path) });
        self.after_save(task, "patches.json", cx);
    }

    fn after_save(&self, task: gpui::Task<std::io::Result<()>>, file: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("ps.mods.save_error").arg("file", file).arg("error", err));
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
        let Some(title_id) = self.title_id.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return;
            };
            let path = folder.path().to_string_lossy().into_owned();
            let saved = cx
                .background_spawn(async move {
                    LauncherSettings::edit_title(&decanter_ps::paths::active(), &title_id, |settings| {
                        settings.set_text(key, &path);
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| match saved {
                Ok(()) => this.refresh(cx),
                Err(err) => {
                    this.error = Some(Text::new("ps.mods.folder_error").arg("error", err));
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

/// The folder whose `psport.exe` Play would start, checked again as Play checks it; none yet when
/// neither a folder, a build nor a package is chosen, which only Jugar or Ports fetch.
#[cfg(windows)]
async fn direct_lister(root: &Root, settings: &LauncherSettings) -> Result<Lister, Text> {
    let Some(dir) = crate::ps_play::direct::chosen_dir(root, settings).await else {
        return Err(Text::new("ps.mods.no_port"));
    };
    match tokio::task::spawn_blocking(move || direct::Folder::try_new(dir)).await {
        Ok(Ok(folder)) => Ok(Lister::Direct(folder)),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "the psport.exe folder can't list mods");
            Err(Text::new("ps.play.direct.bad_folder").text("error", crate::state::ps_folder_error(&err)))
        }
        Err(err) => Err(Text::new("ps.errors.environment").arg("error", err)),
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
            Some(Err(err)) => return theme::error(err).into_any_element(),
            Some(Ok(found)) => found,
        };
        let mods = self.mods_rows(found, cx);
        let patches = self.patch_rows(found, cx);
        let [mods_key, patches_key] = FOLDER_KEYS;
        let mods_folder = self.folder_row(
            t!("ps.mods.mods.folder").into(),
            found.files.mods_dir.clone(),
            mods_key,
            cx,
        );
        let patches_folder = self.folder_row(
            t!("ps.mods.patches.folder").into(),
            found.files.patches_dir.clone(),
            patches_key,
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
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(theme::muted(t!("ps.mods.description")))
                            .child(theme::muted(scope_note(found.files.scope))),
                    )
                    .child(
                        button("mods-refresh", ButtonKind::Secondary, t!("ps.mods.refresh"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .children(self.error.as_ref().map(theme::error))
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
