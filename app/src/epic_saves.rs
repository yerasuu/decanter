//! An installed Epic Games title's cloud saves: whether they sync at all (off until the user turns
//! them on for that game), its save folder from the catalog the library listing cached, and what
//! happens around a launch. The rules for what may move are `decanter_epic`'s (`saves`): here a
//! launch only ever lets through a transfer that replaces nothing newer, and logs the rest.

use decanter_core::{GameSystem, InstalledGame};
use decanter_epic::{Approval, FolderFault, SaveError, SaveFolder, SaveOutcome, SaveRoots, SaveSpec, SaveTarget};

use crate::i18n::Text;

/// Per game in `settings`, keyed by account and `appName`: the choice belongs to whoever owns the
/// save, and it survives an uninstall as the game's language does.
const MODE_KEY: &str = "epic_cloud_saves";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    Off,
    /// Restores from the cloud, never uploads: the way to try the feature on a real game first.
    DownloadOnly,
    Sync,
}

impl SaveMode {
    const fn stored(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::DownloadOnly => Some("download"),
            Self::Sync => Some("sync"),
        }
    }
}

fn mode_key(account_id: &str, app_name: &str) -> String {
    format!("{MODE_KEY}:{account_id}:{app_name}")
}

/// [`SaveMode::Off`] unless the user chose otherwise, and when the choice cannot be read.
pub fn mode(account_id: &str, app_name: &str) -> SaveMode {
    let stored = crate::core::db().ok().and_then(|db| {
        db.setting(&mode_key(account_id, app_name))
            .inspect_err(|err| tracing::warn!(account_id, app_name, %err, "epic cloud saves mode unreadable, off"))
            .ok()
            .flatten()
    });
    match stored.as_deref() {
        Some("download") => SaveMode::DownloadOnly,
        Some("sync") => SaveMode::Sync,
        Some(other) => {
            tracing::warn!(
                account_id,
                app_name,
                stored = other,
                "unknown epic cloud saves mode, off"
            );
            SaveMode::Off
        }
        None => SaveMode::Off,
    }
}

pub fn set_mode(account_id: &str, app_name: &str, mode: SaveMode) -> Result<(), Text> {
    let db = crate::core::db().map_err(|err| err.localized())?;
    let key = mode_key(account_id, app_name);
    match mode.stored() {
        Some(value) => db.set_setting(&key, value),
        None => db.delete_setting(&key),
    }
    .map_err(Text::verbatim)?;
    tracing::info!(account_id, app_name, ?mode, "epic cloud saves mode changed");
    Ok(())
}

/// The game's save folder: the cached catalog's template, resolved inside its prefix under
/// Proton, else against the Windows user's folders.
pub async fn folder(account_id: &str, game: &InstalledGame) -> Result<SaveFolder, Text> {
    let app_name = game.game_id.as_str();
    let cached = crate::views::library::epic_install::cached_game(account_id, app_name);
    let Some(template) = cached.as_ref().and_then(|cached| cached.cloud_save_folder.as_deref()) else {
        tracing::info!(account_id, app_name, "no cached epic cloud save folder yet");
        return Err(Text::new("epic_saves.errors.not_cached"));
    };
    let spec = SaveSpec::new(
        template,
        cached
            .as_ref()
            .and_then(|cached| cached.cloud_include_list.as_deref())
            .unwrap_or_default(),
        cached
            .as_ref()
            .and_then(|cached| cached.cloud_exclude_list.as_deref())
            .unwrap_or_default(),
    );
    let library = crate::core::library_for_account(account_id).map_err(|err| err.localized())?;
    let install_dir = library
        .install_dir(&game.install_dir)
        .ok_or_else(|| Text::new("messages.errors.install_dir"))?;
    let roots = if crate::launch::in_prefixes() {
        let prefix =
            crate::core::prefix_for(account_id, app_name).ok_or_else(|| Text::new("messages.errors.prefix"))?;
        SaveRoots::in_prefix(&prefix.drive_c(), &install_dir, account_id)
    } else if decanter_core::platform::game_system() == GameSystem::Windows {
        let (Some(profile), Some(documents), Some(local), Some(roaming)) = (
            dirs::home_dir(),
            dirs::document_dir(),
            dirs::data_local_dir(),
            dirs::data_dir(),
        ) else {
            return Err(Text::new("messages.errors.user_folders"));
        };
        SaveRoots::native(&profile, &documents, &local, &roaming, &install_dir, account_id)
    } else {
        return Err(Text::new("epic_saves.errors.unsupported"));
    };
    decanter_epic::save_folder(&spec, &roots).await.map_err(|fault| {
        tracing::warn!(account_id, app_name, %fault, "epic cloud save folder refused");
        fault_text(&fault)
    })
}

