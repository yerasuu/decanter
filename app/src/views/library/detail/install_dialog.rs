use std::borrow::Cow;
use std::path::PathBuf;
use std::rc::Rc;

use decanter_gog::Branch;
use gpui::{
    AnyElement, BoxShadow, Context, Div, SharedString, Task, WeakEntity, Window, div, point, prelude::*, px, rgb,
};
use rust_i18n::t;

use super::DetailView;
use crate::state::AppState;
use crate::theme;
use crate::views::library::actions;
use crate::views::library::install::InstallRequest;
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button, menu};

pub(super) struct InstallDialog {
    /// Spent by the confirm click, or by the install that starts at once
    /// when there turns out to be nothing to choose.
    request: Option<InstallRequest>,
    /// Every library folder the install could go in, with its free space.
    folders: Vec<(PathBuf, Option<u64>)>,
    /// The folder picked; `None` is the main one.
    folder: Option<PathBuf>,
    /// Library folders whose disk isn't plugged in — the game may already
    /// be installed on one of them.
    offline: Vec<PathBuf>,
    branches: Option<Result<Vec<Branch>, String>>,
    chosen: Option<String>,
    /// The languages the chosen branch's build splits its files by; empty
    /// when it doesn't, and also when they couldn't be read, which leaves the
    /// install to pick (#1355).
    languages: Option<Vec<String>>,
    /// The language picked, preselected to the game's saved one or the app's.
    language: Option<decanter_gog::language::Resolved>,
    /// Dropping the dialog drops these, so a list still in flight for a
    /// closed dialog is abandoned rather than written back.
    _fetch: Task<()>,
    _languages_fetch: Task<()>,
}

impl InstallDialog {
    /// A dialog with neither another library folder nor another branch or
    /// language to pick (nor a disk or a missing language to warn about)
    /// would only ask to confirm what the install button already said, so it
    /// is never shown.
    fn has_choice(&self) -> bool {
        let named_branch = matches!(&self.branches, Some(Ok(list)) if list.iter().any(|branch| branch.name.is_some()));
        let languages = self.languages.as_ref().map_or(0, Vec::len);
        let lacks_language = self.language.as_ref().is_some_and(|picked| !picked.is_wanted);
        self.folders.len() > 1 || !self.offline.is_empty() || named_branch || languages > 1 || lacks_language
    }

    fn is_loaded(&self) -> bool {
        self.branches.is_some() && self.languages.is_some()
    }
}

/// Reads the languages `branch`'s build offers into the open dialog, then
/// starts the install at once if there turns out to be nothing to choose.
fn fetch_languages(
    account_id: String,
    product_id: String,
    branch: Option<String>,
    cx: &mut Context<DetailView>,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let offered = decanter_gog::offered_languages(&account_id, &product_id, branch.as_deref(), None)
            .await
            .inspect_err(|err| tracing::warn!(account_id, product_id, error = %err, "failed to read the languages a build offers"))
            .unwrap_or_default();
        let wanted = crate::core::gog_languages(&account_id, &product_id);
        let _ = this.update(cx, |this, cx| {
            let Some(dialog) = &mut this.local.dialog else {
                return;
            };
            dialog.language = decanter_gog::language::resolve(&wanted, &offered);
            dialog.languages = Some(offered);
            settle(this, cx);
            cx.notify();
        });
    })
}

/// Once everything the dialog asks about has loaded, an install with
/// nothing to choose starts without showing it.
fn settle(this: &mut DetailView, cx: &mut Context<DetailView>) {
    let Some(dialog) = &this.local.dialog else {
        return;
    };
    if !dialog.is_loaded() || dialog.has_choice() {
        return;
    }
    if let Some(mut dialog) = this.local.dialog.take()
        && let Some(mut request) = dialog.request.take()
    {
        request.language = dialog.language.map(|picked| picked.tag);
        actions::start_install(request, cx);
    }
}

impl DetailView {
    pub(super) fn open_install_dialog(&mut self, request: InstallRequest, cx: &mut Context<Self>) {
        let (account_id, product_id) = (request.account_id.clone(), request.product_id.clone());
        let folders = crate::core::library_for_account(&account_id)
            .inspect_err(
                |err| tracing::warn!(account_id, error = %err, "failed to resolve library root for install dialog"),
            )
            .map(|library| library.roots())
            .unwrap_or_default()
            .into_iter()
            .map(|root| {
                let free = decanter_core::disk_usage(&root).map(|(free, _)| free);
                (root, free)
            })
            .collect();
        let fetch = cx.spawn(async move |this, cx| {
            tracing::debug!(account_id, product_id, "fetching gog branch list for install dialog");
            let result = decanter_gog::list_branches(&account_id, &product_id)
                .await
                .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "failed to fetch branch list"))
                .map_err(|err| err.to_string());
            let _ = this.update(cx, |this, cx| {
                let Some(dialog) = &mut this.local.dialog else {
                    return;
                };
                dialog.branches = Some(result);
                settle(this, cx);
                cx.notify();
            });
        });
        let languages_fetch = fetch_languages(request.account_id.clone(), request.product_id.clone(), None, cx);
        let offline = AppState::get(cx).library.read(cx).offline_folders.clone();
        self.local.dialog = Some(InstallDialog {
            request: Some(request),
            folders,
            folder: None,
            offline,
            branches: None,
            chosen: None,
            languages: None,
            language: None,
            _fetch: fetch,
            _languages_fetch: languages_fetch,
        });
        cx.notify();
    }
}

