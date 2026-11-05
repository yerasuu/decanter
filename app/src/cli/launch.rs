//! Launches an account's installed GOG game and waits for it to exit.
//!
//! Resolved from decanter's own library (`decanter_core::Library`), then played through the same
//! launch as the window's Play ([`crate::launch::play_headless`]): the mods' play tree, the game's
//! own tools and environment, its prefix, Proton build and redistributables, the Galaxy service,
//! cloud saves, the run's log, last played and achievements. This is the path task bar entries and
//! Steam shortcuts start, so it stays in the foreground until the game has ended, stops the game
//! when it is asked to stop, and records the session's playtime once it has ended.

use anyhow::bail;
use decanter_core::Storefront;

pub async fn launch(db: &decanter_core::Db, account_id: &str, product_id: &str) -> anyhow::Result<()> {
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
    if game.launch_executable.is_none() {
        tracing::error!(account_id, product_id, "installed but has no launch executable set");
        bail!("{product_id} is installed but has no launch executable set");
    }
    crate::launch::play_headless(db, account_id, &game, super::stop_signal()).await
}
