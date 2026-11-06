//! The notice shown before the first Amazon Games login.
//!
//! The login registers a device that presents itself as Amazon's own launcher, and Amazon has
//! published no permission for third-party clients. So the user accepts that once per profile
//! before the account row starts the login.

use gpui::{App, ClickEvent, Div, ElementId, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

/// Reading the setting failing counts as not accepted: the notice is shown again rather than skipped.
pub fn needs_consent(profile_id: &str) -> bool {
    crate::core::db()
        .and_then(|db| db.amazon_consent(profile_id).map_err(crate::core::StoreError::Query))
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to read amazon_consent setting"))
        .map_or(true, |accepted| !accepted)
}

pub fn record_consent(profile_id: &str) -> Result<(), Text> {
    crate::core::db()
        .and_then(|db| {
            db.set_amazon_consent(profile_id, true)
                .map_err(crate::core::StoreError::Query)
        })
        .inspect(|()| tracing::info!(profile_id, "amazon consent accepted"))
        .inspect_err(|err| tracing::error!(profile_id, error = %err, "failed to save amazon_consent setting"))
        .map_err(Text::from)
}

/// The notice itself, to sit inside the account row. `on_accept` should call [`record_consent`]
/// and then start the login; `on_cancel` just closes the panel.
pub fn show_consent(
    id: impl Into<ElementId>,
    on_accept: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let id = id.into();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .border_1()
        .border_color(rgb(theme::ACCENT_GOLD))
        .rounded_md()
        .child(theme::h3(t!("amazon.consent.title")))
        .child(div().child(t!("amazon.consent.device")))
        .child(div().child(t!("amazon.consent.storage")))
        .child(
            div()
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(t!("amazon.consent.risk")),
        )
        .child(theme::muted(t!("amazon.consent.affiliation")))
        .child(theme::muted(t!("amazon.consent.removal")))
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(
                    button(
                        (id.clone(), "accept"),
                        ButtonKind::Primary,
                        t!("amazon.consent.accept"),
                        false,
                    )
                    .on_click(on_accept),
                )
                .child(button((id, "cancel"), ButtonKind::Secondary, t!("common.cancel"), false).on_click(on_cancel)),
        )
}
