//! Warnings and errors of decanter-amazon: the literal message of each `tracing::warn!` or
//! `tracing::error!`; the log appends ` job=`, ` exit=` and `: <error>` after it.

use super::super::{Template, t};

#[rustfmt::skip]
pub const TEMPLATES: &[Template] = &[
    t("decanter.amazon.device.registered_under_another_serial", "amazon registered the device under another serial"),
    t("decanter.amazon.session.token_refresh_unanswered", "amazon did not answer the token refresh"),
    t("decanter.amazon.session.previous_device_left_registered", "previous amazon device left registered after a new login"),
];
