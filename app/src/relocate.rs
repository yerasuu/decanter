//! Moving an installed game from one library folder to another — Steam's
//! "Move install folder" — files, prefix and manifest together, since a
//! game lives whole in one folder (see `decanter_core::Library`).
//!
//! Files and prefix go through the same transfer an imported prefix does:
//! a rename on one disk, otherwise an rsync into a staging directory that
//! only takes the real name once whole. The manifest moves last, so until
//! everything has landed the game is still recorded where it was.

use std::path::Path;

use decanter_core::{InstalledGame, Storefront};

use crate::external::transfer::{self, Mode, Progress};

/// Moves `game_id` (of `account_id`, which for an external game is its
/// profile) into the library folder `to`, along with any game a launcher
/// in its prefix installed. Blocking: on another disk every byte is
/// copied.
pub fn move_game(account_id: &str, game_id: &str, to: &Path, progress: &Progress) -> Result<(), String> {
    let library = crate::achv_try!(crate::core::library_for_account(account_id));
    let from = crate::achv_try!(
        library
            .root_of(account_id, game_id)
            .ok_or("el juego ya no figura como instalado")
    );
    if from == to {
        return Ok(());
    }
    let game = crate::achv_try!(
        [Storefront::Gog, Storefront::Unspecified]
            .into_iter()
            .find_map(|storefront| library.read_manifest(account_id, storefront, game_id))
            .ok_or("el juego ya no figura como instalado")
    );
    if game.prefix_of.is_some() {
        return Err("Este juego vive en el prefijo de su launcher: mové el launcher y viene con él.".to_owned());
    }
    let old_prefix = from.join("compatdata").join(account_id).join(game_id);
    if !decanter_runtime::Prefix::new(&old_prefix).processes().is_empty() {
        return Err("El juego está abierto: cerralo antes de moverlo.".to_owned());
    }

    // Storefront files under `common/`, which another account in the same
    // folder may share — moving them would pull them out from under it.
    let files = (game.storefront != Storefront::Unspecified)
        .then(|| library.install_dir(&game.install_dir))
        .flatten()
        .filter(|dir| dir.starts_with(&from) && dir.is_dir());
    if files.is_some() && library.install_dir_is_shared(&game.install_dir, account_id, game_id) {
        return Err("Otra cuenta comparte estos archivos: no se pueden mover solos.".to_owned());
    }
    let new_files = files.as_ref().map(|_| to.join("common").join(&game.install_dir));
    let new_prefix = to.join("compatdata").join(account_id).join(game_id);
    for target in new_files.iter().chain(std::iter::once(&new_prefix)) {
        if target.exists() && std::fs::read_dir(target).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(format!("{} ya existe en la carpeta de destino.", target.display()));
        }
        if let Some(parent) = target.parent() {
            crate::achv_try!(
                std::fs::create_dir_all(parent).map_err(|err| format!("creando {}: {err}", parent.display()))
            );
        }
    }
    tracing::info!(account_id, game_id, from = %from.display(), to = %to.display(), "moving game to another library folder");

    if let (Some(files), Some(new_files)) = (&files, &new_files) {
        crate::achv_try!(transfer::bring(files, new_files, Mode::Move, |done| progress.record(done)));
    }
    if old_prefix.is_dir()
        && let Err(err) = transfer::bring(&old_prefix, &new_prefix, Mode::Move, |done| progress.record(done))
    {
        // Half a game in each folder runs from neither: the files go back.
        if let (Some(files), Some(new_files)) = (&files, &new_files)
            && let Err(undo) = transfer::bring(new_files, files, Mode::Move, |_| {})
        {
            tracing::error!(%undo, "could not move the files back after the prefix failed to move");
        }
        return Err(err);
    }

    // External games, and the games their launchers installed, record an
    // absolute executable path; one inside the prefix moved with it.
    let hosted: Vec<InstalledGame> = library
        .scan(account_id)
        .into_values()
        .filter(|other| other.prefix_of.as_deref() == Some(game_id))
        .collect();
    for record in std::iter::once(game).chain(hosted) {
        let record = InstalledGame {
            launch_executable: record
                .launch_executable
                .as_deref()
                .map(|exe| remap(exe, &old_prefix, &new_prefix)),
            ..record
        };
        crate::achv_try!(
            library
                .relocate_manifest(&record, to)
                .map_err(|err| format!("moviendo el registro de {}: {err}", record.name))
        );
    }
    tracing::info!(account_id, game_id, to = %to.display(), "game moved to another library folder");
    Ok(())
}

/// `exe` with `from` swapped for `to` when it's under `from`.
fn remap(exe: &str, from: &Path, to: &Path) -> String {
    Path::new(exe).strip_prefix(from).map_or_else(
        |_| exe.to_owned(),
        |relative| to.join(relative).to_string_lossy().into_owned(),
    )
}
