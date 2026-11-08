//! The Epic Games side of the accounts tab: its session check, its logout and the filing of its
//! login.

use decanter_core::Storefront;
use decanter_epic::SessionState;
use gpui::{AnyElement, Context, div, prelude::*};
use rust_i18n::t;

use super::{AccountStatus, AccountTab, SessionCheck, column};
use crate::i18n::Text;
use crate::state::AppState;
use crate::state::gog_library::EpicPhase;
use crate::theme;
use crate::views::epic_login::EpicLogin;
use crate::widgets::{ButtonKind, button};

impl AccountTab {
    /// Asks Epic, as GOG's row does: nothing else checks an Epic session until its library is
    /// listed.
    pub(super) fn check_epic_session(account: &decanter_core::Account, cx: &Context<Self>) {
        let account_id = account.id.clone();
        cx.spawn(async move |this, cx| {
            tracing::debug!(account_id, "checking epic session status");
            let check = match decanter_epic::check_session(&account_id).await {
                Ok(SessionState::Valid(account)) => SessionCheck::Valid(account.display_name),
                Ok(SessionState::Closed) => {
                    tracing::info!(account_id, "epic session closed");
                    SessionCheck::Closed
                }
                Ok(SessionState::Unreachable(reason)) => {
                    tracing::info!(account_id, reason, "epic session could not be checked");
                    SessionCheck::Unreachable
                }
                Ok(_) => SessionCheck::Unreachable,
                Err(err) => {
                    tracing::error!(account_id, error = %format_args!("{err:#}"), "epic session check failed");
                    SessionCheck::Failed(format!("{err:#}"))
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(row) = this.row_mut(&account_id, Storefront::EpicGames) {
                    row.session = check;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn log_out_epic(&mut self, account_id: String, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(&account_id, Storefront::EpicGames) {
            row.status = AccountStatus::Working;
        }
        cx.notify();
        tracing::info!(account_id, "logging out of epic");
        let library = AppState::get(cx).library.clone();
        cx.spawn(async move |this, cx| {
            if let Err(err) = decanter_epic::log_out(&account_id).await {
                tracing::error!(account_id, error = %format_args!("{err:#}"), "epic logout failed");
                Self::set_row_status(
                    &this,
                    &account_id,
                    Storefront::EpicGames,
                    AccountStatus::Failed(Text::verbatim(format!("{err:#}"))),
                    cx,
                );
                return;
            }
            match crate::core::db().and_then(|db| {
                db.forget_account(&account_id, Storefront::EpicGames)
                    .map_err(crate::core::StoreError::Query)
            }) {
                Ok(()) => {
                    tracing::info!(account_id, "epic account removed from profile");
                    library.update(cx, |library, cx| library.forget_epic_account(&account_id, cx));
                    let _ = this.update(cx, |this, cx| this.reload_accounts(cx));
                }
                Err(err) => {
                    tracing::error!(account_id, error = %err, "failed to remove epic account from profile");
                    Self::set_row_status(
                        &this,
                        &account_id,
                        Storefront::EpicGames,
                        AccountStatus::Failed(err.localized()),
                        cx,
                    );
                }
            }
        })
        .detach();
    }

    /// Files an Epic login under this profile, or renames the account already there.
    pub(super) fn connect_epic(&mut self, account: &decanter_epic::Account, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.as_str();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs().cast_signed());
        let saved = crate::core::db().and_then(|db| {
            db.upsert_account(
                &account.account_id,
                Storefront::EpicGames,
                profile_id,
                &account.display_name,
                now,
            )
            .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::error!(profile_id, error = %err, "failed to save epic account");
            self.epic_login.update(cx, |login, cx| login.fail(Text::from(err), cx));
            return;
        }
        tracing::info!(profile_id, account_id = %account.account_id, "epic account added");
        AppState::get(cx)
            .library
            .clone()
            .update(cx, |library, cx| library.refresh_epic(&account.account_id, cx));
        self.epic_login.update(cx, EpicLogin::reset);
        // A row that was there keeps its old state through the reload, so it is checked again;
        // a new one is checked by the reload itself.
        let existed = self.row_mut(&account.account_id, Storefront::EpicGames).is_some();
        self.reload_accounts(cx);
        if existed && let Some(row) = self.row_mut(&account.account_id, Storefront::EpicGames) {
            row.session = SessionCheck::Pending;
            let renamed = row.account.clone();
            Self::check_epic_session(&renamed, cx);
        }
    }

    /// The account's library under its row: how many games the last listing left and when, where
    /// the current one stands, and a button to list it again now.
    pub(super) fn epic_library_line(
        index: usize,
        account_id: &str,
        last: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let status = AppState::get(cx).library.read(cx).epic_status(account_id)?;
        let synced = status
            .last_sync
            .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
            .map_or_else(
                || t!("epic.library.never"),
                |at| {
                    t!(
                        "epic.library.synced",
                        when = crate::views::library::fmt::date_time(at.with_timezone(&chrono::Local))
                    )
                },
            );
        let state = match status.phase {
            EpicPhase::Listing => Some(theme::muted(t!("epic.library.listing"))),
            EpicPhase::Listed | EpicPhase::SessionClosed => None,
            EpicPhase::AssetsTimedOut { waited_secs } => {
                Some(theme::error(t!("epic.library.assets_timed_out", seconds = waited_secs)))
            }
            EpicPhase::AssetsFailed => Some(theme::error(t!("epic.library.assets_failed"))),
            EpicPhase::Failed => Some(theme::error(t!("epic.library.failed"))),
        };
        let listing = status.phase == EpicPhase::Listing;
        let label = if matches!(
            status.phase,
            EpicPhase::AssetsTimedOut { .. } | EpicPhase::AssetsFailed | EpicPhase::Failed
        ) {
            t!("epic.library.retry")
        } else {
            t!("epic.library.refresh")
        };
        let account_id = account_id.to_owned();
        let line = theme::list_row(("epic-library", index), last)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(theme::muted(t!("epic.library.games", count = status.games)))
                    .child(theme::muted(synced))
                    .children(state),
            )
            .child(
                button(("epic-refresh", index), ButtonKind::Secondary, label, listing).when(!listing, |b| {
                    b.on_click(cx.listener(move |_, _, _, cx| {
                        tracing::info!(account_id, "epic library listing asked for from settings");
                        AppState::get(cx)
                            .library
                            .clone()
                            .update(cx, |library, cx| library.refresh_epic(&account_id, cx));
                    }))
                }),
            )
            .into_any_element();
        Some(line)
    }

    pub(super) fn epic_section(&self) -> AnyElement {
        column().child(self.epic_login.clone()).into_any_element()
    }
}
