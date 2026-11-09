// SPDX-License-Identifier: GPL-3.0-or-later
//! The local side: the folder the catalog's `CloudSaveFolder` template names (spec, 4.5 and 9),
//! resolved against the game's Windows folders and confined to them, its files as the include and
//! exclude lists keep them, and the restore's writes into it.
//!
//! The template comes from Epic's catalog, so it is untrusted: it must start with one of spec 9's
//! variables, the rest must be a plain relative path, and the folder it lands on, followed through
//! whatever links already exist, must stay inside the variable's own folder (inside `drive_c` for
//! a Proton prefix). Within the folder no link is followed: a scan skips them, and a restore joins
//! every path through [`files::inside`] and replaces files by rename.

use core::time::Duration;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use decanter_runtime::install::files;
use decanter_runtime::install::{ManifestPath, PathFault};
use tokio::task::spawn_blocking;

use super::{MAX_SAVE_BYTES, MAX_SAVE_FILES, SaveError, Shown};
use crate::library::is_id;
use crate::manifest::Sha1Digest;

/// Marks a restored file being written; the scan skips such files.
const PARTIAL_SUFFIX: &str = ".decanter-partial";
const MAX_TEMPLATE_LEN: usize = 1024;
const MAX_FILTERS: usize = 256;
const MAX_FOLDER_ENTRIES: usize = 100_000;

/// The catalog's save attributes for one title, as cached (spec, 4.5).
#[derive(Debug, Clone)]
pub struct SaveSpec {
    folder: String,
    include: String,
    exclude: String,
}

impl SaveSpec {
    /// `folder` is `CloudSaveFolder`; `include` and `exclude` are `CloudIncludeList` and
    /// `CloudExcludeList`, comma lists, empty for none.
    #[inline]
    #[must_use]
    pub fn new(folder: &str, include: &str, exclude: &str) -> Self {
        Self {
            folder: folder.trim().to_owned(),
            include: include.to_owned(),
            exclude: exclude.to_owned(),
        }
    }
}

/// The Windows folders a template's variables stand for, as the game sees them.
#[derive(Debug, Clone)]
pub struct SaveRoots {
    install_dir: PathBuf,
    profile: PathBuf,
    documents: PathBuf,
    local: PathBuf,
    local_low: PathBuf,
    roaming: PathBuf,
    saved_games: PathBuf,
    /// The prefix's `drive_c`, which every folder but the install's must resolve inside; `None`
    /// off a prefix, where each folder only confines what is under it.
    drive_c: Option<PathBuf>,
    account_id: String,
}

impl SaveRoots {
    /// Wine's layout for `steamuser`, the account Proton runs every game as, the same folders
    /// decanter's GOG saves read.
    #[inline]
    #[must_use]
    pub fn in_prefix(drive_c: &Path, install_dir: &Path, account_id: &str) -> Self {
        let profile = drive_c.join("users").join("steamuser");
        Self {
            install_dir: install_dir.to_path_buf(),
            documents: profile.join("Documents"),
            local: profile.join("AppData/Local"),
            local_low: profile.join("AppData/LocalLow"),
            roaming: profile.join("AppData/Roaming"),
            saved_games: profile.join("Saved Games"),
            profile,
            drive_c: Some(drive_c.to_path_buf()),
            account_id: account_id.to_owned(),
        }
    }

    /// A Windows user's own folders, where the system says they are; LocalLow and Saved Games are
    /// taken at their default places under the profile.
    #[inline]
    #[must_use]
    pub fn native(
        profile: &Path,
        documents: &Path,
        local: &Path,
        roaming: &Path,
        install_dir: &Path,
        account_id: &str,
    ) -> Self {
        Self {
            install_dir: install_dir.to_path_buf(),
            documents: documents.to_path_buf(),
            local: local.to_path_buf(),
            local_low: local.with_file_name("LocalLow"),
            roaming: roaming.to_path_buf(),
            saved_games: profile.join("Saved Games"),
            profile: profile.to_path_buf(),
            drive_c: None,
            account_id: account_id.to_owned(),
        }
    }
}

