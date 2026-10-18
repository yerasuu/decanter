//! How much room an external game takes: decanter installed none of its
//! files, so no installer reports a size the way GOG's does, and it is
//! measured from disk instead, off the UI thread. A walk over every file is
//! slow, so the size is kept with a fingerprint of what was measured
//! ([`decanter_core::install_fingerprint`]) and walked again only once that
//! changes.

use std::path::{Path, PathBuf};

use decanter_core::{InstalledGame, Library};

/// What makes up the game on disk: its own prefix, and the executable's
/// folder when that lies outside it. A game sharing a launcher's prefix has
/// only its folder, since the prefix is the launcher's.
struct Parts<'game> {
    prefix: Option<PathBuf>,
    files: Option<&'game Path>,
    executable: Option<&'game Path>,
}

impl<'game> Parts<'game> {
    fn of(library: &Library, game: &'game InstalledGame) -> Self {
        let prefix = game
            .prefix_of
            .is_none()
            .then(|| library.compatdata(&game.account_id, &game.game_id))
            .flatten()
            .filter(|dir| dir.is_dir());
        let executable = game.launch_executable.as_deref().map(Path::new);
        let files = executable
            .and_then(Path::parent)
            // A shallow folder (`/`, `/home/<user>`) is never one game's own:
            // walking it would measure the whole disk.
            .filter(|dir| dir.is_dir() && dir.components().count() > 3)
            .filter(|dir| prefix.as_ref().is_none_or(|prefix| !dir.starts_with(prefix)));
        Self {
            prefix,
            files,
            executable,
        }
    }

    fn fingerprint(&self) -> String {
        let dirs: Vec<&Path> = self.prefix.as_deref().into_iter().chain(self.files).collect();
        decanter_core::install_fingerprint(&dirs, &self.executable.into_iter().collect::<Vec<_>>())
    }

    /// `None` when neither part is on disk.
    fn measure(&self) -> Option<u64> {
        if self.prefix.is_none() && self.files.is_none() {
            return None;
        }
        let size = |dir: &Path| {
            decanter_core::directory_size(dir)
                .inspect_err(
                    |err| tracing::warn!(dir = %dir.display(), %err, "could not measure an external game's folder"),
                )
                .unwrap_or(0)
        };
        Some(
            self.prefix
                .as_deref()
                .map_or(0, size)
                .saturating_add(self.files.map_or(0, size)),
        )
    }
}

/// `game` with a size that matches what is on disk: the recorded one while
/// its fingerprint still matches, else measured again and written to its
/// manifest in `library`. Blocking: stats the game's folders, and walks
/// every file when it has to measure.
pub fn refresh(library: &Library, game: InstalledGame) -> InstalledGame {
    let parts = Parts::of(library, &game);
    let fingerprint = parts.fingerprint();
    if game.size_on_disk.is_some() && game.size_fingerprint.as_deref() == Some(fingerprint.as_str()) {
        return game;
    }
    let size = parts.measure();
    let measured = InstalledGame {
        size_on_disk: size.or(game.size_on_disk),
        size_fingerprint: size.map(|_| fingerprint),
        ..game
    };
    match library.write_manifest(&measured) {
        Ok(()) => tracing::debug!(game_id = %measured.game_id, size = ?size, "external game size recorded"),
        Err(err) => tracing::warn!(game_id = %measured.game_id, %err, "could not record the external game's size"),
    }
    measured
}
