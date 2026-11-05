use decanter_core::capabilities::Feature;
use decanter_core::{Game, InstalledGame, Storefront};
use gpui::{AnyElement, Context, Div, FocusHandle, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::install_dialog::install_dialog;
use super::{BarState, DetailView};
use crate::capabilities::Caps;
use crate::state::{AppState, Download, InstallStatus, MoveKey, MoveStatus, move_fraction, moving_text};
use crate::theme;
use crate::views::downloads::{download_actions, progress_lines};
use crate::views::library::install::{self, InstallRequest};
use crate::views::library::{actions, open_properties};
use crate::widgets::{ButtonKind, button, progress_bar, tooltip};

impl DetailView {
    pub(super) fn action_bar(&self, game: &Game, bar_state: BarState, window: &Window, cx: &Context<Self>) -> Div {
        let bar = div().pt(px(24.)).px(px(32.)).flex().items_center().gap(px(10.));
        if game.storefront == Storefront::Steam {
            return bar.children(self.steam_actions(game, bar_state, cx));
        }
        let is_installed = game.install_state.as_ref().is_some_and(|state| state.installed);
        if !is_installed {
            return bar.children(self.install_action(game, bar_state, window, cx));
        }
        let properties_game = game.clone();
        let relocation = relocation(game, cx);
        let is_moving = relocation.as_ref().is_some_and(|(live, _)| *live);
        bar.child(play_action(
            game,
            bar_state.launch,
            bar_state.running,
            is_moving,
            &self.primary,
            cx,
        ))
        .children(
            (!is_moving)
                .then(|| self.uninstall_action(game, &bar_state.dlcs, cx))
                .into_iter()
                .flatten(),
        )
        .child(
            button("properties", ButtonKind::Icon, t!("library.properties.title"), false)
                .tooltip(tooltip(t!("library.properties.title")))
                .on_click(move |_, _, cx| open_properties(&properties_game, cx)),
        )
        .children(relocation.map(|(_, lines)| lines))
    }

    /// Always safe to call — uninstalling something that isn't installed is
    /// a no-op — but it deletes real files, so it still gets a confirm step.
    fn uninstall_action(&self, game: &Game, dlcs: &[(String, String)], cx: &Context<Self>) -> Vec<AnyElement> {
        let uninstall = &self.local.uninstall;
        if uninstall.busy {
            return vec![theme::muted(t!("library.detail.uninstall.busy")).into_any_element()];
        }
        // An external game's own files aren't decanter's to delete; only
        // its entry and prefix go.
        let (label, confirm) = if matches!(game.storefront, Storefront::Unspecified | Storefront::Steam) {
            (
                t!("library.detail.uninstall.remove"),
                t!("library.detail.uninstall.confirm_remove"),
            )
        } else {
            (
                t!("library.detail.uninstall.uninstall"),
                t!("library.detail.uninstall.confirm_uninstall"),
            )
        };
        if !uninstall.confirming {
            return vec![
                button("uninstall", ButtonKind::Icon, label.clone(), false)
                    .tooltip(tooltip(label))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.local.uninstall.confirming = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            ];
        }
        let account_id = game.account_id.clone();
        let product_id = game.id.clone();
        // Uninstalling deletes the paused downloads of the game and of its DLC
        // too (#1133, #1316), so the question names each before it is answered.
        let background = AppState::get(cx).background.read(cx);
        let mut elements: Vec<AnyElement> = std::iter::once((product_id.as_str(), game.title.as_str()))
            .chain(dlcs.iter().map(|(id, title)| (id.as_str(), title.as_str())))
            .filter_map(|(id, title)| match background.status(&account_id, id) {
                Some(status @ InstallStatus::Paused { .. }) => Some(
                    theme::muted(t!(
                        "library.detail.uninstall.paused_download",
                        title = title,
                        size = crate::format::bytes(status.bytes_downloaded())
                    ))
                    .into_any_element(),
                ),
                _ => None,
            })
            .collect();
        elements.extend([
            button("uninstall-confirm", ButtonKind::Icon, confirm, false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.uninstall(account_id.clone(), product_id.clone(), cx);
                }))
                .into_any_element(),
            button("uninstall-cancel", ButtonKind::Icon, t!("common.cancel"), false)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.local.uninstall.confirming = false;
                    cx.notify();
                }))
                .into_any_element(),
        ]);
        if let Some(err) = &uninstall.error {
            elements.push(theme::error(err).into_any_element());
        }
        elements
    }

    /// A Steam entry: Steam starts it, and installs and uninstalls it, so
    /// decanter offers JUGAR only while Steam has it installed, and only
    /// ever removes the entry.
    fn steam_actions(&self, game: &Game, bar_state: BarState, cx: &Context<Self>) -> Vec<AnyElement> {
        let is_installed = game.install_state.as_ref().is_some_and(|state| state.installed);
        let mut elements = Vec::with_capacity(5);
        if is_installed {
            elements.push(play_action(
                game,
                bar_state.launch,
                bar_state.running,
                false,
                &self.primary,
                cx,
            ));
            if bar_state.starting {
                elements.push(theme::muted(t!("library.steam.starting")).into_any_element());
            }
        } else {
            elements.push(theme::muted(t!("library.steam.not_installed")).into_any_element());
        }
        if !bar_state.running {
            elements.extend(self.uninstall_action(game, &[], cx));
        }
        let properties_game = game.clone();
        elements.push(
            button("properties", ButtonKind::Icon, t!("library.properties.title"), false)
                .tooltip(tooltip(t!("library.properties.title")))
                .on_click(move |_, _, cx| open_properties(&properties_game, cx))
                .into_any_element(),
        );
        elements
    }

    /// Not installed yet: one button, no questions. The `installing` map is
    /// both the progress display and the guard against a second install for
    /// the same game while one is in flight.
    fn install_action(&self, game: &Game, bar_state: BarState, window: &Window, cx: &Context<Self>) -> Vec<AnyElement> {
        let BarState {
            installable,
            download,
            confirming_cancel,
            dlcs,
            ..
        } = bar_state;
        // GOG carries superseded listings — an old edition someone owns
        // alongside the current one — with nothing behind them to install.
        // Pressing install on one only ever fails, so it isn't offered.
        if !installable {
            return vec![theme::muted(t!("library.detail.install.unavailable")).into_any_element()];
        }
        if let Some(reason) = actions::missing_build(game) {
            return vec![theme::muted(reason).into_any_element()];
        }
        let account_id = game.account_id.clone();
        let product_id = game.id.clone();
        match download {
            Some((
                key,
                Download {
                    status: InstallStatus::Failed(err),
                    ..
                },
            )) => vec![
                theme::error(t!("library.detail.install.failed", error = err)).into_any_element(),
                button(
                    "install-dismiss",
                    ButtonKind::Secondary,
                    t!("library.detail.install.dismiss"),
                    false,
                )
                .on_click(move |_, _, cx| actions::dismiss_install(&key, cx))
                .into_any_element(),
            ],
            Some((key, download)) => {
                let current_file = match &download.status {
                    InstallStatus::Installing { current_file, .. } => Some(current_file.clone()),
                    InstallStatus::Queued
                    | InstallStatus::Cancelling { .. }
                    | InstallStatus::Paused { .. }
                    | InstallStatus::Elsewhere { .. }
                    | InstallStatus::Failed(_) => None,
                };
                let buttons = download_actions(0, &key, &download, confirming_cancel);
                let mut elements = vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .min_w(px(220.))
                        .children(progress_lines(&key, &download))
                        .children(current_file.map(|file| {
                            div()
                                .mt_1()
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_MUTED))
                                .truncate()
                                .child(file)
                        }))
                        .into_any_element(),
                ];
                elements.extend(buttons);
                elements
            }
            // Nothing to choose: the files go to the shared library and the
            // prefix is derived from (profile, game) when the game first runs.
            None => {
                let mut elements = vec![
                    button(
                        "install",
                        ButtonKind::PlaySecondary,
                        t!("library.detail.install.button"),
                        false,
                    )
                    .track_focus(&self.primary)
                    .on_click(cx.listener({
                        let title = game.title.clone();
                        move |this, _, _, cx| {
                            this.open_install_dialog(
                                InstallRequest {
                                    account_id: account_id.clone(),
                                    product_id: product_id.clone(),
                                    parent_id: None,
                                    title: title.clone(),
                                    branch: None,
                                    dlcs: dlcs.clone(),
                                    library_root: None,
                                    language: None,
                                },
                                cx,
                            );
                        }
                    }))
                    .into_any_element(),
                ];
                elements.extend(
                    self.local
                        .dialog
                        .as_ref()
                        .and_then(|dialog| install_dialog(dialog, window, cx)),
                );
                elements
            }
        }
    }

    fn uninstall(&mut self, account_id: String, product_id: String, cx: &mut Context<Self>) {
        self.local.uninstall.busy = true;
        cx.notify();
        let storefront = AppState::get(cx)
            .library
            .read(cx)
            .games
            .iter()
            .find(|entry| entry.game.id == product_id)
            .map(|entry| entry.game.storefront);
        if storefront == Some(Storefront::Steam) {
            let result = crate::external::steam::entries::remove(&account_id, &product_id);
            AppState::get(cx)
                .library
                .clone()
                .update(cx, |library, cx| library.refresh_external(cx));
            let uninstall = &mut self.local.uninstall;
            uninstall.error = result.err();
            uninstall.busy = false;
            uninstall.confirming = false;
            return;
        }
        if storefront == Some(Storefront::Unspecified) {
            let result = crate::external::remove(&account_id, &product_id);
            AppState::get(cx)
                .library
                .clone()
                .update(cx, |library, cx| library.refresh_external(cx));
            let uninstall = &mut self.local.uninstall;
            uninstall.error = result.err().as_ref().map(crate::external::RemoveError::localized);
            uninstall.busy = false;
            uninstall.confirming = false;
            return;
        }
        let state = AppState::get(cx).clone();
        // Detached rather than held by the view: it deletes real files, so
        // selecting another game mid-way mustn't cancel it half done.
        cx.spawn(async move |this, cx| {
            let result = install::uninstall(account_id.clone(), product_id.clone(), state, cx).await;
            let _ = this.update(cx, |this, cx| {
                // The pane may have moved on to another game by now, whose
                // confirm step this result has nothing to do with.
                let still_shown = this
                    .loaded
                    .as_ref()
                    .is_some_and(|loaded| loaded.product_id == product_id && loaded.account_id == account_id);
                if !still_shown {
                    return;
                }
                let uninstall = &mut this.local.uninstall;
                if let Err(err) = result {
                    uninstall.error = Some(err);
                }
                uninstall.busy = false;
                uninstall.confirming = false;
                cx.notify();
            });
        })
        .detach();
    }
}

