//! The Epic Games side of the accounts tab: its session check, its logout and the filing of its
//! login.

use decanter_core::Storefront;
use decanter_epic::SessionState;
use gpui::{AnyElement, Context, IntoElement as _, ParentElement as _};

use super::{AccountStatus, AccountTab, SessionCheck, column};
use crate::i18n::Text;
use crate::views::epic_login::EpicLogin;

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

    pub(super) fn epic_section(&self) -> AnyElement {
        column().child(self.epic_login.clone()).into_any_element()
    }
}
