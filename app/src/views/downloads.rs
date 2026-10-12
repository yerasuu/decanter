//! Every install in flight, in one place — the same `installing` map the
//! per-game action bar reads, listed instead of scattered across game
//! pages so a download can be watched or cancelled without hunting for
//! the game that started it.

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::format;
use crate::state::{AppState, InstallStatus};
use crate::theme;
use crate::views::library::install;
use crate::widgets::{ButtonKind, button, progress_bar};

pub struct DownloadsView;

impl DownloadsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.library, |_, _, cx| cx.notify()).detach();
        Self
    }
}

/// The bar and the "x / y · rate · faltan z" line, shared with the detail
/// pane's own install progress so the same download reads identically in
/// both places.
pub fn progress_lines(key: &str, status: &InstallStatus) -> Vec<gpui::AnyElement> {
    let label = |text: String| {
        div()
            .text_size(px(11.))
            .text_color(rgb(theme::TEXT_MUTED))
            .truncate()
            .child(text)
            .into_any_element()
    };
    match status {
        InstallStatus::Queued => vec![
            progress_bar(SharedString::from(format!("bar-{key}")), None, 4.).into_any_element(),
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
                progress_bar(SharedString::from(format!("bar-{key}")), status.fraction(), 4.).into_any_element(),
                label(text),
            ]
        }
        InstallStatus::Failed(err) => vec![theme::error(err.clone()).into_any_element()],
    }
}

impl Render for DownloadsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::get(cx).clone();
        let games = &state.library.read(cx).games;
        // Sorted by title so the list doesn't reshuffle on every progress
        // tick — a `HashMap`'s iteration order is arbitrary. Each row's own
        // account comes from its entry in the merged library: two of these
        // downloads can belong to two different accounts in the same profile.
        let mut entries: Vec<(String, String, String, InstallStatus)> = state
            .background
            .read(cx)
            .installing
            .iter()
            .map(|(product_id, status)| {
                let entry = games.iter().find(|entry| &entry.game.id == product_id);
                let title = entry.map_or_else(|| product_id.clone(), |entry| entry.game.title.clone());
                let account_id = entry.map_or_else(String::new, |entry| entry.game.account_id.clone());
                (product_id.clone(), title, account_id, status.clone())
            })
            .collect();
        entries.sort_by(|left, right| left.1.cmp(&right.1));

        let column = div().flex().flex_col().child(theme::h1(t!("downloads.title")));
        if entries.is_empty() {
            return theme::page("downloads", column.child(theme::empty(t!("downloads.empty"))));
        }
        let count = entries.len();
        let rows = entries
            .into_iter()
            .enumerate()
            .map(|(index, (product_id, title, account_id, status))| {
                let mut main = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).mb_2().child(title))
                    .children(progress_lines(&product_id, &status));
                if let InstallStatus::Installing { current_file, .. } = &status {
                    main = main.child(
                        div()
                            .mt_1()
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .truncate()
                            .child(current_file.clone()),
                    );
                }
                let action = match status {
                    // A failure is already over; the row is just a notice the
                    // user hasn't read yet, so it's dismissed, not cancelled.
                    InstallStatus::Failed(_) => {
                        button(("dismiss", index), ButtonKind::Icon, t!("downloads.dismiss"), false).on_click(
                            cx.listener(move |_, _, _, cx| {
                                tracing::debug!(product_id, "dismissing failed install notice");
                                AppState::get(cx).background.clone().update(cx, |background, cx| {
                                    background.installing.remove(&product_id);
                                    cx.notify();
                                });
                            }),
                        )
                    }
                    InstallStatus::Queued | InstallStatus::Installing { .. } => {
                        button(("cancel", index), ButtonKind::Icon, t!("common.cancel"), false).on_click(cx.listener(
                            move |_, _, _, cx| {
                                let state = AppState::get(cx).clone();
                                let account_id = account_id.clone();
                                let product_id = product_id.clone();
                                cx.spawn(async move |_, cx| {
                                    install::cancel_and_forget(account_id, product_id, state, cx).await;
                                })
                                .detach();
                            },
                        ))
                    }
                };
                theme::list_row(("download-row", index), index + 1 == count)
                    .items_start()
                    .child(main)
                    .child(div().flex_none().child(action))
            });
        theme::page("downloads", column.child(theme::card().children(rows)))
    }
}