/// Why no save folder came out of the catalog's template.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FolderFault {
    #[error("the Epic catalog declares no cloud save folder for this game")]
    NotDeclared,
    #[error("the cloud save folder template is longer than {MAX_TEMPLATE_LEN} characters")]
    TooLong,
    #[error("the cloud save folder template does not start with a known folder variable")]
    NoVariable,
    #[error("the cloud save folder template uses the unknown variable {0:?}")]
    UnknownVariable(String),
    #[error("the cloud save folder template's path is refused: {0}")]
    Path(PathFault),
    #[error("the cloud save folder leads outside the game's folders")]
    Escapes,
    #[error("the cloud save folder is not a folder")]
    NotFolder,
    #[error("the account or game id cannot name a cloud save")]
    BadId,
    #[error("the cloud save folder cannot be looked at: {0}")]
    Io(String),
}

/// A save folder resolved from the catalog and confined, with the catalog's filters.
#[derive(Debug, Clone)]
pub struct SaveFolder {
    path: PathBuf,
    include: Vec<String>,
    exclude: Vec<String>,
}

impl SaveFolder {
    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn is_included(&self, relative: &ManifestPath) -> bool {
        let folded = relative.folded();
        (self.include.is_empty() || self.include.iter().any(|entry| matches(entry, &folded)))
            && !self.exclude.iter().any(|entry| matches(entry, &folded))
    }
}

/// Resolves `spec`'s template against `roots`.
///
/// # Errors
///
/// A [`FolderFault`]: no template, one that does not parse, or a folder that leads outside.
#[inline]
pub async fn save_folder(spec: &SaveSpec, roots: &SaveRoots) -> Result<SaveFolder, FolderFault> {
    let (spec, roots) = (spec.clone(), roots.clone());
    spawn_blocking(move || resolve(&spec, &roots))
        .await
        .map_err(|_| FolderFault::Io("the save folder task panicked".to_owned()))?
}

fn resolve(spec: &SaveSpec, roots: &SaveRoots) -> Result<SaveFolder, FolderFault> {
    let template = spec.folder.as_str();
    if template.is_empty() {
        return Err(FolderFault::NotDeclared);
    }
    if template.chars().count() > MAX_TEMPLATE_LEN {
        return Err(FolderFault::TooLong);
    }
    if !is_id(&roots.account_id) {
        return Err(FolderFault::BadId);
    }
    let (variable, rest) = template
        .strip_prefix('{')
        .and_then(|after| after.split_once('}'))
        .ok_or(FolderFault::NoVariable)?;
    let (root, confine) = root_of(variable, roots)?;
    let rest = substitute_epic_id(rest.trim_start_matches(['/', '\\']), &roots.account_id)?;
    let mut path = root.clone();
    if !rest.is_empty() {
        let relative = ManifestPath::parse(&rest).map_err(FolderFault::Path)?;
        for component in relative.as_str().split('/') {
            path = matched_child(&path, component);
        }
    }
    confined(&path, &root, confine)?;
    Ok(SaveFolder {
        path,
        include: filters(&spec.include),
        exclude: filters(&spec.exclude),
    })
}

/// The folder a variable names and the folder it must stay in. Spec 9 lists the names; which
/// Windows folder each one is, is INFER from the names themselves (`appdata` taken as the local
/// one, `userdir` as Documents) and UNVERIFIED.
fn root_of<'roots>(variable: &str, roots: &'roots SaveRoots) -> Result<(PathBuf, &'roots Path), FolderFault> {
    let root = match variable.to_ascii_lowercase().as_str() {
        "installdir" => return Ok((roots.install_dir.clone(), &roots.install_dir)),
        "userprofile" => &roots.profile,
        "userdir" => &roots.documents,
        "appdata" => &roots.local,
        "locallow" => &roots.local_low,
        "roaming" => &roots.roaming,
        "usersavedgames" => &roots.saved_games,
        _ => return Err(FolderFault::UnknownVariable(variable.chars().take(32).collect())),
    };
    Ok((root.clone(), roots.drive_c.as_deref().unwrap_or(root)))
}

