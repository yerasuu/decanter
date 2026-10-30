//! The patch files psport reads as a title's own, shipped with decanter: they are Bloodborne's, so
//! they live in the bbport part rather than in psport's tree. Written into the data folder before
//! a launch, which hands the folder to the port as `PS_BUILTIN_PATCHES_DIR`.

use std::io;
use std::path::{Path, PathBuf};

use crate::files::write_atomic;

/// Where the files go, under the bbport data folder.
const FOLDER: &str = "builtin-patches";

const FILES: [(&str, &[u8]); 1] = [("Bloodborne.xml", include_bytes!("../../data/bloodborne/Bloodborne.xml"))];

/// The folder holding the shipped files, each written when it is missing or differs: they are
/// decanter's to update, not the user's to edit (the user's patches go in the title's own folder).
///
/// # Errors
/// Reading or writing one of the files.
pub async fn install(data: &Path) -> io::Result<PathBuf> {
    let dir = data.join(FOLDER);
    let folder = dir.clone();
    tokio::task::spawn_blocking(move || {
        for (name, contents) in FILES {
            let path = folder.join(name);
            if std::fs::read(&path).is_ok_and(|existing| existing == contents) {
                continue;
            }
            write_atomic(&path, contents)?;
            tracing::info!(path = %path.display(), "wrote a built-in patch file");
        }
        Ok::<(), io::Error>(())
    })
    .await
    .map_err(io::Error::other)??;
    Ok(dir)
}
