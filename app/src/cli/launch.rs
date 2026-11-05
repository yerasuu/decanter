//! Launches an account's installed GOG game and waits for it to exit.
//!
//! Resolved entirely from decanter's own library (`decanter_core::Library`): the
//! manifest carries the install directory and the launch executable, and
//! the Proton prefix lives wherever `Library::compatdata` says — created
//! on first launch, nothing to look up beforehand. On Windows the
//! executable runs directly, from its own folder.
//!
//! Separate from the GUI's own `crate::launch` (which tracks a session for
//! the window's views) — this is the headless path task bar entries and
//! Steam shortcuts start, so it stays in the foreground until the game has
//! ended, stops the game when it is asked to stop, and keeps the session's
//! books as the window does: cloud saves, the Galaxy service, playtime,
//! last played and achievements. Mods enabled for the game aren't mounted
//! here.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, bail};
use decanter_core::{InstalledGame, Library, Storefront};
use decanter_runtime::{Launch, Session};

use super::report_poll;

pub async fn launch(db: &mut decanter_core::Db, account_id: &str, product_id: &str) -> anyhow::Result<()> {
    let library = match crate::core::library_for_account(account_id) {
        Ok(library) => library,
        Err(err) => {
            tracing::error!(account_id, %err, "could not resolve library");
            bail!("could not resolve library: {err}");
        }
    };

    let Some(game) = library.read_manifest(account_id, Storefront::Gog, product_id) else {
        tracing::error!(account_id, product_id, "not installed for this account");
        bail!("{product_id} isn't installed for this account");
    };
    let Some(install_dir) = library.install_dir(&game.install_dir) else {
        tracing::error!(account_id, product_id, "unusable install directory");
        bail!("unusable install directory for {product_id}");
    };
    let Some(executable) = game.launch_executable.as_deref() else {
        tracing::error!(account_id, product_id, "installed but has no launch executable set");
        bail!("{product_id} is installed but has no launch executable set");
    };
    let executable = install_dir.join(executable);
    let cloud_save_checkpoint = crate::launch::sync_cloud_saves_before_launch(account_id, &game).await;
    let galaxy = galaxy_service(account_id, product_id, &install_dir).await;
    let (session, achievement_log) = if crate::launch::in_prefixes() {
        start_in_prefix(db, &library, &game, executable).await?
    } else {
        start_natively(&library, &game, executable).await?
    };
    tracing::info!(pid = session.launcher_pid(), "launched");

    let started = Instant::now();
    let ended = until_stopped(session, &session_key(account_id, product_id)).await;
    let played = started.elapsed();
    tracing::info!(seconds = played.as_secs(), "program exited");
    drop(galaxy);
    if let Err(err) = db.add_playtime(
        account_id,
        Storefront::Gog,
        product_id,
        i64::try_from(played.as_secs()).unwrap_or(i64::MAX),
    ) {
        tracing::warn!(account_id, product_id, %err, "failed to record playtime");
    }
    crate::launch::sync_cloud_saves_after_exit(account_id, &game, cloud_save_checkpoint).await;
    crate::launch::record_last_played(account_id, &game);

    tracing::debug!(account_id, product_id, "polling achievements after launch");
    report_poll(decanter_gog::poll_achievements(db, account_id, product_id).await?);

    if let Some(achievement_log) = achievement_log {
        let applied = decanter_gog::hook::process_log(db, account_id, product_id, &achievement_log).await;
        if applied > 0 {
            tracing::info!(product_id, applied, "applied locally-witnessed achievement unlocks");
        }
    }
    ended.context("running program")
}

/// The session's key, which [`decanter_runtime::stop`] finds it by.
fn session_key(account_id: &str, product_id: &str) -> String {
    format!("{account_id}/{product_id}")
}

/// Runs the session to its end. Asked to stop meanwhile, this stops the game as the window's Stop
/// does and waits for it to end, since the game outlives this command otherwise: under Proton it
/// runs on below `umu-run`, which is all a kill of this command's own process would reach.
async fn until_stopped(session: Session, key: &str) -> anyhow::Result<()> {
    let waited = session.wait();
    tokio::pin!(waited);
    tokio::select! {
        ended = &mut waited => return ended,
        () = super::stop_signal() => {}
    }
    tracing::info!(key, "asked to stop, stopping the game");
    decanter_runtime::stop(key).await;
    waited.await
}

