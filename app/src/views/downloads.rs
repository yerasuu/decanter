//! Every GOG download in flight, in one place — the same `installing` map
//! the per-game action bar reads, listed instead of scattered across game
//! pages and properties tabs so a download can be watched, paused, resumed
//! or cancelled without hunting for whatever started it.

use std::time::{Duration, SystemTime};

use gpui::{App, BoxShadow, Context, FontWeight, SharedString, Window, div, point, prelude::*, px, rgb};
use rust_i18n::t;

use crate::format;
use crate::state::{AppState, Download, DownloadKey, DownloadKind, InstallStatus};
use crate::theme;
use crate::views::library::actions;
use crate::widgets::{ButtonKind, button, progress_bar, tooltip};

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
            label(queued_label(key)),
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

/// Where a queued download stands: being prepared once it has the turn, else its place in the queue.
fn queued_label(key: &DownloadKey) -> String {
    let queue = decanter_gog::download_queue();
    if queue.is_running(&key.account_id, &key.product_id) {
        return t!("downloads.preparing").into_owned();
    }
    queue
        .position(&key.account_id, &key.product_id)
        .map_or_else(
            || t!("common.queued"),
            |position| t!("downloads.queue_position", position = position + 1),
        )
        .into_owned()
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

/// The page's sections, in the order they are shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Active,
    Queued,
    Paused,
    Elsewhere,
    Failed,
}

impl Group {
    const ALL: [Self; 5] = [Self::Active, Self::Queued, Self::Paused, Self::Elsewhere, Self::Failed];

    fn of(key: &DownloadKey, download: &Download, queue: &decanter_gog::DownloadQueue) -> Self {
        let waits = match download.status {
            InstallStatus::Queued => !queue.is_running(&key.account_id, &key.product_id),
            // A cancel stopping a download that waited stays where it waited.
            InstallStatus::Cancelling { .. } => queue.position(&key.account_id, &key.product_id).is_some(),
            InstallStatus::Installing { .. }
            | InstallStatus::Paused { .. }
            | InstallStatus::Elsewhere { .. }
            | InstallStatus::Failed(_) => false,
        };
        if waits {
            return Self::Queued;
        }
        match download.status {
            InstallStatus::Paused { .. } => Self::Paused,
            InstallStatus::Elsewhere { .. } => Self::Elsewhere,
            InstallStatus::Failed(_) => Self::Failed,
            InstallStatus::Installing { .. } | InstallStatus::Queued | InstallStatus::Cancelling { .. } => Self::Active,
        }
    }

    fn title(self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Active => t!("downloads.group.active"),
            Self::Queued => t!("downloads.group.queued"),
            Self::Paused => t!("downloads.group.paused"),
            Self::Elsewhere => t!("downloads.group.elsewhere"),
            Self::Failed => t!("downloads.group.failed"),
        }
    }
}

/// What a queued row carries while it is dragged to another place in the queue.
struct QueueDrag {
    key: DownloadKey,
    title: SharedString,
}

/// The row's ghost under the cursor while it is dragged.
struct QueueGhost {
    title: SharedString,
}

impl Render for QueueGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(18.))
            .py(px(8.))
            .opacity(0.7)
            .bg(rgb(theme::BG_PANEL))
            .rounded(px(theme::RADIUS))
            .font_weight(FontWeight::SEMIBOLD)
            .child(self.title.clone())
    }
}

/// Subir and Bajar beside the row's buttons, for moving it without dragging.
fn step_buttons(index: usize, key: &DownloadKey, position: usize, count: usize) -> Vec<gpui::AnyElement> {
    let step = |id: &'static str, glyph: &'static str, hint, to, disabled: bool| {
        let key = key.clone();
        button((id, index), ButtonKind::Secondary, glyph, disabled)
            .tooltip(tooltip(hint))
            .when(!disabled, |button| {
                button.on_click(move |_, _, cx| actions::move_in_queue(key.clone(), to, cx))
            })
            .into_any_element()
    };
    vec![
        step(
            "queue-up",
            "↑",
            t!("downloads.move_up"),
            decanter_gog::QueueMove::Up,
            position == 0,
        ),
        step(
            "queue-down",
            "↓",
            t!("downloads.move_down"),
            decanter_gog::QueueMove::Down,
            position + 1 == count,
        ),
    ]
}