/// `{EpicId}` anywhere after the leading variable, as the account id; any other brace is refused.
fn substitute_epic_id(rest: &str, account_id: &str) -> Result<String, FolderFault> {
    let mut out = String::with_capacity(rest.len());
    let mut remaining = rest;
    while let Some(open) = remaining.find('{') {
        out.push_str(remaining.get(..open).unwrap_or_default());
        let after = remaining.get(open.saturating_add(1)..).unwrap_or_default();
        let (name, tail) = after.split_once('}').ok_or(FolderFault::NoVariable)?;
        if !name.eq_ignore_ascii_case("epicid") {
            return Err(FolderFault::UnknownVariable(name.chars().take(32).collect()));
        }
        out.push_str(account_id);
        remaining = tail;
    }
    out.push_str(remaining);
    if out.contains('}') {
        return Err(FolderFault::NoVariable);
    }
    Ok(out)
}

/// `name` under `dir`, or the entry already there whose name differs from it only in case: the
/// catalog spells folders as Windows would, which a Linux file system tells apart.
fn matched_child(dir: &Path, name: &str) -> PathBuf {
    let exact = dir.join(name);
    if fs::symlink_metadata(&exact).is_ok() {
        return exact;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return exact;
    };
    entries
        .take(MAX_FOLDER_ENTRIES)
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|found| found.eq_ignore_ascii_case(name))
        })
        .map_or(exact, |entry| entry.path())
}

/// Whether `path`, through the links that exist on its way, stays under `root`, and `root` under
/// `confine`; a `path` that exists must be a folder.
fn confined(path: &Path, root: &Path, confine: &Path) -> Result<(), FolderFault> {
    let io = |err: io::Error| FolderFault::Io(err.kind().to_string());
    let confine = match fs::canonicalize(confine) {
        Ok(confine) => confine,
        // A prefix or install that is not there yet holds nothing a link could lead out of.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)),
    };
    let real_root = deepest_real(root).map_err(io)?;
    let real = deepest_real(path).map_err(io)?;
    if !real_root.starts_with(&confine) || !real.starts_with(&real_root) {
        return Err(FolderFault::Escapes);
    }
    match fs::metadata(path) {
        Ok(meta) if !meta.is_dir() => Err(FolderFault::NotFolder),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io(err)),
    }
}

/// `path` with its deepest existing ancestor made canonical and the missing rest appended.
fn deepest_real(path: &Path) -> io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(real) => {
                let mut out = real;
                out.extend(missing.iter().rev());
                return Ok(out);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                    return Ok(path.to_path_buf());
                };
                missing.push(name.to_owned());
                current = parent;
            }
            Err(err) => return Err(err),
        }
    }
}

/// A comma list as lower-case `/`-separated entries, at most [`MAX_FILTERS`] of them.
fn filters(list: &str) -> Vec<String> {
    list.split(',')
        .map(|entry| entry.trim().replace('\\', "/").trim_start_matches('/').to_lowercase())
        .filter(|entry| !entry.is_empty())
        .take(MAX_FILTERS)
        .collect()
}

/// Spec 9's filter entry: the path itself, a prefix of it (a folder ending in `/` among them),
/// or a glob of `*` and `?`, all ignoring case. Which of these Epic means by each entry is
/// UNVERIFIED; an entry matching by any of them counts.
fn matches(entry: &str, folded: &str) -> bool {
    if entry.contains(['*', '?']) {
        return glob(entry.as_bytes(), folded.as_bytes());
    }
    folded == entry || folded.starts_with(entry)
}