/// Confirms an install before it starts, the way Steam does — which library
/// folder it goes in and which branch to install (mainline by default, same
/// as Steam defaults to no beta opt-in). `None` while there is nothing to
/// choose, which is when the install starts without asking.
pub(super) fn install_dialog(dialog: &InstallDialog, window: &Window, cx: &Context<DetailView>) -> Option<AnyElement> {
    if !dialog.is_loaded() || !dialog.has_choice() {
        return None;
    }
    let this = cx.entity().downgrade();
    let close: menu::Dismiss = Rc::new({
        let this = this.clone();
        move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.local.dialog = None;
                cx.notify();
            });
        }
    });
    let request = dialog.request.as_ref()?;
    let dlc_count = request.dlcs.len();
    let title = request.title.clone();

    let rows = install_rows(dialog, dlc_count);
    let folders = folder_picker(dialog, this.clone());
    let language = language_picker(dialog, this.clone());
    let branch = branch_picker(dialog, this);
    let field_row = || div().flex().gap(px(10.)).mb_3().max_w(px(520.));

    let confirm = cx.listener(|this, _, _, cx| {
        let dialog = this.local.dialog.take();
        cx.notify();
        if let Some(InstallDialog {
            request: Some(mut request),
            chosen,
            folder,
            language,
            ..
        }) = dialog
        {
            request.branch = chosen;
            request.library_root = folder;
            request.language = language.map(|picked| picked.tag);
            actions::start_install(request, cx);
        }
    });
    let cancel = Rc::clone(&close);
    let dismiss = Rc::clone(&close);
    let (folder_label, branch_label) = (t!("library.detail.install.folder"), t!("library.detail.install.branch"));
    let language_label = t!("library.detail.install.language");
    let language_note = language_note(dialog);
    let (install_label, cancel_label) = (t!("library.detail.install.button"), t!("common.cancel"));

    let panel = dialog_frame(window).child(dialog_header(&title, close)).child(
        div()
            .id("install-dialog-body")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .pt(px(20.))
            .px_6()
            .pb_6()
            .child(rows)
            .children(folders.map(|folders| field_row().child(div().child(folder_label)).child(folders)))
            .child(field_row().child(div().child(branch_label)).child(branch))
            .children(language.map(|language| field_row().child(div().child(language_label)).child(language)))
            .children(language_note.map(|note| div().mb_3().max_w(px(520.)).child(theme::muted(note))))
            .child(
                field_row()
                    .child(button("install-confirm", ButtonKind::Play, install_label, false).on_click(confirm))
                    .child(
                        button("install-dialog-cancel", ButtonKind::Secondary, cancel_label, false)
                            .on_click(move |_, window, cx| cancel(window, cx)),
                    ),
            ),
    );
    Some(menu::modal(panel, window, dismiss))
}

fn dialog_frame(window: &Window) -> Div {
    let viewport = window.viewport_size();
    div()
        .w(px(640.).min(viewport.width * 0.92))
        .max_h(viewport.height * 0.82)
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL_2))
        .border_1()
        .border_color(rgb(theme::LINE))
        .rounded(px(theme::RADIUS_LG))
        .shadow(vec![BoxShadow {
            color: gpui::black().opacity(0.5),
            offset: point(px(0.), px(24.)),
            blur_radius: px(64.),
            spread_radius: px(0.),
            inset: false,
        }])
}

fn dialog_header(title: &str, close: menu::Dismiss) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_6()
        .py(px(18.))
        .border_b_1()
        .border_color(rgb(theme::LINE))
        .child(
            theme::display(16.)
                .text_color(rgb(theme::TEXT_MUTED))
                .child(t!("library.detail.install.dialog_title", title = title).to_uppercase()),
        )
        .child(
            button("install-dialog-close", ButtonKind::Icon, "✕", false)
                .on_click(move |_, window, cx| close(window, cx)),
        )
}

/// `.properties-rows`: where the files go and which DLC rides along.
fn install_rows(dialog: &InstallDialog, dlc_count: usize) -> Div {
    let row = |label: Cow<'static, str>, value: String| {
        div()
            .flex()
            .justify_between()
            .gap_4()
            .text_size(px(13.))
            .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label))
            .child(div().child(value))
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .mb_4()
        .children(
            // With one folder there's nothing to pick; it's just said.
            (dialog.folders.len() == 1)
                .then(|| dialog.folders.first())
                .flatten()
                .map(|(root, _)| {
                    row(
                        t!("library.detail.install.location"),
                        root.join("common").display().to_string(),
                    )
                }),
        )
        .when(dlc_count > 0, |rows| {
            let value = if dlc_count == 1 {
                t!("library.detail.install.dlcs.one", count = dlc_count)
            } else {
                t!("library.detail.install.dlcs.other", count = dlc_count)
            };
            rows.child(row(t!("library.detail.install.dlc"), value.into_owned()))
        })
        .when(!dialog.offline.is_empty(), |rows| {
            let folders = dialog
                .offline
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            rows.child(theme::muted(t!("library.detail.install.offline", folders = folders)))
        })
}

