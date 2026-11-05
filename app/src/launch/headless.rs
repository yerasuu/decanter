//! A PC game played without the window, for `decanter launch` (a Steam shortcut, a task bar
//! entry): the same resolution, mods, launch plan, prefix preparation, cloud saves, Galaxy service,
//! run log and achievements as [`super::start`], with the session claimed so the window shows it
//! running and can stop it. What only the window has stays the window's: the Registro tab's live
//! lines (the run's log file holds them), the close chord and the hints added after a failure.

use std::process::ExitStatus;
use std::time::Instant;

use anyhow::bail;
use decanter_core::{Db, InstalledGame};
use decanter_ps::process::{self, Event, JobKind};
use decanter_ps::run_log::{RunLog, describe_exit};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{Kind, Ready};

/// Lines taken per batch, as the window's runs take them.
const BATCH: usize = 256;

/// Plays `game` until it ends, stopping it when `stop` resolves first (the command was asked to
/// stop), and records its playtime in `db`. The session stays claimed until everything is recorded,
/// so a window that sees it end reads the new playtime.
///
/// # Errors
/// When the game is already running, or could not be started; the reason is logged too.
pub async fn play(
    db: &Db,
    account_id: &str,
    game: &InstalledGame,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let claim = match crate::running::claim(crate::running::Game::pc(account_id, &game.game_id)) {
        Ok(claim) => claim,
        Err(busy) => bail!("{busy}"),
    };
    let Ready {
        kind,
        mut planned,
        play,
        ..
    } = match super::ready(account_id, game).await {
        Ok(ready) => ready,
        Err(unready) => bail!("{}", unready.reason()),
    };
    planned.facts.push(("run", "decanter launch".to_owned()));
    let saves_view = play.as_ref().and_then(|play| play.saves_view(game));
    let saves_game = saves_view.as_ref().unwrap_or(game);

    let checkpoint = super::sync_cloud_saves_before_launch(account_id, saves_game).await;
    let achievement_log = planned.achievement_log.take();
    let galaxy = super::galaxy_service(account_id, game).await;
    let notes = if kind == Kind::Umu && game.storefront == decanter_core::Storefront::Gog {
        super::prepare_gog_prefix(account_id, game, &planned.launch).await
    } else {
        Vec::new()
    };

    let facts = core::mem::take(&mut planned.facts);
    let launcher = planned.launcher;
    let mut prepared = match planned.prepare().await {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            bail!("launch failed: {err:#}");
        }
    };
    let mut log = match crate::run_log::start(account_id, &game.game_id, &facts, &mut prepared).await {
        Ok(log) => Some(log),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "could not create the run's log file");
            None
        }
    };
    crate::taskbar::sync_pc(game).await;
    let mut session = match prepared.spawn() {
        Ok(session) => session,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            let reason = format!("{err:#}");
            if let Some(log) = log {
                crate::run_log::abandon(log, &game.game_id, launcher, &reason).await;
            }
            bail!("launch failed: {reason}");
        }
    };
    tracing::info!(
        game_id = %game.game_id,
        name = %game.name,
        log = log.as_ref().map(|log| tracing::field::display(log.path().display())),
        "game launched"
    );
    for line in &notes {
        note(&mut log, line, &game.game_id).await;
    }
    let (stdout, stderr) = session.take_output();
    let waited = game.game_id.clone();
    let (_job, events) = process::follow(JobKind::Game, session.launcher_pid(), stdout, stderr, async move {
        match session.wait_status().await {
            Ok(status) => Some(status),
            Err(err) => {
                tracing::warn!(game_id = %waited, error = %format_args!("{err:#}"), "game session ended with an error");
                None
            }
        }
    });

    let started = Instant::now();
    let exit = follow(events, &mut log, account_id, &game.game_id, stop).await;
    crate::run_log::finish(log, exit, &game.game_id).await;
    drop(galaxy);
    let elapsed = started.elapsed();
    tracing::info!(
        game_id = %game.game_id,
        exit = describe_exit(exit),
        duration_secs = elapsed.as_secs(),
        "game session ended"
    );

    super::sync_cloud_saves_after_exit(account_id, saves_game, checkpoint).await;
    super::record_last_played(account_id, game);
    super::apply_hook_log(account_id, game, achievement_log.as_deref()).await;
    drop(play);
    let seconds = i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX);
    if let Err(err) = db.add_playtime(account_id, game.storefront, &game.game_id, seconds) {
        tracing::warn!(account_id, game_id = %game.game_id, %err, "failed to record playtime");
    }
    drop(claim);
    Ok(())
}

/// The game's output into its log until it ends; asked to stop meanwhile, the game is stopped as
/// the window's Stop does, since under Proton it runs on below `umu-run`, which is all a kill of
/// this command's own process would reach.
async fn follow(
    mut events: UnboundedReceiver<Event>,
    log: &mut Option<RunLog>,
    account_id: &str,
    game_id: &str,
    stop: impl Future<Output = ()>,
) -> Option<ExitStatus> {
    let mut batch = Vec::with_capacity(BATCH);
    let mut exit = None;
    let mut is_stopping = false;
    tokio::pin!(stop);
    loop {
        tokio::select! {
            received = events.recv_many(&mut batch, BATCH) => {
                if received == 0 {
                    break;
                }
                crate::ps_play::run::log_lines(log, &batch, JobKind::Game, game_id).await;
                for event in batch.drain(..) {
                    if let Event::Exited(status) = event {
                        exit = status;
                    }
                }
            }
            () = &mut stop, if !is_stopping => {
                is_stopping = true;
                note(log, "# stop: decanter was asked to stop, it stopped the game", game_id).await;
                tracing::info!(game_id, "asked to stop, stopping the game");
                super::stop_session(account_id, game_id).await;
            }
        }
    }
    exit
}

/// A line of decanter's own in the run's log.
async fn note(log: &mut Option<RunLog>, line: &str, game_id: &str) {
    if let Some(file) = log.as_mut()
        && let Err(err) = file.line(line).await
    {
        tracing::warn!(game_id, %err, "the run's log file stopped taking lines");
        *log = None;
    }
}