/// Iterative wildcard matching that backtracks only to the last `*`, so a hostile pattern from the
/// catalog costs at most pattern times text steps.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut at_pattern, mut at_text) = (0_usize, 0_usize);
    let mut last_star: Option<(usize, usize)> = None;
    while at_text < text.len() {
        match pattern.get(at_pattern) {
            Some(b'*') => {
                last_star = Some((at_pattern, at_text));
                at_pattern = at_pattern.saturating_add(1);
            }
            Some(&byte) if byte == b'?' || text.get(at_text) == Some(&byte) => {
                at_pattern = at_pattern.saturating_add(1);
                at_text = at_text.saturating_add(1);
            }
            _ => match last_star {
                Some((star_pattern, star_text)) => {
                    at_pattern = star_pattern.saturating_add(1);
                    at_text = star_text.saturating_add(1);
                    last_star = Some((star_pattern, at_text));
                }
                None => return false,
            },
        }
    }
    pattern
        .get(at_pattern..)
        .unwrap_or_default()
        .iter()
        .all(|byte| *byte == b'*')
}

/// One regular file of the folder.
pub(super) struct LocalFile {
    pub(super) path: PathBuf,
    pub(super) relative: ManifestPath,
    pub(super) size: u64,
    /// Kept by the catalog's filters, and so part of an upload.
    pub(super) is_included: bool,
}

/// The folder's regular files, every one listed so a restore finds what it replaces; only the
/// included ones count toward the totals and the newest time.
pub(super) struct Scan {
    pub(super) files: Vec<LocalFile>,
    pub(super) included: usize,
    pub(super) included_bytes: u64,
    pub(super) newest: Option<i64>,
}

pub(super) fn scan(folder: &SaveFolder) -> Result<Scan, SaveError> {
    let local = |err: io::Error| SaveError::Local(format!("scanning the save folder: {}", err.kind()));
    let mut scan = Scan {
        files: Vec::new(),
        included: 0,
        included_bytes: 0,
        newest: None,
    };
    let (mut links, mut unnamed, mut excluded) = (0_usize, 0_usize, 0_usize);
    match fs::symlink_metadata(&folder.path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(SaveError::Folder(FolderFault::NotFolder)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(scan),
        Err(err) => return Err(local(err)),
    }
    let mut pending = vec![folder.path.clone()];
    while let Some(dir) = pending.pop() {
        for listed in fs::read_dir(&dir).map_err(local)? {
            let entry = listed.map_err(local)?;
            let kind = entry.file_type().map_err(local)?;
            let path = entry.path();
            if kind.is_symlink() {
                links = links.saturating_add(1);
                continue;
            }
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Some(relative) = relative_name(&folder.path, &path) else {
                unnamed = unnamed.saturating_add(1);
                continue;
            };
            if relative.as_str().ends_with(PARTIAL_SUFFIX) {
                continue;
            }
            if scan.files.len() >= MAX_SAVE_FILES {
                return Err(SaveError::TooLarge {
                    what: "save folder files",
                    limit: u64::try_from(MAX_SAVE_FILES).unwrap_or(u64::MAX),
                });
            }
            let meta = entry.metadata().map_err(local)?;
            let modified = unix_seconds(meta.modified().map_err(local)?);
            let is_included = folder.is_included(&relative);
            if is_included {
                scan.included = scan.included.saturating_add(1);
                scan.included_bytes = scan.included_bytes.saturating_add(meta.len());
                scan.newest = scan.newest.max(Some(modified));
                if scan.included_bytes > MAX_SAVE_BYTES {
                    return Err(SaveError::TooLarge {
                        what: "save folder",
                        limit: MAX_SAVE_BYTES,
                    });
                }
            } else {
                excluded = excluded.saturating_add(1);
            }
            scan.files.push(LocalFile {
                path,
                relative,
                size: meta.len(),
                is_included,
            });
        }
    }
    if links > 0 || unnamed > 0 || excluded > 0 {
        tracing::info!(
            links,
            unnamed,
            excluded,
            kept = scan.included,
            "epic save folder entries left out of the cloud saves"
        );
    }
    Ok(scan)
}

fn relative_name(root: &Path, path: &Path) -> Option<ManifestPath> {
    let segments = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|component| match component {
            Component::Normal(segment) => segment.to_str(),
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => None,
        })
        .collect::<Option<Vec<_>>>()?;
    ManifestPath::parse(&segments.join("/")).ok()
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

/// The file's content, refused past [`MAX_SAVE_BYTES`] or when it is no longer a regular file.
pub(super) fn read(path: &Path) -> Result<Vec<u8>, SaveError> {
    let local = |err: io::Error| SaveError::Local(format!("reading a save file: {}", err.kind()));
    let meta = fs::symlink_metadata(path).map_err(local)?;
    if !meta.is_file() {
        return Err(SaveError::Local(
            "a save file changed into something else while read".to_owned(),
        ));
    }
    if meta.len() > MAX_SAVE_BYTES {
        return Err(SaveError::TooLarge {
            what: "save file",
            limit: MAX_SAVE_BYTES,
        });
    }
    let mut body = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or_default());
    File::open(path)
        .and_then(|file| file.take(MAX_SAVE_BYTES.saturating_add(1)).read_to_end(&mut body))
        .map_err(local)?;
    Ok(body)
}

