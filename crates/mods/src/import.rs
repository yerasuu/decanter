//! A folder of loose files becomes a mod: read only, copied into `incoming/<pid>-<n>/` with every
//! file hashed, then renamed into place in one step so a cut-short import leaves no mod behind.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};

use crate::game::{GameMods, INCOMING};
use crate::manifest::{FILES, ModFile, ModManifest, check_paths, display_name};
use crate::path::{ModId, ModPath};
use crate::{Error, FREE_SPACE_MARGIN, MAX_MOD_BYTES, MAX_MOD_FILES, Result};

const COPY_BUFFER: usize = 1 << 20;
const MAX_INCOMING_ATTEMPTS: u32 = 1000;

struct Found {
    path: ModPath,
    on_disk: PathBuf,
}

pub(crate) fn folder(game: &GameMods, source: &Path) -> Result<ModManifest> {
    fs::create_dir_all(&game.dir).map_err(Error::io(&game.dir))?;
    let source = source.canonicalize().map_err(Error::io(source))?;
    let mods = game.dir.canonicalize().map_err(Error::io(&game.dir))?;
    if source.starts_with(&mods) {
        return Err(Error::SourceInsideMods);
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(display_name)
        .unwrap_or_else(|| "Mod".to_owned());
    let (found, total) = walk(&source)?;
    if found.is_empty() {
        return Err(Error::Empty);
    }
    check_paths(found.iter().map(|file| &file.path), None)?;
    match decanter_core::disk_usage(&game.dir) {
        Some((free, _)) if total.saturating_add(FREE_SPACE_MARGIN) > free => {
            return Err(Error::NoSpace { needed: total, free });
        }
        Some(_) => {}
        None => tracing::warn!(path = %game.dir.display(), "free space unknown, importing the mod anyway"),
    }

    let incoming = make_incoming(&game.dir.join(INCOMING))?;
    let id = ModId::new();
    let copied = copy_in(&found, &incoming).and_then(|files| {
        let manifest = ModManifest {
            id: id.clone(),
            source_file_name: name.clone(),
            name,
            source_sha256: None,
            source_size: total,
            added_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX)),
            target: None,
            files,
        };
        manifest.write(&incoming)?;
        let destination = game.mod_dir(&id);
        fs::rename(&incoming, &destination).map_err(Error::io(&destination))?;
        Ok(manifest)
    });
    match copied {
        Ok(manifest) => {
            tracing::info!(
                mod_id = %manifest.id,
                name = manifest.name,
                files = manifest.files.len(),
                bytes = total,
                "mod imported from a folder"
            );
            Ok(manifest)
        }
        Err(err) => {
            if let Err(cleanup) = fs::remove_dir_all(&incoming) {
                tracing::warn!(path = %incoming.display(), %cleanup, "could not remove a failed import");
            }
            Err(err)
        }
    }
}

/// Regular files and folders only: a link or a device in the folder refuses the whole import,
/// since a partial mod is worse than none.
fn walk(source: &Path) -> Result<(Vec<Found>, u64)> {
    let mut found = Vec::new();
    let mut total = 0_u64;
    let mut pending = vec![(source.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in fs::read_dir(&dir).map_err(Error::io(&dir))? {
            let entry = entry.map_err(Error::io(&dir))?;
            let on_disk = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| Error::InvalidPath(name.to_string_lossy().into_owned()))?;
            let mut components = prefix.clone();
            components.push(name);
            let file_type = entry.file_type().map_err(Error::io(&on_disk))?;
            if file_type.is_dir() {
                pending.push((on_disk, components));
            } else if file_type.is_file() {
                let size = entry.metadata().map_err(Error::io(&on_disk))?.len();
                total = total.saturating_add(size);
                if total > MAX_MOD_BYTES {
                    return Err(Error::TooLarge);
                }
                found.push(Found {
                    path: ModPath::from_components(components.iter().map(String::as_str))?,
                    on_disk,
                });
                if found.len() > MAX_MOD_FILES {
                    return Err(Error::TooManyFiles);
                }
            } else {
                return Err(Error::NotAFile(on_disk));
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((found, total))
}

fn make_incoming(base: &Path) -> Result<PathBuf> {
    fs::create_dir_all(base).map_err(Error::io(base))?;
    let pid = std::process::id();
    let mut attempt = 0_u32;
    loop {
        let dir = base.join(format!("{pid}-{attempt}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempt < MAX_INCOMING_ATTEMPTS => {
                attempt = attempt.saturating_add(1);
            }
            Err(err) => return Err(Error::io(&dir)(err)),
        }
    }
}

/// The sizes recorded are what was copied, and the cap is enforced again while copying, since a
/// file may grow between the walk and the copy.
fn copy_in(found: &[Found], incoming: &Path) -> Result<Vec<ModFile>> {
    let files_dir = incoming.join(FILES);
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut total = 0_u64;
    let mut files = Vec::with_capacity(found.len());
    for file in found {
        let destination = file.path.under(&files_dir);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(Error::io(parent))?;
        }
        let mut reader = fs::File::open(&file.on_disk).map_err(Error::io(&file.on_disk))?;
        // Never through something already there: the folder is new and only this import writes it.
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(Error::io(&destination))?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(Error::io(&file.on_disk)(err)),
            };
            let chunk = buffer.get(..count).unwrap_or_default();
            size = size.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
            total = total.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
            if total > MAX_MOD_BYTES {
                return Err(Error::TooLarge);
            }
            hasher.update(chunk);
            writer.write_all(chunk).map_err(Error::io(&destination))?;
        }
        writer.flush().map_err(Error::io(&destination))?;
        files.push(ModFile {
            path: file.path.clone(),
            size,
            sha256: hex(&hasher.finalize()),
            executable: false,
        });
    }
    Ok(files)
}

fn hex(digest: &[u8]) -> String {
    let mut text = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        for nibble in [byte >> 4_u8, byte & 0x0f] {
            text.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    text
}
