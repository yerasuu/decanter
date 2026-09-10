//! Where a game's files land before they're moved into the library, and how
//! they get from one to the other.

use std::path::{Path, PathBuf};

use achv_runtime::sanitize_relative_path;
use anyhow::Context;

/// Where a game's files are downloaded before they're moved into the
/// library. Kept outside it so a failed or cancelled install never leaves
/// partial files where the game is supposed to live.
pub(super) fn staging_root(profile_id: &str, product_id: &str) -> anyhow::Result<PathBuf> {
    let base = achv_core::data_dir();
    let profile = crate::achv_try!(
        sanitize_relative_path(profile_id)
            .filter(|path| path.components().count() == 1)
            .context("suspicious profile_id")
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

            if tokio::fs::rename(&source, &destination).await.is_err() {
                crate::achv_try!(
                    tokio::fs::copy(&source, &destination)
                        .await
                        .with_context(|| format!("copying into {}", destination.display()))
                );
                tokio::fs::remove_file(&source).await.ok();
            }
        }
    }

    Ok(())
}
