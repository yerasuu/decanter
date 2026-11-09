//! A zip or 7z archive becomes a mod (design, section 5). Its directory is read and every entry
//! checked before anything is written: names become [`ModPath`]s or refuse the whole archive, links
//! and devices refuse it, and the counts, sizes and ratios are capped. Extraction then enforces the
//! sizes again, since a header can lie, into a fresh `incoming/<pid>-<n>/` that only this import
//! writes, renamed into place once complete. Nothing in the archive is ever run.

mod sevenz;
mod zipped;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};

use crate::game::{GameMods, INCOMING};
use crate::import::{hex, make_incoming};
use crate::manifest::{FILES, ModFile, ModManifest, check_paths, display_name};
use crate::merge::walk_install;
use crate::path::{ModId, ModPath};
use crate::progress::Progress;
use crate::{Error, FREE_SPACE_MARGIN, MAX_ENTRIES, MAX_MOD_BYTES, MAX_MOD_FILES, MAX_RATIO, Result};

const COPY_BUFFER: usize = 1 << 20;
/// Below this an entry can't be a decompression bomb whatever its ratio, and padding files of
/// zeros legitimately compress far beyond 1 000:1.
const RATIO_FLOOR: u64 = 1 << 20;
/// How deep the install's folders are offered as a mod's target.
const TARGET_DEPTH: usize = 3;
const MAX_TARGETS: usize = 500;
/// How many of the files a mod would replace a preview keeps to show.
const SHOWN_OVERRIDES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveFormat {
    Zip,
    SevenZ,
}

/// One entry of an archive's directory, before its name is checked.
struct RawEntry {
    name: String,
    is_dir: bool,
    size: u64,
    /// What the entry takes in the archive, when the format knows it per entry; 7z checks the
    /// ratio per solid block itself.
    packed: Option<u64>,
}

/// One file of an archive, checked.
#[derive(Debug, Clone)]
struct Entry {
    /// The archive's own name, which the format's reader looks the entry up by.
    raw: String,
    path: ModPath,
    size: u64,
}

/// What an archive would add, read before the user says *Add*: nothing is written to the library
/// until then.
#[derive(Debug, Clone)]
pub struct ArchivePreview {
    format: ArchiveFormat,
    name: String,
    source_file_name: String,
    size: u64,
    stripped: Option<ModPath>,
    target: Option<ModPath>,
    folders: Vec<ModPath>,
    has_installer: bool,
    duplicate_of: Option<(ModId, String)>,
    source: PathBuf,
    source_sha256: String,
    source_size: u64,
    source_modified: Option<SystemTime>,
    entries: Vec<Entry>,
    install: HashSet<String>,
}

impl ArchivePreview {
    #[must_use]
    #[inline]
    pub const fn format(&self) -> ArchiveFormat {
        self.format
    }

    /// From the archive's file name, editable before adding.
    #[must_use]
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    #[inline]
    pub fn source_file_name(&self) -> &str {
        &self.source_file_name
    }