/// Held by [`launch`] until the program exits, as the GUI's launch does.
async fn galaxy_service(account_id: &str, product_id: &str, install_dir: &Path) -> Option<decanter_gog::GalaxyLease> {
    let Some(db_path) = decanter_core::bootstrap::active_db_path() else {
        tracing::warn!(
            product_id,
            "no active database, the game starts without the local Galaxy service"
        );
        return None;
    };
    decanter_gog::serve_galaxy(&db_path, account_id, product_id, install_dir).await
}

/// Starts `game` in its prefix, with decanter-hook told where to write;
/// the session and that log.
async fn start_in_prefix(
    db: &decanter_core::Db,
    library: &Library,
    game: &InstalledGame,
    executable: PathBuf,
) -> anyhow::Result<(Session, Option<PathBuf>)> {
    let (account_id, product_id) = (game.account_id.as_str(), game.game_id.as_str());
    let Some(prefix_path) = library.compatdata(account_id, product_id) else {
        tracing::error!(account_id, product_id, "unusable account or product id for prefix");
        bail!("unusable account or product id for {product_id}'s prefix");
    };
    let prefix = decanter_runtime::Prefix::new(prefix_path);

    // `n` (native) rather than the default `b` (builtin then native):
    // makes Wine prefer whatever's actually on disk at `Galaxy64.dll`
    // over any builtin stub of the same name, which is what makes
    // decanter-hook's proxy (see `decanter_gog::hook::deploy`, run at
    // install time) the one that actually loads.
    let mut launch = Launch::new(executable, session_key(account_id, product_id))
        .game_id(format!("umu-{product_id}"))
        .store("gog");
    launch.dll_overrides.push(("Galaxy64".to_owned(), "n".to_owned()));
    // Same pin the GUI applies (`crate::launch::start`) — without it this
    // path falls back to `Proton::Latest`, which lets umu pick whatever
    // build it wants and can land on a different one than the prefix was
    // created with. Proton "upgrades" a prefix across builds in place
    // rather than refusing, corrupting it and making the game crash on
    // launch (seen firsthand: exit code 3, "Prefix has an invalid
    // version?!").
    launch.proton = crate::core::proton_for(game);
    let key = crate::taskbar::key(game.storefront, &game.game_id);
    launch.gamemoded = crate::game_tools::gamemoded(db, &key);
    if let Some(autostart_log) = crate::game_tools::mangohud(db, &key) {
        launch.env.extend(
            crate::game_tools::mangohud_env(&key, library.root(), autostart_log)
                .await
                .map(|(name, value)| (name.to_owned(), value)),
        );
    }
    launch.identity = Some((key, game.name.clone()));
    launch.env.extend(crate::cache::launch_env(game));

    let achievement_log = decanter_gog::hook::achievement_log_in_prefix(&prefix);
    launch
        .env
        .push(("DECANTER_PRODUCT_ID".to_owned(), product_id.to_owned()));
    launch.env.push((
        "DECANTER_ACHIEVEMENT_LOG".to_owned(),
        achievement_log.display().to_string(),
    ));

    launch.prefix = Some(prefix);
    let failed = crate::launch::prepare_gog_prefix(account_id, game, &launch).await.len();
    if failed > 0 {
        tracing::warn!(
            product_id,
            failed,
            "redistributables failed to install, starting the game anyway"
        );
    }
    decanter_runtime::start(launch)
        .await
        .context("launching program")
        .map(|session| (session, Some(achievement_log)))
}

/// Starts `game`'s executable from its own folder, with decanter-hook told
/// where to write; the session and that log.
async fn start_natively(
    library: &Library,
    game: &InstalledGame,
    executable: PathBuf,
) -> anyhow::Result<(Session, Option<PathBuf>)> {
    let (account_id, product_id) = (game.account_id.as_str(), game.game_id.as_str());
    let mut launch = Launch::new(executable, session_key(account_id, product_id));
    let achievement_log = decanter_gog::hook::achievement_log_path(library, account_id, product_id);
    if let Some(path) = &achievement_log {
        launch
            .env
            .push(("DECANTER_PRODUCT_ID".to_owned(), product_id.to_owned()));
        launch
            .env
            .push(("DECANTER_ACHIEVEMENT_LOG".to_owned(), path.display().to_string()));
    }
    decanter_runtime::start(launch)
        .await
        .context("launching program")
        .map(|session| (session, achievement_log))
}
