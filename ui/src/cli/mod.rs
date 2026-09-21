//! Headless CLI, folded into the same binary as the GUI so decanter ships as
//! one executable instead of a separate `decanter-sidecar`. See `decanter-gog`
//! (library listing, installs, achievement polling — all shared with the
//! GUI) and [`launch`] (resolving and running an account's install
//! through `decanter-runtime`) for the actual work; this module is dispatch
//! only. Every subcommand takes `--account-id`, not a profile: these are
//! single-account operations, and the profile that groups accounts is a
//! GUI-only concept the CLI has no need for.
//!
//! `main` calls [`Cli::parse`] before ever touching dioxus: a recognized
//! subcommand runs headless and exits, and no subcommand (the ordinary
//! double-click launch) falls through to the GUI unchanged.

use anyhow::Context;
use clap::{Parser, Subcommand};
use decanter_core::Db;

mod install;
mod launch;

#[derive(Parser)]
#[command(name = "decanter-tray")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Launches an account's installed GOG game — resolved live against
    /// decanter's own library, no separate link step — waits for it to exit,
    /// then polls achievements.
    Launch {
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        product_id: String,
    },
    /// Lists an account's GOG library.
    Library {
        #[arg(long)]
        account_id: String,
    },
    /// Polls achievement status for one game without launching it. The GUI
    /// does this itself on selection now — mainly useful headless (cron,
    /// scripts) or for a quick manual re-check.
    Achievements {
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        product_id: String,
    },
    /// Diagnostic: calls `decanter_gog::install` directly and logs progress —
    /// same call the GUI's install button drives, run headless so its
    /// real behavior can be observed independent of the Dioxus app.
    Install {
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        product_id: String,
        /// Catalog title to record — the depot manifest only carries an
        /// install directory, not a display name.
        #[arg(long)]
        title: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        build_id: Option<String>,
    },
    /// Cancels an install left running for this (account, product) —
    /// useful to clear an orphan left by a client that died mid-transfer.
    CancelInstall {
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        product_id: String,
    },
    /// Lists every decanter-hook installation failure recorded for this
    /// game — the forensic detail `abi_scan` would need to be adapted
    /// for whatever `Galaxy64.dll` shape this one shipped. Empty either
    /// means the hook installed fine, or this game has no Galaxy
    /// integration to hook in the first place.
    HookDiagnostics {
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        product_id: String,
    },
}

/// Runs one CLI command to completion and exits the process. Called from
/// `main` only when it parsed a subcommand, so this never returns to a
/// caller that might otherwise go on to launch the GUI.
///
/// The tracing subscriber is already installed by `main` by the time this
/// runs — installing a second one here would panic (`tracing` only ever
/// accepts one global default).
pub fn run(cmd: Cmd) -> ! {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: could not start the async runtime: {err}");
            std::process::exit(1);
        }
    };
    match runtime.block_on(dispatch(cmd)) {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            eprintln!("error: {err:?}");
            std::process::exit(1);
        }
    }
}

async fn dispatch(cmd: Cmd) -> anyhow::Result<()> {
    let mut db = Db::open(decanter_core::default_db_path()).context("opening decanter cache db")?;
    tracing::debug!("opened db for cli command");

    match cmd {
        Cmd::Library { account_id } => {
            tracing::debug!(account_id, "listing gog library");
            let library = crate::core::library_for_account(&account_id).map_err(|err| anyhow::anyhow!(err))?;
            let games = decanter_gog::list_games(&library, &account_id, &mut |_| {}).await?;
            tracing::info!(account_id, count = games.len(), "gog library listed");
            for entry in games {
                println!("{}\t{}", entry.game.id, entry.game.title);
            }
        }
        Cmd::Achievements { account_id, product_id } => {
            tracing::debug!(account_id, product_id, "polling achievements");
            report_poll(decanter_gog::poll_achievements(&mut db, &account_id, &product_id).await?);
        }
        Cmd::Launch { account_id, product_id } => {
            launch::launch(&mut db, &account_id, &product_id).await?;
        }
        Cmd::Install {
            account_id,
            product_id,
            title,
            branch,
            build_id,
        } => {
            install::install(&account_id, &product_id, &title, branch.as_deref(), build_id.as_deref()).await?;
        }
        Cmd::CancelInstall { account_id, product_id } => {
            install::cancel(&account_id, &product_id).await;
            println!("cancelled (or nothing was in flight)");
        }
        Cmd::HookDiagnostics { account_id, product_id } => {
            let diagnostics =
                db.list_hook_diagnostics(&account_id, decanter_core::Storefront::Gog, &product_id)?;
            if diagnostics.is_empty() {
                println!("no hook diagnostics recorded for {product_id}");
            }
            for diagnostic in diagnostics {
                println!("{}\t{}\t{}", diagnostic.recorded_at, diagnostic.stage, diagnostic.detail);
            }
        }
    }

    Ok(())
}

pub(crate) fn report_poll(newly_unlocked: Vec<decanter_core::CachedAchievement>) {
    for achievement in &newly_unlocked {
        tracing::info!(name = %achievement.name, "achievement unlocked");
    }
    if newly_unlocked.is_empty() {
        tracing::info!("no new achievements this session");
    }
}
