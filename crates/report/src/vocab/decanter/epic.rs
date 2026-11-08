//! Warnings and errors of decanter-epic: the literal message of each `tracing::warn!` or
//! `tracing::error!`; the log appends ` job=`, ` exit=` and `: <error>` after it.

use super::super::{Template, t};

#[rustfmt::skip]
pub const TEMPLATES: &[Template] = &[
    t("decanter.epic.session.token_refresh_unanswered", "epic did not answer the token refresh"),
];
