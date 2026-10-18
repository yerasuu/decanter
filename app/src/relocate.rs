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
use rust_i18n::t;

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
            .ok_or_else(|| t!("messages.errors.not_installed").into_owned())
    );
    if from == to {
        return Ok(());
    }
    let game = crate::achv_try!(
        [Storefront::Gog, Storefront::Unspecified]
            .into_iter()
            .find_map(|storefront| library.read_manifest(account_id, storefront, game_id))
            .ok_or_else(|| t!("messages.errors.not_installed").into_owned())
    );
    if game.prefix_of.is_some() {
        return Err(t!("storage.errors.hosted").into_owned());
    }
    let old_prefix = from.join("compatdata").join(account_id).join(game_id);
    #[cfg(proton)]
    let running = !decanter_runtime::Prefix::new(&old_prefix).processes().is_empty();
    #[cfg(not(proton))]
    let running = crate::launch::is_running(account_id, game_id);
    if running {
        return Err(t!("storage.errors.running").into_owned());
    }

    // Storefront files under `common/`, which another account in the same
    // folder may share — moving them would pull them out from under it.
    let files = (game.storefront != Storefront::Unspecified)
        .then(|| library.install_dir(&game.install_dir))
        .flatten()
        .filter(|dir| dir.starts_with(&from) && dir.is_dir());
    if files.is_some() && library.install_dir_is_shared(&game.install_dir, account_id, game_id) {
        return Err(t!("storage.errors.shared").into_owned());
    }
    let new_files = files.as_ref().map(|_| to.join("common").join(&game.install_dir));
    let new_prefix = to.join("compatdata").join(account_id).join(game_id);
    for target in new_files.iter().chain(std::iter::once(&new_prefix)) {
        if target.exists() && std::fs::read_dir(target).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(t!("storage.errors.exists", path = target.display()).into_owned());
        }
        if let Some(parent) = target.parent() {
            crate::achv_try!(
                std::fs::create_dir_all(parent).map_err(|err| t!(
                    "messages.errors.create_dir",
                    path = parent.display(),
                    error = err
                )
                .into_owned())
            );
        }
    }
    tracing::info!(account_id, game_id, from = %from.display(), to = %to.display(), "moving game to another library folder");

    if let (Some(files), Some(new_files)) = (&files, &new_files) {
        crate::achv_try!(
            transfer::bring(files, new_files, Mode::Move, |done| progress.record(done)).inspect_err(
                |err| tracing::error!(account_id, game_id, from = %files.display(), %err, "could not move the game's files")
            )
        );
    }
    if old_prefix.is_dir()
        && let Err(err) = transfer::bring(&old_prefix, &new_prefix, Mode::Move, |done| progress.record(done))
    {
        tracing::error!(account_id, game_id, from = %old_prefix.display(), %err, "could not move the game's prefix");
        // Half a game in each folder runs from neither: the files go back.
        if let (Some(files), Some(new_files)) = (&files, &new_files)
            && let Err(undo) = transfer::bring(new_files, files, Mode::Move, |_| {})
        {
            tracing::error!(account_id, game_id, %undo, "could not move the files back after the prefix failed to move");
        }
        return Err(err.localized());
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
                .inspect_err(
                    |err| tracing::error!(account_id, game_id = record.game_id, %err, "could not relocate the manifest")
                )
                .map_err(|err| t!("storage.errors.manifest", game = record.name, error = err).into_owned())
        );
    }
    tracing::info!(account_id, game_id, to = %to.display(), "game moved to another library folder");
    Ok(())
}

/// Moves the PS dump in `dir` into `library_root`'s games folder, keeping
/// its folder name. The caller makes sure the game isn't running. Blocking:
/// on another disk every byte is copied, and the original is only removed
/// once the copy is whole.
pub fn move_dump(dir: &Path, library_root: &Path, progress: &Progress) -> Result<(), String> {
    let games = decanter_ps::paths::games_in(library_root);
    // A dump dropped straight into a games folder is that folder itself:
    // moving it would take every other dump with it.
    let Some(name) = dir
        .file_name()
        .filter(|_| dir.parent().is_some_and(|parent| parent.ends_with("psport/games")))
    else {
        return Err(t!("storage.errors.dump_root", path = dir.display()).into_owned());
    };
    if dir.parent() == Some(games.as_path()) {
        return Ok(());
    }
    let target = games.join(name);
    if target.exists() && std::fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(t!("storage.errors.exists", path = target.display()).into_owned());
    }
    crate::achv_try!(
        std::fs::create_dir_all(&games).map_err(|err| t!(
            "messages.errors.create_dir",
            path = games.display(),
            error = err
        )
        .into_owned())
    );
    tracing::info!(from = %dir.display(), to = %target.display(), "moving PS dump to another library folder");
    crate::achv_try!(
        transfer::bring(dir, &target, Mode::Move, |done| progress.record(done))
            .inspect_err(|err| tracing::error!(dir = %dir.display(), %err, "could not move the PS dump"))
    );
    crate::dump_size::moved(dir, &target);
    tracing::info!(to = %target.display(), "PS dump moved to another library folder");
    Ok(())
}

/// `exe` with `from` swapped for `to` when it's under `from`.
fn remap(exe: &str, from: &Path, to: &Path) -> String {
    Path::new(exe).strip_prefix(from).map_or_else(
        |_| exe.to_owned(),
        |relative| to.join(relative).to_string_lossy().into_owned(),
    )
}
