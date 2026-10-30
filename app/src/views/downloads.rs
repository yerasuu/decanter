//! Every GOG download in flight, in one place — the same `installing` map
//! the per-game action bar reads, listed instead of scattered across game
//! pages and properties tabs so a download can be watched, paused, resumed
//! or cancelled without hunting for whatever started it.

use std::time::{Duration, SystemTime};

use gpui::{App, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::format;
use crate::state::{AppState, Download, DownloadKey, DownloadKind, InstallStatus};
use crate::theme;
use crate::views::library::actions;
use crate::widgets::{ButtonKind, button, progress_bar};

/// A download paused this long gets a notice offering to resume or delete
/// it (owner's decision, #1281); nothing deletes it on its own.
const STALE_AFTER: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Whole days `status` has been paused, once that is [`STALE_AFTER`] or more.
pub fn stale_days(status: &InstallStatus) -> Option<u64> {
    let InstallStatus::Paused { since: Some(since), .. } = status else {
        return None;
    };
    let paused = SystemTime::now().duration_since(*since).ok()?;
    (paused >= STALE_AFTER).then_some(paused.as_secs() / (24 * 60 * 60))
}

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
pub fn progress_lines(key: &DownloadKey, download: &Download) -> Vec<gpui::AnyElement> {
    let status = &download.status;
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
            let mut lines = vec![
                progress_bar(bar_id, status.fraction(), 4.).into_any_element(),
                label(text),
            ];
            lines.extend(reused_line(download, &label));
            lines
        }
        InstallStatus::Cancelling { .. } => vec![
            progress_bar(bar_id, None, 4.).into_any_element(),
            label(t!("downloads.cancelling").into_owned()),
        ],
        InstallStatus::Paused {
            reason,
            room,
            bytes_done,
            total_bytes,
            ..
        } => {
            let done = format::bytes(*bytes_done);
            let progress = total_bytes.map_or_else(
                || t!("downloads.paused_unsized", done = done.as_str()),
                |total| t!("downloads.paused", done = done.as_str(), total = format::bytes(total)),
            );
            let mut lines = vec![
                progress_bar(bar_id, status.fraction(), 4.).into_any_element(),
                label(format!("{progress} · {}", pause_reason(*reason, *room, download))),
            ];
            if let Some(days) = stale_days(status) {
                lines.push(label(
                    t!("downloads.stale", days = days, size = done.as_str()).into_owned(),
                ));
            }
            lines.extend(reused_line(download, &label));
            lines
        }
        InstallStatus::Elsewhere {
            bytes_done,
            total_bytes,
        } => vec![
            progress_bar(bar_id, status.fraction(), 4.).into_any_element(),
            label(
                t!(
                    "downloads.elsewhere",
                    done = format::bytes(*bytes_done),
                    total = format::bytes(*total_bytes)
                )
                .into_owned(),
            ),
        ],
        InstallStatus::Failed(err) => vec![theme::error(err.clone()).into_any_element()],
    }
}

/// "Se reutilizaron X de Y" once GOG's new build kept part of what was downloaded.
fn reused_line(download: &Download, label: &impl Fn(String) -> gpui::AnyElement) -> Option<gpui::AnyElement> {
    download.reused.map(|switch| {
        label(
            t!(
                "downloads.reused",
                kept = format::bytes(switch.kept_bytes),
                had = format::bytes(switch.had_bytes)
            )
            .into_owned(),
        )
    })
}

/// Why `download` is paused, naming its library folder and, for a full
/// disk, how much has to be freed there.
fn pause_reason(
    reason: decanter_gog::PauseReason,
    room: Option<decanter_gog::Room>,
    download: &Download,
) -> std::borrow::Cow<'static, str> {
    let folder = download.root.as_ref().map(|root| root.display().to_string());
    match (reason, folder) {
        (decanter_gog::PauseReason::User, _) => t!("downloads.paused_reason.user"),
        (decanter_gog::PauseReason::Network, _) => t!("downloads.paused_reason.network"),
        (decanter_gog::PauseReason::DiskFull, Some(path)) => match room.filter(|room| room.short() > 0) {
            // Staging holds a whole second copy of the game until the move.
            Some(room) if download.kind == DownloadKind::Update => t!(
                "downloads.paused_reason.disk_full_update",
                needed = format::bytes(room.needed),
                short = format::bytes(room.short()),
                path = path
            ),
            Some(room) => t!(
                "downloads.paused_reason.disk_full_short",
                short = format::bytes(room.short()),
                path = path
            ),
            None => t!("downloads.paused_reason.disk_full_at", path = path),
        },
        (decanter_gog::PauseReason::DiskFull, None) => t!("downloads.paused_reason.disk_full"),
        (decanter_gog::PauseReason::LibraryUnavailable, Some(path)) => {
            t!("downloads.paused_reason.library_unavailable_at", path = path)
        }
        (decanter_gog::PauseReason::LibraryUnavailable, None) => t!("downloads.paused_reason.library_unavailable"),
        (decanter_gog::PauseReason::AppClosed, _) => t!("downloads.paused_reason.app_closed"),
        (decanter_gog::PauseReason::ClosedUnexpectedly, _) => t!("downloads.paused_reason.closed_unexpectedly"),
    }
}