    #[must_use]
    #[inline]
    pub const fn files(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    #[inline]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// The single folder wrapping every file that matched nothing of the game's, left out.
    #[must_use]
    #[inline]
    pub const fn stripped(&self) -> Option<&ModPath> {
        self.stripped.as_ref()
    }

    /// Where the files go, preselected by matching the archive's top level against the install's.
    #[must_use]
    #[inline]
    pub const fn target(&self) -> Option<&ModPath> {
        self.target.as_ref()
    }

    /// The install's folders a target can be picked from.
    #[must_use]
    #[inline]
    pub fn folders(&self) -> &[ModPath] {
        &self.folders
    }

    /// A FOMOD script or an installer program, which decanter imports as plain files and never runs.
    #[must_use]
    #[inline]
    pub const fn has_installer(&self) -> bool {
        self.has_installer
    }

    /// The game's mod already made from this same archive, and its name.
    #[must_use]
    #[inline]
    pub fn duplicate_of(&self) -> Option<(&ModId, &str)> {
        self.duplicate_of.as_ref().map(|found| (&found.0, found.1.as_str()))
    }

    /// The game's files the mod would replace with its files under `target`, as many as a preview
    /// shows, and how many there are in all.
    #[must_use]
    #[inline]
    pub fn overrides(&self, target: Option<&ModPath>) -> (Vec<ModPath>, usize) {
        let mut shown = Vec::new();
        let mut count = 0_usize;
        for entry in &self.entries {
            let Ok(in_game) = entry.path.inside(target) else {
                continue;
            };
            if self.install.contains(&in_game.folded()) {
                count = count.saturating_add(1);
                if shown.len() < SHOWN_OVERRIDES {
                    shown.push(in_game);
                }
            }
        }
        (shown, count)
    }
}

pub(crate) fn preview(game: &GameMods, source: &Path, progress: &Progress) -> Result<ArchivePreview> {
    let (format, metadata) = open(source)?;
    let source_sha256 = hash(source, metadata.len(), progress)?;
    let listed = list(format, source)?;
    let install_files = walk_install(&game.install)?;
    let (entries, stripped) = strip_wrapper(listed, &install_files)?;
    let install: HashSet<String> = install_files.iter().map(ModPath::folded).collect();
    let folders = install_folders(&install_files);
    let target = preselect(&entries, &install_files, &folders);
    check_paths(entries.iter().map(|entry| &entry.path), target.as_ref())?;
    let has_installer = entries.iter().any(|entry| is_installer(&entry.path));
    let duplicate_of = game
        .list()?
        .into_iter()
        .find(|manifest| manifest.source_sha256.as_deref() == Some(source_sha256.as_str()))
        .map(|manifest| (manifest.id, manifest.name));
    let source_file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(display_name)
        .unwrap_or_else(|| "Mod".to_owned());
    let name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(display_name)
        .unwrap_or_else(|| source_file_name.clone());
    let size = entries.iter().map(|entry| entry.size).sum();
    tracing::info!(
        path = %source.display(),
        ?format,
        files = entries.len(),
        bytes = size,
        stripped = ?stripped.as_ref().map(ModPath::as_str),
        target = ?target.as_ref().map(ModPath::as_str),
        has_installer,
        duplicate = ?duplicate_of.as_ref().map(|found| found.0.as_str()),
        "mod archive read"
    );
    Ok(ArchivePreview {
        format,
        name,
        source_file_name,
        size,
        stripped,
        target,
        folders,
        has_installer,
        duplicate_of,
        source: source.to_path_buf(),
        source_sha256,
        source_size: metadata.len(),
        source_modified: metadata.modified().ok(),
        entries,
        install,
    })
}

/// Extracts what `preview` read into a new mod named `name`, with its files under `target`.
pub(crate) fn import(
    game: &GameMods,
    preview: &ArchivePreview,
    name: &str,
    target: Option<ModPath>,
    progress: &Progress,
) -> Result<ModManifest> {
    let name = display_name(name).ok_or_else(|| Error::InvalidName(name.to_owned()))?;
    let (format, metadata) = open(&preview.source)?;
    if format != preview.format
        || metadata.len() != preview.source_size
        || metadata.modified().ok() != preview.source_modified
    {
        return Err(Error::SourceChanged);
    }
    check_paths(preview.entries.iter().map(|entry| &entry.path), target.as_ref())?;
    match decanter_core::disk_usage(&game.dir) {
        Some((free, _)) if preview.size.saturating_add(FREE_SPACE_MARGIN) > free => {
            return Err(Error::NoSpace {
                needed: preview.size,
                free,
            });
        }
        Some(_) => {}
        None => tracing::warn!(path = %game.dir.display(), "free space unknown, importing the mod anyway"),
    }
    fs::create_dir_all(&game.dir).map_err(Error::io(&game.dir))?;
    let incoming = make_incoming(&game.dir.join(INCOMING))?;
    let id = ModId::new();
    let extracted = extract(preview, &incoming, progress).and_then(|files| {
        let manifest = ModManifest {
            id: id.clone(),
            name,
            source_file_name: preview.source_file_name.clone(),
            source_sha256: Some(preview.source_sha256.clone()),
            source_size: preview.source_size,
            added_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX)),
            target,
            files,
        };
        manifest.write(&incoming)?;
        let destination = game.mod_dir(&id);
        fs::rename(&incoming, &destination).map_err(Error::io(&destination))?;
        Ok(manifest)
    });
    match extracted {
        Ok(manifest) => {
            tracing::info!(
                mod_id = %manifest.id,
                name = manifest.name,
                format = ?preview.format,
                files = manifest.files.len(),
                bytes = preview.size,
                "mod imported from an archive"
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

/// The format, read from the file's first bytes rather than its name.
fn open(source: &Path) -> Result<(ArchiveFormat, fs::Metadata)> {
    const ZIP: &[u8] = b"PK\x03\x04";
    const ZIP_EMPTY: &[u8] = b"PK\x05\x06";
    const SEVEN_Z: &[u8] = b"7z\xBC\xAF\x27\x1C";
    const RAR: &[u8] = b"Rar!\x1A\x07";
    let mut file = fs::File::open(source).map_err(Error::io(source))?;
    let metadata = file.metadata().map_err(Error::io(source))?;
    if !metadata.is_file() {
        return Err(Error::NotAFile(source.to_path_buf()));
    }
    let mut magic = [0_u8; 6];
    let mut read = 0_usize;
    while let Some(rest) = magic.get_mut(read..).filter(|rest| !rest.is_empty()) {
        match file.read(rest) {
            Ok(0) => break,
            Ok(count) => read = read.saturating_add(count),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(Error::io(source)(err)),
        }
    }
    let magic = magic.get(..read).unwrap_or_default();
    if magic.starts_with(ZIP) || magic.starts_with(ZIP_EMPTY) {
        Ok((ArchiveFormat::Zip, metadata))
    } else if magic.starts_with(SEVEN_Z) {
        Ok((ArchiveFormat::SevenZ, metadata))
    } else if magic.starts_with(RAR) {
        Err(Error::Rar)
    } else {
        Err(Error::UnknownFormat)
    }
}

/// The SHA-256 of the file the user gave, which recognises the same archive added twice.
fn hash(source: &Path, size: u64, progress: &Progress) -> Result<String> {
    progress.start(size);
    let mut file = fs::File::open(source).map_err(Error::io(source))?;
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut hasher = Sha256::new();
    loop {
        let count = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(Error::io(source)(err)),
        };
        hasher.update(buffer.get(..count).unwrap_or_default());
        progress.advance(u64::try_from(count).unwrap_or(u64::MAX))?;
    }
    Ok(hex(&hasher.finalize()))
}

fn list(format: ArchiveFormat, source: &Path) -> Result<Vec<Entry>> {
    let raw = match format {
        ArchiveFormat::Zip => zipped::list(source)?,
        ArchiveFormat::SevenZ => sevenz::list(source)?,
    };
    check_entries(raw)
}

/// Every rule an entry keeps, any break refusing the whole archive: a partial mod is worse than
/// none.
fn check_entries(raw: Vec<RawEntry>) -> Result<Vec<Entry>> {
    if raw.len() > MAX_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    let mut entries = Vec::with_capacity(raw.len());
    let mut total = 0_u64;
    for entry in raw {
        let path = entry_path(&entry.name)?;
        if entry.is_dir {
            continue;
        }
        if let Some(packed) = entry.packed {
            check_ratio(&entry.name, entry.size, packed)?;
        }
        total = total.saturating_add(entry.size);
        if total > MAX_MOD_BYTES {
            return Err(Error::TooLarge);
        }
        entries.push(Entry {
            raw: entry.name,
            path,
            size: entry.size,
        });
        if entries.len() > MAX_MOD_FILES {
            return Err(Error::TooManyFiles);
        }
    }
    if entries.is_empty() {
        return Err(Error::Empty);
    }
    check_paths(entries.iter().map(|entry| &entry.path), None)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// An entry's name as a path inside the mod: `\` is a separator as Windows tools write it, a
/// folder's trailing `/` goes, and then [`ModPath`] refuses anything absolute, `..`, `.`, empty
/// components, drive letters and control characters.
fn entry_path(name: &str) -> Result<ModPath> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return Err(Error::InvalidPath(name));
    }
    ModPath::parse(name.strip_suffix('/').unwrap_or(&name))
}

pub(super) fn check_ratio(name: &str, size: u64, packed: u64) -> Result<()> {
    if size > RATIO_FLOOR && size > packed.saturating_mul(MAX_RATIO) {
        return Err(Error::Ratio(name.to_owned()));
    }
    Ok(())
}

/// Takes off one folder that wraps every file when no name in the game is that folder's, as a
/// mod packed as `SomeMod-1.2/Data/…` for a game whose root holds `Data`; a `textures/` wrapper
/// stays when the game has a `textures` folder anywhere, which the target is then matched to.
fn strip_wrapper(mut entries: Vec<Entry>, install_files: &[ModPath]) -> Result<(Vec<Entry>, Option<ModPath>)> {
    let Some(first) = entries.first().and_then(|entry| entry.path.components().next()) else {
        return Ok((entries, None));
    };
    let wrapper = first.to_owned();
    let folded = wrapper.to_lowercase();
    let wraps_all = entries.iter().all(|entry| {
        entry.path.depth() > 1
            && entry
                .path
                .components()
                .next()
                .is_some_and(|name| name.to_lowercase() == folded)
    });
    let in_game = install_files
        .iter()
        .any(|file| file.components().any(|name| name.to_lowercase() == folded));
    if !wraps_all || in_game {
        return Ok((entries, None));
    }
    for entry in &mut entries {
        let rest = entry
            .path
            .as_str()
            .split_once('/')
            .map(|(_, rest)| rest)
            .ok_or_else(|| Error::InvalidPath(entry.path.to_string()))?;
        entry.path = ModPath::parse(rest)?;
    }
    Ok((entries, Some(ModPath::parse(&wrapper)?)))
}

/// The install's folders down to [`TARGET_DEPTH`], as the install spells them.
fn install_folders(files: &[ModPath]) -> Vec<ModPath> {
    let mut folders = BTreeSet::new();
    for file in files {
        let mut folder = String::new();
        for name in file
            .components()
            .take(file.depth().saturating_sub(1))
            .take(TARGET_DEPTH)
        {
            if !folder.is_empty() {
                folder.push('/');
            }
            folder.push_str(name);
            folders.insert(folder.clone());
        }
    }
    folders
        .into_iter()
        .filter_map(|folder| ModPath::parse(&folder).ok())
        .take(MAX_TARGETS)
        .collect()
}

/// The game's root when the archive's top level matches it, else the install folder whose
/// children match most of the archive's top level (a texture pack's `textures/` and `meshes/` into
/// `Data/`), else the root.
fn preselect(entries: &[Entry], install_files: &[ModPath], folders: &[ModPath]) -> Option<ModPath> {
    let tops: HashSet<String> = entries
        .iter()
        .filter_map(|entry| entry.path.components().next())
        .map(str::to_lowercase)
        .collect();
    let mut children: HashMap<String, HashSet<String>> = HashMap::new();
    for file in install_files {
        let mut parent = String::new();
        for name in file.components() {
            children.entry(parent.clone()).or_default().insert(name.to_lowercase());
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(&name.to_lowercase());
        }
    }
    let matches = |folder: &str| {
        children
            .get(folder)
            .map_or(0, |names| tops.iter().filter(|top| names.contains(*top)).count())
    };
    let at_root = matches("");
    folders
        .iter()
        .map(|folder| (matches(&folder.folded()), folder))
        .filter(|&(count, _)| count > at_root)
        .max_by_key(|&(count, folder)| (count, core::cmp::Reverse(folder.depth())))
        .map(|(_, folder)| folder.clone())
}

fn is_installer(path: &ModPath) -> bool {
    let name = path.file_name().to_lowercase();
    let is_program = Path::new(&name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
    path.folded().ends_with("fomod/moduleconfig.xml")
        || (is_program && (name.contains("setup") || name.contains("install")))
}

/// Writes every file of `preview` under `incoming/files/`, hashing as it goes.
fn extract(preview: &ArchivePreview, incoming: &Path, progress: &Progress) -> Result<Vec<ModFile>> {
    progress.start(preview.size);
    let mut sink = Sink {
        files_dir: incoming.join(FILES),
        by_raw: preview
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.raw.as_str(), index))
            .collect(),
        entries: &preview.entries,
        written: vec![None; preview.entries.len()],
        buffer: vec![0_u8; COPY_BUFFER],
        total: 0,
        progress,
    };
    match preview.format {
        ArchiveFormat::Zip => zipped::extract(&preview.source, &mut sink)?,
        ArchiveFormat::SevenZ => sevenz::extract(&preview.source, &mut sink)?,
    }
    sink.written
        .into_iter()
        .zip(&preview.entries)
        .map(|(file, entry)| file.ok_or_else(|| Error::Archive(format!("{:?} was not in its data", entry.raw))))
        .collect()
}

