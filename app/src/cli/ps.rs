//! `decanter ps ...`: the PS section's headless commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Subcommand, ValueEnum};
use decanter_core::Db;
use decanter_ps::failures::Log;

use crate::i18n::Text;
use crate::report::send::{self, Deleted, Server, Submitted};
use crate::report::sent::{self, Entry};
use crate::report::{self, Built};

#[derive(Subcommand)]
pub enum PsCmd {
    /// Builds an error report from a recorded failure and saves it, or prints it: the report holds no
    /// paths, names or accounts, and this shows exactly what it holds. Nothing leaves the machine
    /// unless `--send --yes` is given.
    Report(Report),
    /// Plays a game headless, as the window's Play does, and records its failures where Play records
    /// them, for `decanter ps report`. The game's output goes to its run log in the library; at the end
    /// a summary is printed. Never connects to online services, whatever the settings say. Exits 0 for a
    /// clean run, 1 for a crash, STOP or bad exit, or a timeout, and 2 when the game could not start.
    Run {
        /// The game's title id (`CUSA03173`), as the PS section lists it.
        #[arg(long, value_parser = title_id)]
        title: String,
        /// Asks the game to stop after this many seconds, then kills it if it is still running
        /// after a grace period; the run is recorded as ended by the timeout, not as a crash.
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=super::ps_run::MAX_TIMEOUT))]
        timeout: Option<u64>,
        /// A test variable for the game, `PS_NAME=value`; repeatable, up to 32. Online variables are
        /// refused.
        #[arg(long = "env", value_name = "NAME=VALUE", value_parser = super::ps_run::env_pair)]
        env: Vec<(String, String)>,
    },
}

#[derive(Args)]
pub struct Report {
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
    /// Shows the report and what would be sent; with `--yes` it is sent, publicly, to the report
    /// server (the address is in the settings).
    #[arg(long, conflicts_with_all = ["sent", "delete"])]
    send: bool,
    /// Confirms `--send` or `--delete`; without it nothing touches the network.
    #[arg(long)]
    yes: bool,
    /// With `--send --yes`: also sends the run's log, privately, for whoever maintains the server.
    #[arg(long, requires_all = ["send", "yes"])]
    attach_log: bool,
    /// Lists the reports sent from this library, without their delete codes.
    #[arg(long, conflicts_with_all = ["send", "delete"])]
    sent: bool,
    /// Deletes a sent report from the server, using the code kept in the library; needs `--yes`.
    #[arg(long, value_name = "ID", value_parser = report_id, conflicts_with_all = ["send", "sent"])]
    delete: Option<String>,
    /// For development: a server other than the settings' one, `https://` or `http://127.0.0.1:PORT`.
    #[arg(long, hide = true, value_parser = server_address)]
    server: Option<String>,
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

fn report_id(value: &str) -> Result<String, String> {
    if send::is_id(value) {
        Ok(value.to_owned())
    } else {
        Err(format!("{value} isn't a report id"))
    }
}

fn server_address(value: &str) -> Result<String, String> {
    Server::parse(value)
        .map(|_| value.to_owned())
        .map_err(|_| format!("{value} isn't an accepted server address"))
}

fn day(value: &str) -> Result<String, String> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|_| value.to_owned())
        .map_err(|_| format!("{value} isn't a day as YYYY-MM-DD"))
}

/// The process's exit code.
pub async fn run(db: &Db, cmd: PsCmd) -> anyhow::Result<i32> {
    crate::i18n::init();
    match cmd {
        PsCmd::Report(args) => report_cmd(db, args).await.map(|()| 0),
        PsCmd::Run { title, timeout, env } => Ok(super::ps_run::run(db, &title, timeout, env).await.code()),
    }
}

