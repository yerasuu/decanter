//! Moving an installed game from one library folder to another — Steam's
//! "Move install folder" — files, prefix and manifest together, since a
//! game lives whole in one folder (see `decanter_core::Library`).
//!
//! Files and prefix go through the same transfer an imported prefix does:
//! a rename on one disk, otherwise an rsync into a staging directory that
//! only takes the real name once whole. The manifest moves last, so until
//! everything has landed the game is still recorded where it was.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use decanter_core::{InstalledGame, Storefront};
use decanter_ps::library::{Game, Refusal};

use crate::external::transfer::{self, Mode, Progress};
use crate::i18n::Text;

/// Moves `game_id` (of `account_id`, which for an external game is its
/// profile) into the library folder `to`, along with any game a launcher
/// in its prefix installed. Blocking: on another disk every byte is
/// copied.
pub fn move_game(account_id: &str, game_id: &str, to: &Path, progress: &Progress) -> Result<(), Text> {
    let library = crate::achv_try!(crate::core::library_for_account(account_id));
    let from = crate::achv_try!(
        library
            .root_of(account_id, game_id)
            .ok_or_else(|| Text::new("messages.errors.not_installed"))
    );
    if from == to {
        return Ok(());
    }
    let game = crate::achv_try!(
        [Storefront::Gog, Storefront::Unspecified]
            .into_iter()
            .find_map(|storefront| library.read_manifest(account_id, storefront, game_id))
            .ok_or_else(|| Text::new("messages.errors.not_installed"))
    );
    if game.prefix_of.is_some() {
        return Err(Text::new("storage.errors.hosted"));
    }
    let old_prefix = from.join("compatdata").join(account_id).join(game_id);
    if crate::launch::is_running(account_id, game_id) {
        return Err(Text::new("storage.errors.running"));
    }

    // Storefront files under `common/`, which another account in the same
    // folder may share — moving them would pull them out from under it.
    let files = (game.storefront != Storefront::Unspecified)
        .then(|| library.install_dir(&game.install_dir))
        .flatten()
        .filter(|dir| dir.starts_with(&from) && dir.is_dir());
    if files.is_some() && library.install_dir_is_shared(&game.install_dir, account_id, game_id) {
        return Err(Text::new("storage.errors.shared"));
    }
    let new_files = files.as_ref().map(|_| to.join("common").join(&game.install_dir));
    let new_prefix = to.join("compatdata").join(account_id).join(game_id);
    for target in new_files.iter().chain(std::iter::once(&new_prefix)) {
        if target.exists() && std::fs::read_dir(target).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(Text::new("storage.errors.exists").arg("path", target.display()));
        }
        if let Some(parent) = target.parent() {
            crate::achv_try!(std::fs::create_dir_all(parent).map_err(|err| {
                Text::new("messages.errors.create_dir")
                    .arg("path", parent.display())
                    .arg("error", err)
            }));
        }
    }
    tracing::info!(account_id, game_id, from = %from.display(), to = %to.display(), "moving game to another library folder");

    if let (Some(files), Some(new_files)) = (&files, &new_files) {
        crate::achv_try!(
            transfer::bring(files, new_files, Mode::Move, progress.stop_flag(), |done| progress.record(done)).inspect_err(
                |err| tracing::error!(account_id, game_id, from = %files.display(), %err, "could not move the game's files")
            )
        );
    }
    if old_prefix.is_dir()
        && let Err(err) = transfer::bring(&old_prefix, &new_prefix, Mode::Move, progress.stop_flag(), |done| {
            progress.record(done);
        })
    {
        tracing::error!(account_id, game_id, from = %old_prefix.display(), %err, "could not move the game's prefix");
        // Half a game in each folder runs from neither: the files go back.
        if let (Some(files), Some(new_files)) = (&files, &new_files)
            && let Err(undo) = transfer::bring(new_files, files, Mode::Move, &AtomicBool::new(false), |_| {})
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
                .map_err(|err| Text::new("storage.errors.manifest")
                    .arg("game", &record.name)
                    .arg("error", err))
        );
    }
    tracing::info!(account_id, game_id, to = %to.display(), "game moved to another library folder");
    Ok(())
}