fn fault_text(fault: &FolderFault) -> Text {
    match fault {
        FolderFault::NotDeclared => Text::new("epic_saves.errors.not_declared"),
        other => Text::new("epic_saves.errors.folder").arg("reason", other),
    }
}

pub fn error_text(err: &SaveError) -> Text {
    match err {
        SaveError::Folder(fault) => fault_text(fault),
        SaveError::SessionClosed => Text::new("epic.session.closed"),
        SaveError::Unreachable(_) => Text::new("epic_saves.errors.unreachable").arg("reason", err),
        SaveError::UnexpectedShape(_) | SaveError::Incomplete { .. } => {
            Text::new("epic_saves.errors.unexpected").arg("reason", err)
        }
        SaveError::Changed => Text::new("epic_saves.errors.changed"),
        SaveError::NeedsConfirmation { .. } => Text::new("epic_saves.errors.needs_decision"),
        _ => Text::verbatim(err),
    }
}

/// Records `outcome`'s sync instant, when it has one.
pub fn record(account_id: &str, app_name: &str, outcome: &SaveOutcome) -> Option<i64> {
    let last_sync = outcome.last_sync?;
    if let Err(err) = crate::core::record_cloud_save_sync(account_id, app_name, last_sync) {
        tracing::warn!(account_id, app_name, error = %err, "could not record the epic cloud saves sync");
    }
    Some(last_sync)
}

/// Restores the cloud's snapshot before a launch when that replaces nothing newer, for a game
/// whose saves the user turned on. The checkpoint the session's exit is judged against comes back,
/// as GOG's sync does it.
pub async fn before_launch(account_id: &str, game: &InstalledGame) -> Option<i64> {
    let app_name = game.game_id.as_str();
    if mode(account_id, app_name) == SaveMode::Off {
        return game.cloud_save_last_sync;
    }
    let folder = match folder(account_id, game).await {
        Ok(folder) => folder,
        Err(reason) => {
            tracing::info!(account_id, app_name, %reason, "epic cloud saves skipped before launch");
            return game.cloud_save_last_sync;
        }
    };
    let Ok(target) = SaveTarget::new(account_id, app_name, &folder) else {
        return game.cloud_save_last_sync;
    };
    match decanter_epic::download_saves(target, game.cloud_save_last_sync, Approval::Automatic).await {
        Ok(outcome) => {
            tracing::info!(account_id, app_name, verdict = ?outcome.verdict, files = outcome.files, "epic cloud saves before launch");
            record(account_id, app_name, &outcome).or(game.cloud_save_last_sync)
        }
        Err(err) => {
            tracing::warn!(account_id, app_name, error = %err, retryable = err.is_retryable(), "epic cloud saves not restored before launch, playing with the local copy");
            game.cloud_save_last_sync
        }
    }
}

/// Uploads the session's saves once it ends, in [`SaveMode::Sync`] only and only when the cloud did
/// not change meanwhile.
pub async fn after_exit(account_id: &str, game: &InstalledGame, checkpoint: Option<i64>) {
    let app_name = game.game_id.as_str();
    if mode(account_id, app_name) != SaveMode::Sync {
        return;
    }
    let folder = match folder(account_id, game).await {
        Ok(folder) => folder,
        Err(reason) => {
            tracing::info!(account_id, app_name, %reason, "epic cloud saves skipped after exit");
            return;
        }
    };
    let Ok(target) = SaveTarget::new(account_id, app_name, &folder) else {
        return;
    };
    match decanter_epic::upload_saves(target, checkpoint, Approval::Automatic).await {
        Ok(outcome) => {
            tracing::info!(account_id, app_name, verdict = ?outcome.verdict, files = outcome.files, "epic cloud saves after exit");
            record(account_id, app_name, &outcome);
        }
        Err(err) => {
            tracing::warn!(account_id, app_name, error = %err, retryable = err.is_retryable(), "epic cloud saves not uploaded after exit, left for the properties window");
        }
    }
}
