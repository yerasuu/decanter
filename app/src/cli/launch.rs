//! Launches an account's installed GOG game and waits for it to exit.
//!
//! Resolved entirely from decanter's own library (`decanter_core::Library`): the
//! manifest carries the install directory and the launch executable, and
//! the Proton prefix lives wherever `Library::compatdata` says — created
//! on first launch, nothing to look up beforehand.
//!
//! Separate from the GUI's own `crate::launch` (which tracks a session for
//! the tray/exit-on-signal machinery) — this is the headless CLI path,
//! used for scripting and for observing a launch independent of the app.

use anyhow::{Context, bail};
use decanter_core::Storefront;
use decanter_runtime::Launch;

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
    let mut launch = Launch::new(install_dir.join(executable), format!("umu-{product_id}")).store("gog");
    launch.dll_overrides.push(("Galaxy64".to_owned(), "n".to_owned()));
    // Same pin the GUI applies (`crate::launch::start`) — without it this
    // path falls back to `Proton::Latest`, which lets umu pick whatever
    // build it wants and can land on a different one than the prefix was
    // created with. Proton "upgrades" a prefix across builds in place
    // rather than refusing, corrupting it and making the game crash on
    // launch (seen firsthand: exit code 3, "Prefix has an invalid
    // version?!").
    launch.proton = crate::core::proton_for(&game);
    launch.identity = Some((crate::taskbar::key(game.storefront, &game.game_id), game.name.clone()));

    let achievement_log = decanter_gog::hook::achievement_log_path(&prefix);
    launch
        .env
        .push(("DECANTER_PRODUCT_ID".to_owned(), product_id.to_owned()));
    launch.env.push((
        "DECANTER_ACHIEVEMENT_LOG".to_owned(),
        achievement_log.display().to_string(),
    ));

    let session = decanter_runtime::start(&prefix, launch)
        .await
        .context("launching program")?;
    tracing::info!(pid = session.launcher_pid(), "launched");

    session.wait().await.context("running program")?;
    tracing::info!("program exited");

    tracing::debug!(account_id, product_id, "polling achievements after launch");
    report_poll(decanter_gog::poll_achievements(db, account_id, product_id).await?);

    let applied = decanter_gog::hook::process_log(db, account_id, product_id, &achievement_log).await;
    if applied > 0 {
        tracing::info!(product_id, applied, "applied locally-witnessed achievement unlocks");
    }
    Ok(())
}