async fn report_cmd(db: &Db, args: Report) -> anyhow::Result<()> {
    let Report {
        product,
        title,
        failure,
        day,
        print,
        out,
        send,
        yes,
        attach_log,
        sent,
        delete,
        server,
    } = args;
    let Some(profile) = db.active_profile().context("reading the active profile")? else {
        bail!("{}", Text::new("ps.report.errors.no_profile"));
    };
    let library = crate::core::library_for_profile(&profile.id).map_err(|err| anyhow::anyhow!(err))?;
    decanter_ps::paths::use_library(library.root());
    if sent {
        return list_sent();
    }
    if let Some(id) = delete {
        return delete_sent(&id, yes).await;
    }
    let server = if send {
        let server = send::resolve(send::setting(db).as_deref(), server.as_deref())
            .map_err(|err| anyhow::anyhow!("{}", err.text()))?;
        Some(server.ok_or_else(|| anyhow::anyhow!("{}", Text::new("ps.report.send.off")))?)
    } else {
        None
    };
    let nth = usize::try_from(failure - 1).unwrap_or(usize::MAX);
    let (built, log) = match product {
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
            (report::psport(picked), picked.log.clone())
        }
        Product::Decanter => (
            report::day_failures(day.as_deref()).and_then(|failures| {
                failures
                    .iter()
                    .rev()
                    .nth(nth)
                    .map_or(Err(report::Error::NoFailure), report::decanter)
            }),
            None,
        ),
    };
    let built = built.map_err(|err| anyhow::anyhow!("{}", err.text()))?;
    if attach_log && log.is_none() {
        bail!("{}", Text::new("ps.report.send.no_log"));
    }
    match server {
        Some(server) => {
            send_report(
                &built,
                &server,
                yes,
                log.filter(|_| attach_log).as_deref(),
                out.as_deref(),
            )
            .await
        }
        None => deliver(&built, print, out.as_deref()),
    }
}

fn deliver(built: &Built, print: bool, out: Option<&Path>) -> anyhow::Result<()> {
    if print {
        println!("{}", built.json);
    }
    if !print || out.is_some() {
        let path = report::save(built, out).map_err(|err| anyhow::anyhow!("{}", err.text()))?;
        println!("{}", Text::new("ps.report.saved").arg("path", path.display()));
    }
    Ok(())
}

async fn send_report(
    built: &Built,
    server: &Server,
    yes: bool,
    log: Option<&Path>,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    println!("{}", built.json);
    if let Some(out) = out {
        let path = report::save(built, Some(out)).map_err(|err| anyhow::anyhow!("{}", err.text()))?;
        println!("{}", Text::new("ps.report.saved").arg("path", path.display()));
    }
    if !yes {
        let key = if log.is_some() {
            "ps.report.send.dry_run_log"
        } else {
            "ps.report.send.dry_run"
        };
        println!("{}", Text::new(key).arg("server", server.origin()));
        println!("{}", Text::new("ps.report.privacy"));
        return Ok(());
    }
    match send::submit(built, server, log)
        .await
        .map_err(|err| anyhow::anyhow!("{}", err.text()))?
    {
        Submitted::Duplicate(earlier) => {
            bail!("{}", Text::new("ps.report.send.duplicate").arg("url", earlier.url));
        }
        Submitted::Sent {
            entry,
            unrecorded,
            attached,
        } => {
            println!("{}", Text::new("ps.report.send.sent").arg("url", &entry.url));
            println!("{}", Text::new("ps.report.send.code").arg("code", &entry.delete_code));
            if let Some(err) = unrecorded {
                eprintln!("{}", Text::new("ps.report.send.unrecorded").text("error", err.text()));
            }
            match attached {
                Some(Ok(())) => println!("{}", Text::new("ps.report.send.log_attached")),
                Some(Err(err)) => {
                    eprintln!("{}", Text::new("ps.report.send.log_failed").text("error", err.text()));
                    bail!("{}", Text::new("ps.report.send.sent_without_log"));
                }
                None => {}
            }
            Ok(())
        }
    }
}

fn list_sent() -> anyhow::Result<()> {
    let entries = sent::load().map_err(|err| anyhow::anyhow!("{}", err.text()))?;
    if entries.is_empty() {
        println!("{}", Text::new("ps.report.send.none_sent"));
    }
    for Entry { id, url, day, .. } in entries {
        println!("{id}\t{day}\t{url}");
    }
    Ok(())
}

async fn delete_sent(id: &str, yes: bool) -> anyhow::Result<()> {
    if !yes {
        println!("{}", Text::new("ps.report.send.delete_dry_run").arg("id", id));
        return Ok(());
    }
    match send::delete(id)
        .await
        .map_err(|err| anyhow::anyhow!("{}", err.text()))?
    {
        Deleted::Removed => println!("{}", Text::new("ps.report.send.deleted").arg("id", id)),
        Deleted::Gone => println!("{}", Text::new("ps.report.send.gone").arg("id", id)),
    }
    Ok(())
}