/// Moves the PS dump in `dir` into `library_root`'s games folder, keeping
/// its folder name. The caller makes sure the game isn't running. Blocking:
/// on another disk every byte is copied, and the original is only removed
/// once the copy is whole.
pub fn move_dump(dir: &Path, library_root: &Path, progress: &Progress) -> Result<(), Text> {
    let games = decanter_ps::paths::games_in(library_root);
    // A dump lying straight in a games folder is that folder itself: it
    // gets its own folder first, so the other dumps stay behind.
    let wrapped = if dir.ends_with("psport/games") {
        crate::achv_try!(wrap_root_dump(dir)).map(|(_, wrapped)| wrapped)
    } else {
        None
    };
    let dir = wrapped.as_deref().unwrap_or(dir);
    let Some(name) = dir
        .file_name()
        .filter(|_| dir.parent().is_some_and(|parent| parent.ends_with("psport/games")))
    else {
        return Err(Text::new("messages.errors.not_installed"));
    };
    if dir.parent() == Some(games.as_path()) {
        return Ok(());
    }
    let target = games.join(name);
    if target.exists() && std::fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(Text::new("storage.errors.exists").arg("path", target.display()));
    }
    crate::achv_try!(std::fs::create_dir_all(&games).map_err(|err| {
        Text::new("messages.errors.create_dir")
            .arg("path", games.display())
            .arg("error", err)
    }));
    tracing::info!(from = %dir.display(), to = %target.display(), "moving PS dump to another library folder");
    crate::achv_try!(
        transfer::bring(dir, &target, Mode::Move, progress.stop_flag(), |done| progress
            .record(done))
        .inspect_err(|err| tracing::error!(dir = %dir.display(), %err, "could not move the PS dump"))
    );
    crate::dump_size::moved(dir, &target);
    tracing::info!(to = %target.display(), "PS dump moved to another library folder");
    Ok(())
}

/// Where the dump `game` read from `source` lands in `games`, the primary
/// games folder: under the picked folder's own name when it is a plain one,
/// else under its title id, never under anything else its metadata says.
/// Refused when `source` is already in one of `games_folders` (the
/// profile's, `games` among them), holds one, or the name is taken.
pub fn dump_target(source: &Path, game: &Game, games: &Path, games_folders: &[PathBuf]) -> Result<PathBuf, Text> {
    // Compared as the filesystem resolves them, so a link to the games
    // folder can't pass for a folder outside it.
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let real_source = real(source);
    for folder in games_folders.iter().map(PathBuf::as_path).chain(std::iter::once(games)) {
        let folder = real(folder);
        if real_source.starts_with(&folder) {
            return Err(Text::new("ps.add.refused.in_games").arg("path", source.display()));
        }
        if folder.starts_with(&real_source) {
            return Err(Text::new("ps.add.refused.holds_games").arg("path", source.display()));
        }
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        // A leading dot would hide it, and is how staging folders are named.
        .filter(|name| !name.starts_with('.'))
        .unwrap_or(&game.title_id);
    let target = games.join(name);
    if target.exists() && std::fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(Text::new("storage.errors.exists").arg("path", target.display()));
    }
    Ok(target)
}

/// Why [`decanter_ps::library::inspect`] turned the folder at `path` down.
pub fn dump_refusal(path: &Path, refusal: &Refusal) -> Text {
    match refusal {
        Refusal::NotFolder => Text::new("ps.add.refused.not_folder"),
        Refusal::NotDump => Text::new("ps.add.refused.not_dump"),
        Refusal::TitleId(title_id) => Text::new("ps.add.refused.title_id").arg("title_id", title_id),
        Refusal::NoEboot => Text::new("ps.add.refused.no_eboot"),
    }
    .arg("path", path.display())
}

/// Brings the dump in `source` to `target` (from [`dump_target`]). Blocking:
/// a copy, or a move to another disk, copies every byte. A copy that fails
/// or is cancelled leaves nothing at `target`, and `source` is only removed
/// by a move once the copy is whole.
pub fn add_dump(source: &Path, target: &Path, mode: Mode, progress: &Progress) -> Result<(), Text> {
    if let Some(games) = target.parent() {
        crate::achv_try!(std::fs::create_dir_all(games).map_err(|err| {
            Text::new("messages.errors.create_dir")
                .arg("path", games.display())
                .arg("error", err)
        }));
    }
    tracing::info!(from = %source.display(), to = %target.display(), moved = mode == Mode::Move, "adding PS dump");
    if let Err(err) = transfer::bring(source, target, mode, progress.stop_flag(), |done| progress.record(done)) {
        if !matches!(err, transfer::Error::Cancelled) {
            tracing::error!(from = %source.display(), %err, "could not add the PS dump");
        }
        // Unlike a prefix import, nothing retries into a dump's staging: a
        // half copy would only sit in the games folder.
        transfer::discard_staging(target);
        return Err(err.localized());
    }
    if mode == Mode::Move {
        crate::dump_size::moved(source, target);
    }
    tracing::info!(to = %target.display(), "PS dump added");
    Ok(())
}

