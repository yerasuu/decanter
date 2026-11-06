//! The Amazon Games side of the accounts tab: its session check, its logout and the filing of its
//! login. Linux only, since decanter's Amazon support is (decision #1518, D10).

use decanter_amazon::{Deregistration, SessionState};
use decanter_core::Storefront;
use gpui::{AnyElement, Context, IntoElement as _, ParentElement as _, Styled as _};

use super::{AccountStatus, AccountTab, SessionCheck, column};
use crate::i18n::Text;
use crate::state::AppState;
use crate::theme;
use crate::views::amazon_login::AmazonLogin;

impl AccountTab {
    /// Reads only what the keyring says, with no call to Amazon: the library loop renews the
    /// session when it lists, so only a closed one is worth telling here.
    pub(super) fn check_amazon_session(account: &decanter_core::Account, cx: &Context<Self>) {
        let account_id = account.id.clone();
        let name = account.name.clone();
        cx.spawn(async move |this, cx| {
            let check = match decanter_amazon::session_state(&account_id).await {
                Ok(SessionState::Valid | SessionState::NeedsRefresh) => SessionCheck::Valid(name),
                Ok(SessionState::LoggedOut | SessionState::Refused) => {
                    tracing::info!(account_id, "amazon session closed");
                    SessionCheck::Closed
                }
                Ok(_) => SessionCheck::Unreachable,
                Err(err) => {
                    tracing::error!(account_id, error = %format_args!("{err:#}"), "amazon session check failed");
                    SessionCheck::Failed(format!("{err:#}"))
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(row) = this.row_mut(&account_id, Storefront::AmazonGames) {
                    row.session = check;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn log_out_amazon(&mut self, account_id: String, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(&account_id, Storefront::AmazonGames) {
            row.status = AccountStatus::Working;
        }
        cx.notify();
        tracing::info!(account_id, "logging out of amazon");
        let library = AppState::get(cx).library.clone();
        cx.spawn(async move |this, cx| {
            let notice = match decanter_amazon::log_out(&account_id).await {
                Ok(Deregistration::Refused) => Some(Text::new("amazon.logout.refused")),
                Ok(Deregistration::Unreachable(reason)) => {
                    tracing::info!(account_id, reason, "amazon device not deregistered");
                    Some(Text::new("amazon.logout.unreachable"))
                }
                Ok(_) => None,
                Err(err) => {
                    tracing::error!(account_id, error = %format_args!("{err:#}"), "amazon logout failed");
                    Self::set_row_status(
                        &this,
                        &account_id,
                        Storefront::AmazonGames,
                        AccountStatus::Failed(Text::verbatim(format!("{err:#}"))),
                        cx,
                    );
                    return;
                }
            };
            match crate::core::db().and_then(|db| {
                db.forget_account(&account_id, Storefront::AmazonGames)
                    .map_err(crate::core::StoreError::Query)
            }) {
                Ok(()) => {
                    tracing::info!(account_id, "amazon account removed from profile");
                    library.update(cx, |library, cx| library.forget_amazon_account(&account_id, cx));
                    let _ = this.update(cx, |this, cx| {
                        this.amazon_notice = notice;
                        this.reload_accounts(cx);
                    });
                }
                Err(err) => {
                    tracing::error!(account_id, error = %err, "failed to remove amazon account from profile");
                    Self::set_row_status(
                        &this,
                        &account_id,
                        Storefront::AmazonGames,
                        AccountStatus::Failed(err.localized()),
                        cx,
                    );
                }
            }
        })
        .detach();
    }

    /// Files an Amazon login under this profile, or renames the account already there.
    pub(super) fn connect_amazon(&mut self, account: &decanter_amazon::Account, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.as_str();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs().cast_signed());
        let saved = crate::core::db().and_then(|db| {
            db.upsert_account(
                &account.account_id,
                Storefront::AmazonGames,
                profile_id,
                &account.display_name,
                now,
            )
            .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::error!(profile_id, error = %err, "failed to save amazon account");
            self.amazon_login
                .update(cx, |login, cx| login.fail(Text::from(err), cx));
            return;
        }
        tracing::info!(profile_id, account_id = %account.account_id, "amazon account added");
        AppState::get(cx)
            .library
            .clone()
            .update(cx, |library, cx| library.amazon_logged_in(&account.account_id, cx));
        self.amazon_notice = None;
        self.amazon_login.update(cx, AmazonLogin::reset);
        // A row that was there keeps its old state through the reload, so it is checked again;
        // a new one is checked by the reload itself.
        let existed = self.row_mut(&account.account_id, Storefront::AmazonGames).is_some();
        self.reload_accounts(cx);
        if existed && let Some(row) = self.row_mut(&account.account_id, Storefront::AmazonGames) {
            row.session = SessionCheck::Pending;
            let renamed = row.account.clone();
            Self::check_amazon_session(&renamed, cx);
        }
    }

    pub(super) fn amazon_section(&self) -> AnyElement {
        column()
            .child(self.amazon_login.clone())
            .children(self.amazon_notice.clone().map(|notice| theme::error(notice).mt_2()))
            .into_any_element()
    }
}