/// Subir al principio and Bajar al final, under the row's progress.
fn end_buttons(index: usize, key: &DownloadKey, position: usize, count: usize) -> gpui::Div {
    let to_end = |id: &'static str, text, to, disabled: bool| {
        let key = key.clone();
        button((id, index), ButtonKind::Icon, text, disabled)
            .text_size(px(11.))
            .when(!disabled, |button| {
                button.on_click(move |_, _, cx| actions::move_in_queue(key.clone(), to, cx))
            })
    };
    div()
        .flex()
        .gap_4()
        .mt_1()
        .child(to_end(
            "queue-first",
            t!("downloads.move_first"),
            decanter_gog::QueueMove::First,
            position == 0,
        ))
        .child(to_end(
            "queue-last",
            t!("downloads.move_last"),
            decanter_gog::QueueMove::Last,
            position + 1 == count,
        ))
}

/// A row's buttons: Descartar on a failure, else what `download_actions` offers.
fn row_buttons(index: usize, key: &DownloadKey, download: &Download, confirming: bool) -> Vec<gpui::AnyElement> {
    let InstallStatus::Failed(_) = download.status else {
        return download_actions(index, key, download, confirming);
    };
    // A failure is already over; the row is just a notice the user hasn't
    // read yet, so it's dismissed, not cancelled.
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

/// One row; `queued` is its place among `count` waiting downloads, which makes it draggable and
/// movable.
fn row(
    index: usize,
    is_last: bool,
    key: &DownloadKey,
    download: &Download,
    confirming: bool,
    queued: Option<(usize, usize)>,
) -> gpui::Stateful<gpui::Div> {
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
    let movable = queued.filter(|(_, count)| *count > 1 && !confirming);
    if let Some((position, count)) = movable {
        main = main.child(end_buttons(index, key, position, count));
    }
    let buttons = movable
        .map(|(position, count)| step_buttons(index, key, position, count))
        .unwrap_or_default()
        .into_iter()
        .chain(row_buttons(index, key, download, confirming));
    let row = theme::list_row(("download-row", index), is_last)
        .items_start()
        .when(movable.is_some(), |row| {
            row.child(
                div()
                    .id(("queue-grip", index))
                    .flex_none()
                    .tooltip(tooltip(t!("downloads.drag_to_reorder")))
                    .text_size(px(13.))
                    .text_color(rgb(theme::TEXT_DIM))
                    .child("⠿"),
            )
        })
        .child(main)
        .child(div().flex_none().flex().items_start().gap_2().children(buttons));
    let Some((position, _)) = movable else {
        return row;
    };
    let drag = QueueDrag {
        key: key.clone(),
        title: SharedString::from(download.title.clone()),
    };
    let dragged = key.clone();
    // Steam moves a queued download by dragging its row; the whole row is the handle.
    row.cursor_grab()
        .on_drag(drag, |drag, _, _, cx| {
            let title = drag.title.clone();
            cx.new(|_| QueueGhost { title })
        })
        .drag_over::<QueueDrag>(move |style, drag, _, _| {
            if drag.key == dragged {
                return style;
            }
            style.shadow(vec![BoxShadow {
                color: rgb(theme::ACCENT_BLUE).into(),
                offset: point(px(0.), px(-2.)),
                blur_radius: px(0.),
                spread_radius: px(0.),
                inset: false,
            }])
        })
        .on_drop(move |drag: &QueueDrag, _, cx| {
            actions::move_in_queue(drag.key.clone(), decanter_gog::QueueMove::To(position), cx);
        })
}

impl Render for DownloadsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::get(cx).clone();
        let background = state.background.read(cx);
        let queue = decanter_gog::download_queue();
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
        let mut column = div()
            .flex()
            .flex_col()
            .child(header)
            .when(!background.verify_offers.is_empty(), |column| {
                column.child(theme::card().mb_4().children(offers))
            });
        if entries.is_empty() {
            return theme::page("downloads", column.child(theme::empty(t!("downloads.empty"))));
        }
        let mut index = 0;
        for group in Group::ALL {
            let mut members: Vec<(&DownloadKey, &Download)> = entries
                .iter()
                .copied()
                .filter(|(key, download)| Group::of(key, download, &queue) == group)
                .collect();
            if members.is_empty() {
                continue;
            }
            if group == Group::Queued {
                // In the order they will start; one still joining the queue goes last.
                members.sort_by_key(|(key, _)| queue.position(&key.account_id, &key.product_id).unwrap_or(usize::MAX));
            }
            let waiting = queue.waiting.len();
            let count = members.len();
            let mut card = theme::card();
            for (offset, (key, download)) in members.into_iter().enumerate() {
                let queued = (group == Group::Queued)
                    .then(|| queue.position(&key.account_id, &key.product_id))
                    .flatten()
                    .map(|position| (position, waiting));
                let confirming = background.cancel_confirm.as_ref() == Some(key);
                card = card.child(row(index, offset + 1 == count, key, download, confirming, queued));
                index += 1;
            }
            column = column.child(theme::h2(group.title())).child(card);
        }
        theme::page("downloads", column)
    }
}