/// Cancelar, or the question it opens while that is pending: the same
/// buttons in Descargas and on the game's own page.
pub fn cancel_action(index: usize, key: &DownloadKey, download: &Download, confirming: bool) -> gpui::AnyElement {
    if !confirming {
        let key = key.clone();
        // Long paused, what is left to do with it is keep it or delete it.
        let label = if stale_days(&download.status).is_some() {
            t!("downloads.delete")
        } else {
            t!("common.cancel")
        };
        return button(("cancel", index), ButtonKind::Icon, label, false)
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

/// What a download's row and its game's page offer: Pausar on one queued
/// or running, Reanudar on one paused, and Cancelar on either; while a
/// cancel waits for its answer, only that question. Nothing on one already
/// stopping, failed, or run by another process.
pub fn download_actions(
    index: usize,
    key: &DownloadKey,
    download: &Download,
    confirming: bool,
) -> Vec<gpui::AnyElement> {
    if matches!(
        download.status,
        InstallStatus::Cancelling { .. } | InstallStatus::Elsewhere { .. } | InstallStatus::Failed(_)
    ) {
        return Vec::new();
    }
    let cancel = cancel_action(index, key, download, confirming);
    if confirming {
        return vec![cancel];
    }
    let toggle = match download.status {
        InstallStatus::Queued | InstallStatus::Installing { .. } => {
            let key = key.clone();
            Some(
                button(("pause", index), ButtonKind::Secondary, t!("downloads.pause"), false)
                    .on_click(move |_, _, cx| actions::pause_download(key.clone(), cx))
                    .into_any_element(),
            )
        }
        InstallStatus::Paused { .. } => {
            let key = key.clone();
            Some(
                button(("resume", index), ButtonKind::Secondary, t!("downloads.resume"), false)
                    .on_click(move |_, _, cx| actions::resume_download(key.clone(), cx))
                    .into_any_element(),
            )
        }
        InstallStatus::Cancelling { .. } | InstallStatus::Elsewhere { .. } | InstallStatus::Failed(_) => None,
    };
    toggle.into_iter().chain(std::iter::once(cancel)).collect()
}

/// Pausar todo and Reanudar todo, once there are rows enough to want them.
fn bulk_actions(entries: &[(&DownloadKey, &Download)]) -> Option<gpui::Div> {
    if entries.len() < 2 {
        return None;
    }
    let any_live = entries
        .iter()
        .any(|(_, row)| matches!(row.status, InstallStatus::Queued | InstallStatus::Installing { .. }));
    let any_paused = entries.iter().any(|(_, row)| actions::is_resumable(row));
    Some(
        div()
            .flex()
            .gap_2()
            .when(any_live, |bar| {
                bar.child(
                    button("pause-all", ButtonKind::Secondary, t!("downloads.pause_all"), false)
                        .on_click(|_, _, cx| actions::pause_all(cx)),
                )
            })
            .when(any_paused, |bar| {
                bar.child(
                    button("resume-all", ButtonKind::Secondary, t!("downloads.resume_all"), false)
                        .on_click(|_, _, cx| actions::resume_all(cx)),
                )
            }),
    )
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

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(theme::h1(t!("downloads.title")))
            .children(bulk_actions(&entries));
        let offers = background.verify_offers.iter().enumerate().map(|(index, offer)| {
            let verify_key = offer.key.clone();
            let dismiss_key = offer.key.clone();
            div()
                .flex()
                .items_center()
                .gap_4()
                .px(px(18.))
                .py(px(10.))
                .border_b_1()
                .border_color(rgb(theme::LINE))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(t!("downloads.verify_offer", title = offer.title.as_str())),
                )
                .child(
                    button(
                        ("verify-now", index),
                        ButtonKind::Secondary,
                        t!("downloads.verify_now"),
                        false,
                    )
                    .on_click(move |_, _, cx| actions::verify_now(&verify_key, cx)),
                )
                .child(
                    button(
                        ("verify-dismiss", index),
                        ButtonKind::Icon,
                        t!("downloads.dismiss"),
                        false,
                    )
                    .on_click(move |_, _, cx| actions::dismiss_verify_offer(&dismiss_key, cx)),
                )
        });
        let column = div()
            .flex()
            .flex_col()
            .child(header)
            .when(!background.verify_offers.is_empty(), |column| {
                column.child(theme::card().mb_4().children(offers))
            });
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
                .children(progress_lines(key, download));
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
            let buttons = match &download.status {
                // A failure is already over; the row is just a notice the
                // user hasn't read yet, so it's dismissed, not cancelled.
                InstallStatus::Failed(_) => {
                    let key = key.clone();
                    vec![
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
                    ]
                }
                _ => download_actions(index, key, download, background.cancel_confirm.as_ref() == Some(key)),
            };
            theme::list_row(("download-row", index), index + 1 == count)
                .items_start()
                .child(main)
                .child(div().flex_none().flex().items_start().gap_2().children(buttons))
        });
        theme::page("downloads", column.child(theme::card().children(rows)))
    }
}
