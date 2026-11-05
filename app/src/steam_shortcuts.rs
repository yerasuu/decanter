//! At startup: the Steam shortcuts decanter already wrote that start no decanter binary, pointed at
//! the command this decanter writes ([`crate::launcher`]). That is a GOG shortcut from before
//! shortcuts went through the CLI, which named the game's Windows executable for Steam to run with
//! no prefix, or one an earlier build wrote with a development run's dynamic loader as its program.
//! An entry that starts some decanter binary is left as it is, even another one than this, since
//! which decanter it starts is the user's to change by exporting again; except one that starts this
//! very `AppImage` with other options, which gets this decanter's options (an entry written before
//! the `AppImage` was told to extract and run fails to start from Steam). Only decanter's own entries
//! are touched, a GOG one only when its appid is recorded, so a game never exported stays out of
//! Steam; and nothing is touched when this decanter has no command to write.

use std::path::Path;

use decanter_core::steam;
use decanter_core::{Db, Storefront};
use decanter_ps::title_id::TitleId;

/// Best-effort like the exports: what can't be read or written is logged and left as it is.
pub async fn retarget() {
    let Ok(db) = crate::core::db() else {
        return;
    };
    // Writes into the real Steam user's own config files, which the Ajustes toggle turns off.
    if !db.steam_export_enabled().unwrap_or(true) {
        return;
    }
    let Some(steam_root) = steam::steam_root() else {
        return;
    };
    let Some(user_id) = steam::local_user_id(&steam_root) else {
        return;
    };
    match tokio::task::spawn_blocking(move || retarget_entries(&db, &steam_root, &user_id)).await {
        Ok(Ok(0)) => {}
        Ok(Ok(count)) => tracing::info!(count, "steam shortcuts pointed at decanter's command again"),
        Ok(Err(err)) => tracing::warn!(%err, "could not read steam shortcuts to point them at decanter's command"),
        Err(err) => tracing::warn!(%err, "steam shortcut retarget worker task panicked"),
    }
}

/// How many entries were rewritten.
fn retarget_entries(db: &Db, steam_root: &Path, user_id: &str) -> std::io::Result<usize> {
    let mut count = 0;
    for own in steam::own_entries(steam_root, user_id)? {
        let Some((command, gog)) = command_for(db, &own) else {
            continue;
        };
        if crate::launcher::names_decanter(&own.exe) && !is_outdated(&own, &command) {
            continue;
        }
        let appid = match steam::retarget(steam_root, user_id, own.appid, &command) {
            Ok(Some(appid)) => appid,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(appid = own.appid, %err, "could not point a steam shortcut at decanter's command");
                continue;
            }
        };
        count += 1;
        tracing::info!(
            from = own.appid,
            to = appid,
            "steam shortcut now starts decanter's command"
        );
        if let Some((account_id, product_id)) = gog
            && let Err(err) = db.set_steam_shortcut_appid(&account_id, Storefront::Gog, &product_id, appid)
        {
            tracing::warn!(account_id, product_id, appid, %err, "could not record the steam shortcut's new appid");
        }
    }
    Ok(count)
}

/// Whether `own` starts this very `AppImage` with other options than it would now write, such as an
/// entry from before shortcuts told the `AppImage` to extract and run, which Steam can't start.
fn is_outdated(own: &steam::Own, command: &steam::Command) -> bool {
    Path::new(own.exe.trim_matches('"')) == command.exe
        && crate::launcher::starts_appimage(command)
        && own.launch_options != command.launch_options
}

/// The command `own` should start, and the GOG game it belongs to when it is one.
fn command_for(db: &Db, own: &steam::Own) -> Option<(steam::Command, Option<(String, String)>)> {
    if let Some(title_id) = crate::ps_steam::title_of(&own.launch_options) {
        let title_id = title_id.parse::<TitleId>().ok()?;
        let command = crate::launcher::Launcher::ps(title_id.as_str())?.steam()?;
        return Some((command, None));
    }
    let (account_id, product_id) = db
        .steam_shortcut_game(Storefront::Gog, own.appid)
        .inspect_err(|err| tracing::warn!(appid = own.appid, %err, "could not read the steam shortcut's game"))
        .ok()
        .flatten()?;
    let command = crate::launcher::Launcher::gog(&account_id, &product_id)?.steam()?;
    Some((command, Some((account_id, product_id))))
}
