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
    /// Every library folder the install could go in, with its free space.
    folders: Vec<(PathBuf, Option<u64>)>,
    /// The folder picked; `None` is the main one.
    folder: Option<PathBuf>,
    /// Library folders whose disk isn't plugged in — the game may already
    /// be installed on one of them.
    offline: Vec<PathBuf>,
    branches: Option<Result<Vec<Branch>, String>>,
    chosen: Option<String>,
    /// Dropping the dialog drops this, so a branch list still in flight
    /// for a closed dialog is abandoned rather than written back.
    _fetch: Task<()>,
}

impl DetailView {
    pub(super) fn open_install_dialog(&mut self, account_id: String, product_id: String, cx: &mut Context<Self>) {
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
                if let Some(dialog) = &mut this.local.dialog {
                    dialog.branches = Some(result);
                    cx.notify();
                }
            });
        });
        let offline = AppState::get(cx).library.read(cx).offline_folders.clone();
        self.local.dialog = Some(InstallDialog {
            folders,
            folder: None,
            offline,
            branches: None,
            chosen: None,
            _fetch: fetch,
        });
        cx.notify();
    }
}

/// Confirms an install before it starts, the way Steam does — where the
/// files go, and which branch to install (mainline by default, same as
/// Steam defaults to no beta opt-in). The library location shown here is
/// whatever this account's profile already resolved to (its own chosen
/// location, or the shared default) — informational, since changing it is
/// a per-profile setting, not something to pick mid-install.
pub(super) fn install_dialog(
    dialog: &InstallDialog,
    request: InstallRequest,
    window: &Window,
    cx: &Context<DetailView>,
) -> AnyElement {
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
    let dlc_count = request.dlcs.len();
    let title = request.title.clone();

    let rows = install_rows(dialog, dlc_count);
    let folders = folder_picker(dialog, this.clone());
    let branch = branch_picker(dialog, this);
    let field_row = || div().flex().gap(px(10.)).mb_3().max_w(px(520.));

    let chosen = dialog.chosen.clone();
    let folder = dialog.folder.clone();
    // A click handler is `Fn`, but the request is spent once: the dialog
    // closes on the same click, so a second one never reaches it.
    let request = std::cell::Cell::new(Some(request));
    let confirm = cx.listener(move |this, _, _, cx| {
        this.local.dialog = None;
        cx.notify();
        if let Some(mut request) = request.take() {
            request.branch.clone_from(&chosen);
            request.library_root.clone_from(&folder);
            actions::start_install(request, cx);
        }
    });
    let cancel = Rc::clone(&close);
    let dismiss = Rc::clone(&close);
    let (folder_label, branch_label) = (t!("library.detail.install.folder"), t!("library.detail.install.branch"));
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
            .child(
                field_row()
                    .child(button("install-confirm", ButtonKind::Play, install_label, false).on_click(confirm))
                    .child(
                        button("install-dialog-cancel", ButtonKind::Secondary, cancel_label, false)
                            .on_click(move |_, window, cx| cancel(window, cx)),
                    ),
            ),
    );
    menu::modal(panel, window, dismiss)
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
                        if let Some(dialog) = &mut this.local.dialog {
                            dialog.chosen = (!value.is_empty()).then(|| value.to_string());
                            cx.notify();
                        }
                    });
                })
                .into_any_element()
        }
    }
}