/// Which library folder a new install goes in, with each one's free space —
/// only when the profile has more than one.
fn folder_picker(dialog: &InstallDialog, this: WeakEntity<DetailView>) -> Option<AnyElement> {
    if dialog.folders.len() < 2 {
        return None;
    }
    let options = dialog.folders.iter().map(|(root, free)| {
        let label = match free {
            Some(free) => t!(
                "library.detail.install.folder_free",
                folder = root.display(),
                free = crate::format::bytes(*free)
            )
            .into_owned(),
            None => root.display().to_string(),
        };
        (
            SharedString::from(root.to_string_lossy().into_owned()),
            SharedString::from(label),
        )
    });
    let current = dialog
        .folder
        .as_ref()
        .or_else(|| dialog.folders.first().map(|(root, _)| root))
        .map(|root| SharedString::from(root.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let main = dialog.folders.first().map(|(root, _)| root.clone());
    Some(
        Select::new("install-folder", options, current)
            .on_change(move |value, _, cx| {
                let picked = PathBuf::from(value.as_ref());
                let folder = (Some(&picked) != main.as_ref()).then_some(picked);
                let _ = this.update(cx, |this, cx| {
                    if let Some(dialog) = &mut this.local.dialog {
                        dialog.folder = folder;
                        cx.notify();
                    }
                });
            })
            .into_any_element(),
    )
}

/// Which language the game installs in, by its own name, among those the
/// build splits its files by; only when it offers more than one.
fn language_picker(dialog: &InstallDialog, this: WeakEntity<DetailView>) -> Option<AnyElement> {
    let offered = dialog.languages.as_ref().filter(|offered| offered.len() > 1)?;
    let options = offered.iter().map(|tag| {
        (
            SharedString::from(tag.clone()),
            SharedString::from(decanter_gog::language::native_name(tag)),
        )
    });
    let current = dialog
        .language
        .as_ref()
        .map(|picked| SharedString::from(picked.tag.clone()))
        .unwrap_or_default();
    Some(
        Select::new("install-language", options, current)
            .on_change(move |value, _, cx| {
                let _ = this.update(cx, |this, cx| {
                    if let Some(dialog) = &mut this.local.dialog {
                        dialog.language = Some(decanter_gog::language::Resolved::picked(value.to_string()));
                        cx.notify();
                    }
                });
            })
            .into_any_element(),
    )
}

/// Says the game isn't offered in the app's language, and which one it
/// installs in instead, while that fallback is still the one picked.
fn language_note(dialog: &InstallDialog) -> Option<String> {
    let picked = dialog.language.as_ref().filter(|picked| !picked.is_wanted)?;
    let app = crate::i18n::Language::from_code(&rust_i18n::locale())
        .map(|language| language.native_name().into_owned())
        .unwrap_or_default();
    Some(
        t!(
            "library.detail.install.language_missing",
            language = app,
            fallback = decanter_gog::language::native_name(&picked.tag)
        )
        .into_owned(),
    )
}

fn branch_picker(dialog: &InstallDialog, this: WeakEntity<DetailView>) -> AnyElement {
    match &dialog.branches {
        None => theme::muted(t!("common.loading")).into_any_element(),
        Some(Err(err)) => theme::error(err.clone()).into_any_element(),
        Some(Ok(list)) => {
            let options = std::iter::once((
                SharedString::default(),
                SharedString::from(t!("library.detail.install.mainline")),
            ))
            .chain(
                list.iter()
                    .filter_map(|branch| branch.name.as_deref())
                    .map(|name| (SharedString::from(name.to_owned()), SharedString::from(name.to_owned()))),
            );
            Select::new("install-branch", options, dialog.chosen.clone().unwrap_or_default())
                .on_change(move |value, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        let Some(dialog) = &mut this.local.dialog else {
                            return;
                        };
                        dialog.chosen = (!value.is_empty()).then(|| value.to_string());
                        let ids = dialog
                            .request
                            .as_ref()
                            .map(|request| (request.account_id.clone(), request.product_id.clone()));
                        let branch = dialog.chosen.clone();
                        if let Some((account_id, product_id)) = ids {
                            let fetch = fetch_languages(account_id, product_id, branch, cx);
                            if let Some(dialog) = &mut this.local.dialog {
                                dialog._languages_fetch = fetch;
                            }
                        }
                        cx.notify();
                    });
                })
                .into_any_element()
        }
    }
}
