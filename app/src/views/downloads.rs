//! Every GOG download in flight, in one place — the same `installing` map
//! the per-game action bar reads, listed instead of scattered across game
//! pages and properties tabs so a download can be watched or cancelled
//! without hunting for whatever started it.

use gpui::{App, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::format;
use crate::state::{AppState, Download, DownloadKey, DownloadKind, InstallStatus};
use crate::theme;
use crate::views::library::actions;
use crate::widgets::{ButtonKind, button, progress_bar};

pub struct DownloadsView;

impl DownloadsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        Self
    }
}

/// Asks the shell for this page, from a view that doesn't own it.
pub fn open(cx: &mut App) {
    let session = AppState::get(cx).session.clone();
    session.update(cx, |session, cx| {
        session.downloads_request = session.downloads_request.wrapping_add(1);
        cx.notify();
    });
}

/// The bar and the "x / y · rate · faltan z" line, shared with the detail
/// pane's own install progress so the same download reads identically in
/// both places.
pub fn progress_lines(key: &DownloadKey, status: &InstallStatus) -> Vec<gpui::AnyElement> {
    let label = |text: String| {
        div()
            .text_size(px(11.))
            .text_color(rgb(theme::TEXT_MUTED))
            .truncate()
            .child(text)
            .into_any_element()
    };
    let bar_id = SharedString::from(format!("bar-{}", key.id()));
    match status {
        InstallStatus::Queued => vec![
            progress_bar(bar_id, None, 4.).into_any_element(),
            label(t!("common.queued").into_owned()),
        ],
        InstallStatus::Installing {
            bytes_downloaded,
            total_bytes,
            bytes_per_second,
            ..
        } => {
            let remaining = total_bytes.unwrap_or(0).saturating_sub(*bytes_downloaded);
            let mut text = format::bytes(*bytes_downloaded);
            if let Some(total) = total_bytes {
                text.push_str(&format!(" / {}", format::bytes(*total)));
            }
            text.push_str(&format!(" · {}", format::rate(*bytes_per_second)));
            if let Some(left) = format::eta(remaining, *bytes_per_second) {
                text.push_str(&format!(" · {}", t!("downloads.eta", left = left)));
            }
            vec![
                progress_bar(bar_id, status.fraction(), 4.).into_any_element(),
                label(text),
            ]
        }
        InstallStatus::Cancelling { .. } => vec![
            progress_bar(bar_id, None, 4.).into_any_element(),
            label(t!("downloads.cancelling").into_owned()),
        ],
        InstallStatus::Failed(err) => vec![theme::error(err.clone()).into_any_element()],
    }
}

/// Cancelar, or the question it opens while that is pending: the same
/// buttons in Descargas and on the game's own page.
pub fn cancel_action(index: usize, key: &DownloadKey, download: &Download, confirming: bool) -> gpui::AnyElement {
    if !confirming {
        let key = key.clone();
        return button(("cancel", index), ButtonKind::Icon, t!("common.cancel"), false)
            .on_click(move |_, _, cx| actions::ask_cancel(key.clone(), cx))
            .into_any_element();
    }
    let downloaded = download.status.bytes_downloaded();
    let question = if downloaded == 0 {
        t!("downloads.cancel_confirm_empty", title = download.title)
    } else {
        t!(
            "downloads.cancel_confirm",
            title = download.title,
            size = format::bytes(downloaded)
        )
    };
    let confirm_key = key.clone();
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap(px(6.))
        .max_w(px(320.))
        .child(div().text_size(px(12.)).child(question))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    button(
                        ("cancel-keep", index),
                        ButtonKind::Secondary,
                        t!("downloads.keep"),
                        false,
                    )
                    .on_click(|_, _, cx| actions::keep_download(cx)),
                )
                .child(
                    button(
                        ("cancel-confirm", index),
                        ButtonKind::Icon,
                        t!("downloads.cancel_delete"),
                        false,
                    )
                    .on_click(move |_, _, cx| actions::confirm_cancel(confirm_key.clone(), cx)),
                ),
        )
        .into_any_element()
}

fn kind_label(kind: DownloadKind) -> Option<std::borrow::Cow<'static, str>> {
    match kind {
        DownloadKind::Install => None,
        DownloadKind::Dlc => Some(t!("downloads.kind.dlc")),
        DownloadKind::Update => Some(t!("downloads.kind.update")),
        DownloadKind::BranchSwitch => Some(t!("downloads.kind.branch_switch")),
        DownloadKind::Rollback => Some(t!("downloads.kind.rollback")),
        DownloadKind::Repair => Some(t!("downloads.kind.repair")),
    }
}

impl Render for DownloadsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::get(cx).clone();
        let background = state.background.read(cx);
        // Sorted so the list doesn't reshuffle on every progress tick — a
        // `HashMap`'s iteration order is arbitrary. The account breaks ties:
        // the same game can download for two accounts of one profile.
        let mut entries: Vec<(&DownloadKey, &Download)> = background.installing.iter().collect();
        entries.sort_by(|left, right| (&left.1.title, left.0).cmp(&(&right.1.title, right.0)));

        let column = div().flex().flex_col().child(theme::h1(t!("downloads.title")));
        if entries.is_empty() {
            return theme::page("downloads", column.child(theme::empty(t!("downloads.empty"))));
        }
        let count = entries.len();
        let rows = entries.into_iter().enumerate().map(|(index, (key, download))| {
            let mut main = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .mb_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(download.title.clone()))
                        .children(
                            kind_label(download.kind)
                                .map(|kind| div().text_size(px(11.)).text_color(rgb(theme::TEXT_MUTED)).child(kind)),
                        ),
                )
                .children(progress_lines(key, &download.status));
            if let InstallStatus::Installing { current_file, .. } = &download.status {
                main = main.child(
                    div()
                        .mt_1()
                        .text_size(px(11.))
                        .text_color(rgb(theme::TEXT_MUTED))
                        .truncate()
                        .child(current_file.clone()),
                );
            }
            let action = match &download.status {
                // A failure is already over; the row is just a notice the
                // user hasn't read yet, so it's dismissed, not cancelled.
                InstallStatus::Failed(_) => {
                    let key = key.clone();
                    Some(
                        button(("dismiss", index), ButtonKind::Icon, t!("downloads.dismiss"), false)
                            .on_click(move |_, _, cx| {
                                tracing::debug!(
                                    account_id = key.account_id,
                                    product_id = key.product_id,
                                    "dismissing failed download notice"
                                );
                                actions::dismiss_install(&key, cx);
                            })
                            .into_any_element(),
                    )
                }
                InstallStatus::Cancelling { .. } => None,
                InstallStatus::Queued | InstallStatus::Installing { .. } => Some(cancel_action(
                    index,
                    key,
                    download,
                    background.cancel_confirm.as_ref() == Some(key),
                )),
            };
            theme::list_row(("download-row", index), index + 1 == count)
                .items_start()
                .child(main)
                .child(div().flex_none().children(action))
        });
        theme::page("downloads", column.child(theme::card().children(rows)))
    }
}
