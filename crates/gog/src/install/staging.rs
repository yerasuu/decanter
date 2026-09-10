//! Where a game's files land before they're moved into the library, and how
//! they get from one to the other.

use std::path::{Path, PathBuf};

use achv_runtime::sanitize_relative_path;
use anyhow::Context;

/// Where a game's files are downloaded before they're moved into the
/// library. Kept outside it so a failed or cancelled install never leaves
/// partial files where the game is supposed to live.
pub(super) fn staging_root(account_id: &str, product_id: &str) -> anyhow::Result<PathBuf> {
    let base = achv_core::data_dir();
    let profile = crate::achv_try!(
        sanitize_relative_path(account_id)
            .filter(|path| path.components().count() == 1)
            .context("suspicious account_id")
    );
    let game = crate::achv_try!(
        sanitize_relative_path(product_id)
            .filter(|path| path.components().count() == 1)
            .context("suspicious product_id")
    );
    Ok(base.join("staging").join(profile).join(game))
}

/// Moves a finished download from staging into the library, merging into
/// whatever is already there rather than replacing the directory — a
/// reinstall shouldn't take out save files the game keeps beside itself.
///
/// `rename` handles the common case (staging and the prefix on one
/// filesystem) in constant time; a separate mount falls back to copying.
pub(super) async fn move_into_library(staging: &Path, destination_root: &Path) -> anyhow::Result<()> {
    let mut directories = vec![staging.to_path_buf()];
    let mut staged = 0usize;

    while let Some(directory) = directories.pop() {
        let mut entries = crate::achv_try!(
            tokio::fs::read_dir(&directory)
                .await
                .with_context(|| format!("reading {}", directory.display()))
        );

        while let Some(entry) = crate::achv_try!(entries.next_entry().await) {
            let source = entry.path();
            if crate::achv_try!(entry.file_type().await).is_dir() {
                directories.push(source);
                continue;
            }

            let relative = source
                .strip_prefix(staging)
                .expect("walked paths are always under the staging root");
            let destination = destination_root.join(relative);

            if let Some(parent) = destination.parent() {
                crate::achv_try!(
                    tokio::fs::create_dir_all(parent)
                        .await
                        .with_context(|| format!("creating {}", parent.display()))
                );
            }

            match tokio::fs::rename(&source, &destination).await {
                Ok(()) => tracing::debug!(relative = %relative.display(), "staged file (renamed into place)"),
                Err(_) => {
                    // A separate mount can't be renamed across, so this
                    // falls back to a copy — the rename error itself isn't
                    // useful (it's always "cross-device link" in practice)
                    // so it's dropped in favor of the copy's own error.
                    crate::achv_try!(
                        tokio::fs::copy(&source, &destination)
                            .await
                            .with_context(|| format!("copying into {}", destination.display()))
                    );
                    if let Err(err) = tokio::fs::remove_file(&source).await {
                        tracing::warn!(path = %source.display(), error = %err, "couldn't remove staged file after copying it into place");
                    }
                    tracing::debug!(relative = %relative.display(), "staged file (copied across filesystems)");
                }
            }
            staged += 1;
        }
    }

    tracing::debug!(file_count = staged, "moved staged install into library");
    Ok(())
}
