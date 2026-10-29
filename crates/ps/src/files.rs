use std::io;
use std::path::Path;

/// Writes then renames, so a port starting at the same moment never reads half a file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    std::fs::write(&temp, contents)?;
    std::fs::rename(&temp, path)
}

/// The entries of `dir`, a folder decanter manages, whose names `doomed` picks; a name that isn't
/// UTF-8 is never picked. A symlink goes as the link, never what it points at. Best effort: each
/// entry's outcome is said in the log, as `what`.
pub async fn remove_entries(dir: &Path, what: &'static str, doomed: impl Fn(&str) -> bool) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.file_name().to_str().is_some_and(&doomed) {
            continue;
        }
        let path = entry.path();
        match remove_entry(&entry, &path).await {
            Ok(()) => tracing::info!(what, path = %path.display(), "removed"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(what, path = %path.display(), %err, "could not remove"),
        }
    }
}

async fn remove_entry(entry: &tokio::fs::DirEntry, path: &Path) -> io::Result<()> {
    match entry.file_type().await {
        Ok(kind) if kind.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(err) => Err(err),
    }
}