/// Where the formats' readers hand each entry's data.
struct Sink<'preview> {
    files_dir: PathBuf,
    by_raw: HashMap<&'preview str, usize>,
    entries: &'preview [Entry],
    written: Vec<Option<ModFile>>,
    buffer: Vec<u8>,
    total: u64,
    progress: &'preview Progress,
}

impl Sink<'_> {
    /// Whether the archive's entry `raw` is one of the preview's files; folders are not.
    fn wants(&self, raw: &str) -> bool {
        self.by_raw.contains_key(raw)
    }

    fn write(&mut self, raw: &str, reader: &mut dyn io::Read) -> Result<()> {
        let index = *self
            .by_raw
            .get(raw)
            .ok_or_else(|| Error::Archive(format!("{raw:?} was not in its directory")))?;
        let entry = self
            .entries
            .get(index)
            .ok_or_else(|| Error::Archive(format!("{raw:?} was not in its directory")))?;
        if self.written.get(index).is_some_and(Option::is_some) {
            return Err(Error::Archive(format!("{raw:?} is in it twice")));
        }
        let destination = entry.path.under(&self.files_dir);
        if let Some(parent) = destination.parent() {
            os::create_dirs(parent).map_err(Error::io(parent))?;
        }
        // Never through something already there: the folder is new and only this import writes it.
        let mut writer = os::create_file(&destination).map_err(Error::io(&destination))?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        loop {
            let count = match reader.read(&mut self.buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(Error::Archive(format!("{raw:?}: {err}"))),
            };
            let counted = u64::try_from(count).unwrap_or(u64::MAX);
            size = size.saturating_add(counted);
            self.total = self.total.saturating_add(counted);
            if size > entry.size {
                return Err(Error::Oversized(raw.to_owned()));
            }
            if self.total > MAX_MOD_BYTES {
                return Err(Error::TooLarge);
            }
            let chunk = self.buffer.get(..count).unwrap_or_default();
            hasher.update(chunk);
            writer.write_all(chunk).map_err(Error::io(&destination))?;
            self.progress.advance(counted)?;
        }
        if size != entry.size {
            return Err(Error::Archive(format!("{raw:?} is shorter than the archive says")));
        }
        writer.flush().map_err(Error::io(&destination))?;
        if let Some(slot) = self.written.get_mut(index) {
            *slot = Some(ModFile {
                path: entry.path.clone(),
                size,
                sha256: hex(&hasher.finalize()),
                // Only Windows games have mods (GOG, Amazon and Epic under Proton), and the design
                // keeps the execute bit for a Linux game's files alone.
                executable: false,
            });
        }
        Ok(())
    }
}

#[cfg(unix)]
mod os {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::path::Path;

    /// Never more than the design allows, whatever the archive's own modes or the umask say.
    const FILE_MODE: u32 = 0o644;
    const DIR_MODE: u32 = 0o755;

    pub(super) fn create_dirs(path: &Path) -> io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(DIR_MODE).create(path)
    }

    pub(super) fn create_file(path: &Path) -> io::Result<fs::File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(path)
    }
}

#[cfg(not(unix))]
mod os {
    use std::fs;
    use std::io;
    use std::path::Path;

    pub(super) fn create_dirs(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    pub(super) fn create_file(path: &Path) -> io::Result<fs::File> {
        fs::OpenOptions::new().write(true).create_new(true).open(path)
    }
}
