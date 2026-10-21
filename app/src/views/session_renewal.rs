//! The window that offers to renew a GOG session GOG closed.
//!
//! decanter works offline: a closed session leaves the library as it was,
//! and only downloads, cloud saves and achievements need it back. So this
//! asks once instead of the library showing an error. Renewing first tries
//! the stored refresh token; only when GOG refuses that too does it fall
//! back to the browser login, which must land on the same account.

use std::rc::Rc;

use decanter_core::{Account, Storefront};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, IntoElement, Subscription, Window, div, prelude::*, px, rgb,
};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::AppState;
use crate::theme;
use crate::views::chrome;
use crate::views::gog_login::{GogLogin, LoggedIn};
use crate::widgets::{ButtonKind, button};

/// The session works again; carries the account's name as GOG has it now.
pub struct Renewed(pub String);

#[derive(Clone, PartialEq)]
enum Step {
    Offer,
    Renewing,
    Unreachable,
    Login,
    Failed(Text),
}

pub struct SessionRenewal {
    account: Account,
    step: Step,
    login: Entity<GogLogin>,
    _logged_in: Subscription,
    /// The title the window has, so it's only set again when the language changes.
    title: String,
}

impl EventEmitter<Renewed> for SessionRenewal {}

/// Opens the renewal window for `account`. `renew_now` skips the offer and
/// starts renewing, for a caller where the user already asked to.
pub fn open(account: Account, renew_now: bool, cx: &mut App) -> Option<Entity<SessionRenewal>> {
    let title = t!("library.session_renewal.window_title").into_owned();
    let options = chrome::window_options(&title, 520., 400., 420., 320., cx);
    cx.open_window(options, |window, cx| {
        cx.new(|cx| SessionRenewal::new(account, renew_now, title, window, cx))
    })
    .and_then(|handle| handle.update(cx, |_, _, cx| cx.entity()))
    .inspect_err(|err| tracing::error!(%err, "cannot open the session renewal window"))
    .ok()
}

/// Files a GOG login under `profile_id`, or renames the account already
/// there to the name GOG now has for it.
pub fn save_account(account: &decanter_gog::Account, profile_id: &str) -> Result<(), Text> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs().cast_signed());
    crate::core::db()
        .and_then(|db| {
            db.upsert_account(
                &account.account_id,
                Storefront::Gog,
                profile_id,
                &account.display_name,
                now,
            )
            .map_err(crate::core::StoreError::Query)
        })
        .map(|_| ())
        .inspect_err(|err| tracing::error!(profile_id, error = %err, "failed to save gog account"))
        .map_err(Text::from)
}

impl SessionRenewal {
    fn new(account: Account, renew_now: bool, title: String, window: &Window, cx: &mut Context<Self>) -> Self {
        let login = cx.new(GogLogin::new);
        let logged_in = cx.subscribe_in(&login, window, |this, _, LoggedIn(account), window, cx| {
            this.logged_in(account, window, cx);
        });
        let mut this = Self {
            account,
            step: Step::Offer,
            login,
            _logged_in: logged_in,
            title,
        };
        if renew_now {
            this.renew(window, cx);
        }
        this
    }

