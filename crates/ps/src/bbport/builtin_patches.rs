//! The patch files psport reads as a title's own, shipped with decanter: they are Bloodborne's, so
//! they live in the bbport part rather than in psport's tree. Written into the data folder before
//! a launch, which hands the folder to the port as `PS_BUILTIN_PATCHES_DIR`.

use std::io;
use std::path::{Path, PathBuf};

use crate::files::write_atomic;

/// Where the files go, under the bbport data folder.
const FOLDER: &str = "builtin-patches";

/// A shipped file, the title ids it is for (its own `TitleID` list) and its contents.
type Shipped = (&'static str, &'static [&'static str], &'static [u8]);

const FILES: [Shipped; 1] = [(
    "Bloodborne.xml",
    &[
        "CUSA00207",
        "CUSA00208",
        "CUSA00299",
        "CUSA00900",
        "CUSA01363",
        "CUSA03014",
        "CUSA03023",
        "CUSA03173",
    ],
    include_bytes!("../../data/bloodborne/Bloodborne.xml"),
)];

/// The folder holding the shipped files, each written when it is missing or differs: they are
/// decanter's to update, not the user's to edit (the user's patches go in the title's own folder).
///
/// # Errors
/// Reading or writing one of the files.
pub async fn install(data: &Path) -> io::Result<PathBuf> {
    let dir = data.join(FOLDER);
    let folder = dir.clone();
    tokio::task::spawn_blocking(move || {
        for (name, _, contents) in FILES {
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

/// Puts the shipped patch files for `title_id` into `dir`, a title's own patches folder, where a file
/// of that name is missing, so the Mods tab lists their patches for the user to switch (the port lists the
/// folder it is given). A file already there is the user's and is left alone.
///
/// # Errors
/// Writing one of the files.
pub fn seed(dir: &Path, title_id: &str) -> io::Result<()> {
    for (name, titles, contents) in FILES {
        let path = dir.join(name);
        if !titles.contains(&title_id) || path.exists() {
            continue;
        }
        write_atomic(&path, contents)?;
        tracing::info!(path = %path.display(), "seeded a title's patches folder with a shipped patch file");
    }
    Ok(())
}
