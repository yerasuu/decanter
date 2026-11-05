//! At startup: the Steam shortcuts decanter already wrote that start no decanter binary, pointed at
//! the command this decanter writes ([`crate::launcher`]). That is a GOG shortcut from before
//! shortcuts went through the CLI, which named the game's Windows executable for Steam to run with
//! no prefix, or one an earlier build wrote with a development run's dynamic loader as its program.
//! An entry that starts some decanter binary is left as it is, even another one than this, since
//! which decanter it starts is the user's to change by exporting again; except one that starts this
//! very `AppImage` with other options, which gets this decanter's options (an entry written before
//! the `AppImage` was told to extract and run fails to start from Steam). Only decanter's own entries
//! are touched, a GOG one only when its appid is recorded, so a game never exported stays out of
//! Steam; and nothing is touched when this decanter has no command to write. Then the art of those
//! same entries is brought to the names Steam reads.

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
    if !db.steam_automatic().is_ok_and(|automatic| automatic.export_shortcuts) {
        return;
    }
    let Some(steam_root) = steam::steam_root() else {
        return;
    };
    let Some(user_id) = steam::local_user_id(&steam_root) else {
        return;
    };
    let worked = tokio::task::spawn_blocking(move || {
        match retarget_entries(&db, &steam_root, &user_id) {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "steam shortcuts pointed at decanter's command again"),
            Err(err) => tracing::warn!(%err, "could not read steam shortcuts to point them at decanter's command"),
        }
        if let Err(err) = refresh_entries(&steam_root, &user_id) {
            tracing::warn!(%err, "could not read steam shortcuts to bring their art up to date");
        }
    })
    .await;
    if let Err(err) = worked {
        tracing::warn!(%err, "steam shortcut retarget worker task panicked");
    }
}

/// The art of every entry of decanter's under the names Steam's library reads, since earlier builds
/// named it by the 64-bit id the current library never asks for; and a PS game's capsules, which
/// earlier builds left out, drawn from the icon its entry already has.
fn refresh_entries(steam_root: &Path, user_id: &str) -> std::io::Result<()> {
    for own in steam::own_entries(steam_root, user_id)? {
        let art = match steam::refresh_art(steam_root, user_id, own.appid, &steam::Artwork::default()) {
            Ok(Some(art)) => art,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(appid = own.appid, %err, "could not bring a steam shortcut's art up to date");
                continue;
            }
        };
        let Some(icon) = art
            .icon
            .filter(|_| art.portrait.is_none() && crate::ps_steam::title_of(&own.launch_options).is_some())
        else {
            continue;
        };
        let crate::ps_steam::Capsules { dir, portrait, wide } = crate::ps_steam::Capsules::draw(&icon);
        let artwork = steam::Artwork {
            capsule: wide,
            portrait,
            ..steam::Artwork::default()
        };
        let drawn = steam::refresh_art(steam_root, user_id, own.appid, &artwork);
        crate::ps_steam::Capsules::clean(dir);
        match drawn {
            Ok(_) => tracing::info!(appid = own.appid, "PS game steam shortcut got its capsules"),
            Err(err) => tracing::warn!(appid = own.appid, %err, "could not write a PS game steam shortcut's capsules"),
        }
    }
    Ok(())
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