pub(super) fn sha1_of(path: &Path) -> Result<Sha1Digest, SaveError> {
    read(path).map(|body| Sha1Digest::of(&body))
}

/// One file a restore writes: at the local file it replaces when there is one, else at its path
/// under the folder.
pub(super) struct Write {
    pub(super) relative: ManifestPath,
    pub(super) existing: Option<PathBuf>,
    pub(super) body: Vec<u8>,
}

/// Writes every file, its time set to the snapshot's, and returns how many were written. The first
/// failure stops the rest; what was written stays, each file whole.
pub(super) fn write_all(folder: &SaveFolder, writes: Vec<Write>, snapshot_time: i64) -> Result<usize, SaveError> {
    let modified = UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::try_from(snapshot_time).unwrap_or_default()))
        .unwrap_or(UNIX_EPOCH);
    let mut written = 0_usize;
    for write in writes {
        let shown = Shown(write.relative.as_str());
        // A new file goes into the folders already there under another case, as the Windows game
        // would find it.
        let placed = write.existing.unwrap_or_else(|| {
            write
                .relative
                .as_str()
                .split('/')
                .fold(folder.path.clone(), |dir, name| matched_child(&dir, name))
        });
        let relative = relative_name(&folder.path, &placed)
            .ok_or_else(|| SaveError::Local(format!("{shown} does not stay in the save folder")))?;
        // A folder on the way may have become a link since the scan.
        let target =
            files::inside(&folder.path, &relative).map_err(|err| SaveError::Local(format!("{shown}: {err}")))?;
        write_atomically(&target, &write.body, modified).map_err(|err| {
            tracing::warn!(path = %shown, error = %err, "writing a restored epic save failed");
            SaveError::Local(format!("writing {shown}: {}", err.kind()))
        })?;
        written = written.saturating_add(1);
    }
    Ok(written)
}

/// Replaces `target` with `body` through a fsynced sibling renamed over it, so a file is never
/// half written and a link at `target` is replaced, not followed.
fn write_atomically(target: &Path, body: &[u8], modified: SystemTime) -> io::Result<()> {
    let (Some(dir), Some(file_name)) = (target.parent(), target.file_name()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "save path has no folder"));
    };
    fs::create_dir_all(dir)?;
    let mut partial_name = file_name.to_os_string();
    partial_name.push(PARTIAL_SUFFIX);
    let partial = dir.join(partial_name);
    match fs::remove_file(&partial) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        Ok(()) | Err(_) => {}
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(body)?;
            file.set_modified(modified)?;
            file.sync_all()
        });
    if let Err(err) = written.and_then(|()| fs::rename(&partial, target)) {
        drop(fs::remove_file(&partial));
        return Err(err);
    }
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}
