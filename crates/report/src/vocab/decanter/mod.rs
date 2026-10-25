//! decanter's own warnings and errors, as its day log carries them (`decanter-YYYY-MM-DD.log`),
//! one table per area. What the log appends after the message is read by [`super::pattern`]
//! (` job=`, ` exit=`, written into the first entries) or [`super::cause`] (`: <error>`).

mod app;
mod core;
mod gog;
mod ps;
mod runtime;
mod state;
mod views;

use super::{Template, t};

#[rustfmt::skip]
const JOBS: &[Template] = &[
    t("decanter.job.failed", "job failed job={build|game|install|update|rollback} exit=code {number}"),
    t("decanter.job.failed_signal", "job failed job={build|game|install|update|rollback} exit=signal {number}"),
    t("decanter.job.failed_other_exit", "job failed job={build|game|install|update|rollback} exit={_}"),
    t("decanter.job.failed_no_exit", "job failed job={build|game|install|update|rollback}"),
];

pub const TABLES: &[&[Template]] = &[
    JOBS,
    app::TEMPLATES,
    core::TEMPLATES,
    gog::TEMPLATES,
    ps::TEMPLATES,
    runtime::TEMPLATES,
    state::TEMPLATES,
    views::TEMPLATES,
];