    fn renew(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.step = Step::Renewing;
        cx.notify();
        let account_id = self.account.id.clone();
        tracing::info!(account_id, "renewing gog session");
        cx.spawn_in(window, async move |this, cx| {
            let state = decanter_gog::renew_session(&account_id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.step = match state {
                    Ok(decanter_gog::SessionState::Valid(account)) => {
                        this.finish(&account, window, cx);
                        return;
                    }
                    Ok(decanter_gog::SessionState::Closed) => {
                        tracing::info!(account_id, "gog refused the refresh token, browser login needed");
                        Step::Login
                    }
                    Ok(decanter_gog::SessionState::Unreachable(reason)) => {
                        tracing::info!(account_id, reason, "gog did not answer the session renewal");
                        Step::Unreachable
                    }
                    Err(err) => {
                        tracing::error!(account_id, error = %err, "gog session renewal failed locally");
                        Step::Failed(Text::verbatim(err))
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// A browser login only renews this session if it was this account: any
    /// other one is turned away, and its session dropped again unless it's an
    /// account decanter already knows, since the login just stored it under
    /// that other id.
    fn logged_in(&self, account: &decanter_gog::Account, window: &mut Window, cx: &mut Context<Self>) {
        if account.account_id == self.account.id {
            self.finish(account, window, cx);
            return;
        }
        tracing::warn!(
            account_id = %self.account.id,
            other_account_id = %account.account_id,
            "gog login for session renewal landed on another account"
        );
        let message = Text::new("library.session_renewal.other_account")
            .arg("other", &account.display_name)
            .arg("name", &self.account.name);
        self.login.update(cx, |login, cx| login.fail(message, cx));
        match crate::core::db().and_then(|db| {
            db.profile_id_for_account(&account.account_id, Storefront::Gog)
                .map_err(crate::core::StoreError::Query)
        }) {
            Ok(Some(_)) => {}
            Ok(None) => {
                let other = account.account_id.clone();
                cx.spawn(async move |_, _| {
                    if let Err(err) = decanter_gog::log_out(&other).await {
                        tracing::error!(account_id = other, error = %err, "failed to drop the stray gog session");
                    }
                })
                .detach();
            }
            Err(err) => {
                tracing::warn!(account_id = %account.account_id, error = %err, "cannot tell whether the other gog account is known, keeping its session");
            }
        }
    }

    fn finish(&self, account: &decanter_gog::Account, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = save_account(account, &self.account.profile_id) {
            self.login.update(cx, |login, cx| login.fail(err, cx));
            return;
        }
        tracing::info!(account_id = %self.account.id, "gog session renewed");
        AppState::get(cx)
            .library
            .clone()
            .update(cx, |library, cx| library.session_renewed(&self.account.id, cx));
        cx.emit(Renewed(account.display_name.clone()));
        window.remove_window();
    }

    fn body(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let renewing = self.step == Step::Renewing;
        let actions = div()
            .flex()
            .gap(px(10.))
            .when(self.step != Step::Login, |actions| {
                actions.child(
                    button(
                        "renew-session",
                        ButtonKind::Primary,
                        if renewing {
                            t!("library.session_renewal.renewing")
                        } else {
                            t!("library.session_renewal.renew")
                        },
                        renewing,
                    )
                    .when(!renewing, |b| {
                        b.on_click(cx.listener(|this, _, window, cx| this.renew(window, cx)))
                    }),
                )
            })
            .child(
                button(
                    "renew-later",
                    ButtonKind::Secondary,
                    t!("library.session_renewal.later"),
                    false,
                )
                .on_click(|_, window, _| window.remove_window()),
            );
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().child(t!("library.session_renewal.closed", name = self.account.name)))
            .children(match &self.step {
                Step::Offer | Step::Renewing => None,
                Step::Unreachable => Some(theme::muted(t!("library.session_renewal.unreachable")).into_any_element()),
                Step::Login => Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(theme::muted(t!(
                            "library.session_renewal.login",
                            name = self.account.name
                        )))
                        .child(self.login.clone())
                        .into_any_element(),
                ),
                Step::Failed(message) => Some(theme::error(message.clone()).into_any_element()),
            })
            .child(actions)
    }
}

impl Render for SessionRenewal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = t!("library.session_renewal.window_title");
        if title != self.title {
            window.set_window_title(&title);
            self.title = title.into_owned();
        }
        let on_close: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(|this_window, _| this_window.remove_window());
        let topbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_6()
            .h(px(52.))
            .px(px(18.))
            .bg(rgb(theme::BG_PANEL_3))
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(chrome::logo(&self.title.to_uppercase()))
            .child(chrome::drag_handle(div().flex_1().h_full()))
            .child(chrome::window_controls(on_close));

        // Same root styling the main window's `Root` sets, since a second
        // window inherits none of it.
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_DEEP))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .font_family(theme::SANS)
            .text_size(px(14.))
            .line_height(gpui::relative(1.5))
            .child(topbar)
            .child(
                div()
                    .id("session-renewal-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_6()
                    .child(self.body(cx)),
            )
            .children(chrome::resize_edges())
    }
}