fn play_action(
    game: &Game,
    launch: Option<InstalledGame>,
    running: bool,
    is_moving: bool,
    primary: &FocusHandle,
    cx: &Context<DetailView>,
) -> AnyElement {
    let Some(installed) = launch else {
        let reason = actions::missing_build(game).unwrap_or_else(|| t!("library.detail.no_executable"));
        return theme::muted(reason).into_any_element();
    };
    let account_id = game.account_id.clone();
    let product_id = game.id.clone();
    let action = if running {
        button("stop", ButtonKind::PlaySecondary, t!("library.detail.stop"), false)
            .track_focus(primary)
            .on_click(move |_, _, cx| {
                tracing::info!(account_id, product_id, "stopping game");
                actions::stop_game(account_id.clone(), product_id.clone(), cx);
            })
    } else {
        button("play", ButtonKind::Play, t!("library.detail.play"), is_moving)
            .track_focus(primary)
            .when(!is_moving, |play| {
                play.on_click(cx.listener(move |this, _, _, cx| {
                    this.show_log(cx);
                    actions::launch_game(account_id.clone(), installed.clone(), cx);
                }))
            })
    };
    Caps::get(cx).gate_or(Feature::Play, action, t!("library.detail.play_unavailable"))
}

/// The game's move in the queue, as the bar and line shown under its actions,
/// and whether it still holds the game's files: a queued or running move does,
/// a failed one is only reported until the queue is cleared.
fn relocation(game: &Game, cx: &Context<DetailView>) -> Option<(bool, AnyElement)> {
    let key = MoveKey::Game {
        account_id: game.account_id.clone(),
        game_id: game.id.clone(),
    };
    let queue = AppState::get(cx).moves.read(cx);
    let job = queue.jobs().iter().rev().find(|job| job.key == key)?;
    let label = |text: String| {
        div()
            .text_size(px(11.))
            .text_color(rgb(theme::TEXT_MUTED))
            .truncate()
            .child(text)
            .into_any_element()
    };
    let bar_id = "move-bar";
    let (live, lines) = match &job.status {
        MoveStatus::Queued => (
            true,
            vec![
                progress_bar(bar_id, None, 4.).into_any_element(),
                label(t!("library.detail.move.waiting", to = job.to.display()).into_owned()),
            ],
        ),
        MoveStatus::Moving(progress) => (
            true,
            vec![
                progress_bar(
                    bar_id,
                    progress.snapshot().and_then(|(done, total)| move_fraction(done, total)),
                    4.,
                )
                .into_any_element(),
                label(moving_text(progress)),
            ],
        ),
        MoveStatus::Failed(err) => (
            false,
            vec![theme::error(t!("library.detail.move.failed", error = err)).into_any_element()],
        ),
        MoveStatus::Skipped(reason) => (false, vec![label(reason.to_string())]),
        MoveStatus::Moved => return None,
    };
    Some((
        live,
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .min_w(px(220.))
            .children(lines)
            .into_any_element(),
    ))
}
