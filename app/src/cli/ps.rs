//! `decanter ps ...`: the PS section's headless commands.

use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use decanter_core::Db;
use decanter_ps::failures::Log;

use crate::i18n::Text;
use crate::report::{self, Built};

#[derive(Subcommand)]
pub enum PsCmd {
    /// Builds an error report from a recorded failure and saves it, or prints it, without sending
    /// anything: the report holds no paths, names or accounts, and this shows exactly what it holds.
    Report {
        #[arg(long, value_enum, default_value = "psport")]
        product: Product,
        /// psport: only the failures of this title (`CUSA03173`).
        #[arg(long, value_parser = title_id)]
        title: Option<String>,
        /// Which failure, counting the newest as 1.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        failure: u32,
        /// decanter: the day log to read (`2026-10-25`), the latest by default.
        #[arg(long, value_parser = day)]
        day: Option<String>,
        /// Prints the report as JSON, as it would go out, and saves nothing unless `--out` is given.
        #[arg(long)]
        print: bool,
        /// Where to save it; by default `decanter/reports/<date>-<fingerprint>.json` in the library.
        /// An existing file is never replaced.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Product {
    Psport,
    Decanter,
}

fn title_id(value: &str) -> Result<String, String> {
    if (1..=16).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(value.to_owned())
    } else {
        Err(format!("{value} isn't a title id"))
    }
}

fn day(value: &str) -> Result<String, String> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|_| value.to_owned())
        .map_err(|_| format!("{value} isn't a day as YYYY-MM-DD"))
}

pub fn run(db: &Db, cmd: PsCmd) -> anyhow::Result<()> {
    crate::i18n::init();
    let PsCmd::Report {
        product,
        title,
        failure,
        day,
        print,
        out,
    } = cmd;
    let Some(profile) = db.active_profile().context("reading the active profile")? else {
        bail!("{}", Text::new("ps.report.errors.no_profile"));
    };
    let library = crate::core::library_for_profile(&profile.id).map_err(|err| anyhow::anyhow!(err))?;
    decanter_ps::paths::use_library(library.root());
    let nth = usize::try_from(failure - 1).unwrap_or(usize::MAX);
    let built = match product {
        Product::Psport => {
            let path = decanter_ps::paths::active().profile(&profile.id).join("failures.jsonl");
            let records = Log::load(path);
            let picked = records
                .records()
                .iter()
                .rev()
                .filter(|record| title.as_deref().is_none_or(|title| record.title_id == title))
                .nth(nth);
            let Some(picked) = picked else {
                bail!("{}", report::Error::NoFailure.text());
            };
            report::psport(picked)
        }
        Product::Decanter => report::day_failures(day.as_deref()).and_then(|failures| {
            failures
                .iter()
                .rev()
                .nth(nth)
                .map_or(Err(report::Error::NoFailure), report::decanter)
        }),
    };
    deliver(built, print, out)
}

fn deliver(built: Result<Built, report::Error>, print: bool, out: Option<PathBuf>) -> anyhow::Result<()> {
    let built = built.map_err(|err| anyhow::anyhow!("{}", err.text()))?;
    if print {
        println!("{}", built.json);
    }
    if !print || out.is_some() {
        let path = report::save(&built, out.as_deref()).map_err(|err| anyhow::anyhow!("{}", err.text()))?;
        println!("{}", Text::new("ps.report.saved").arg("path", path.display()));
    }
    Ok(())
}