/// Puts a dump lying straight in the games folder `games` into a folder of
/// its own, `games/<TITLE_ID>` (`<TITLE_ID>-2`... when taken): a games
/// folder that is itself a dump can't move without every other dump in it.
///
/// Only renames, on one disk. The dump's entries gather in
/// `.<name>.wrapping`, `sce_sys` last so that the root reads as the dump
/// until everything else is across, and the next call finishes one a crash
/// cut short. Other dumps in `games` and staging folders stay. Blocking; the
/// caller makes sure the game isn't running.
pub fn wrap_root_dump(games: &Path) -> Result<Option<(Game, PathBuf)>, Text> {
    let failed = |path: &Path, err: std::io::Error| {
        tracing::error!(games = %games.display(), path = %path.display(), %err, "could not put a PS dump in its own folder");
        Text::new("ps.games.wrap_failed")
            .arg("path", games.display())
            .arg("error", err)
    };
    let root_dump = decanter_ps::library::read_game(games);
    let (staging, name) = match (unfinished_wrap(games), &root_dump) {
        (Some(unfinished), _) => unfinished,
        (None, None) => return Ok(None),
        (None, Some(game)) => {
            let Some(name) = std::iter::once(game.title_id.clone())
                .chain((2..100).map(|suffix| format!("{}-{suffix}", game.title_id)))
                .find(|name| !games.join(name).exists())
            else {
                return Err(failed(
                    &games.join(&game.title_id),
                    std::io::ErrorKind::AlreadyExists.into(),
                ));
            };
            (games.join(format!(".{name}.wrapping")), name)
        }
    };
    if root_dump.is_some() {
        crate::achv_try!(std::fs::create_dir_all(&staging).map_err(|err| failed(&staging, err)));
        let mut entries: Vec<_> = crate::achv_try!(std::fs::read_dir(games).map_err(|err| failed(games, err)))
            .flatten()
            .filter(|entry| !is_staging(&entry.file_name().to_string_lossy()))
            .filter(|entry| decanter_ps::library::read_game(&entry.path()).is_none())
            .collect();
        entries.sort_by_key(|entry| entry.file_name() == "sce_sys");
        for entry in entries {
            crate::achv_try!(
                std::fs::rename(entry.path(), staging.join(entry.file_name()))
                    .map_err(|err| failed(&entry.path(), err))
            );
        }
    }
    let target = games.join(&name);
    crate::achv_try!(std::fs::rename(&staging, &target).map_err(|err| failed(&target, err)));
    crate::dump_size::moved(games, &target);
    tracing::info!(games = %games.display(), to = %target.display(), "PS dump put in its own folder");
    Ok(decanter_ps::library::read_game(&target).map(|game| (game, target)))
}

/// A `.<name>.wrapping` folder an earlier [`wrap_root_dump`] left in
/// `games`, with the name it was taking.
fn unfinished_wrap(games: &Path) -> Option<(PathBuf, String)> {
    std::fs::read_dir(games).ok()?.flatten().find_map(|entry| {
        let file_name = entry.file_name();
        let name = file_name.to_str()?.strip_prefix('.')?.strip_suffix(".wrapping")?;
        (!name.is_empty() && !name.starts_with('.')).then(|| (entry.path(), name.to_owned()))
    })
}

/// A copy or wrap under way in a games folder, never part of a dump.
fn is_staging(name: &str) -> bool {
    name.starts_with('.') && (name.ends_with(".partial") || name.ends_with(".wrapping"))
}

/// `exe` with `from` swapped for `to` when it's under `from`.
fn remap(exe: &str, from: &Path, to: &Path) -> String {
    Path::new(exe).strip_prefix(from).map_or_else(
        |_| exe.to_owned(),
        |relative| to.join(relative).to_string_lossy().into_owned(),
    )
}
