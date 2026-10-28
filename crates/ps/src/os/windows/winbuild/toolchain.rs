//! The toolchain a Windows build runs, kept in the library (`<library>/psport/toolchain/win32/`):
//! each [`Archive`] downloaded, checked against its pinned SHA-256 and size, extracted with every
//! check of [`archive::unpack`] into a staging folder, then renamed into place and recorded with
//! the SHA-256 it was checked against and the SHA-256 of every file in it. A folder counts only
//! while its record names the pin of this decanter and its required files are there; a new pin
//! downloads it again. Before each build every file is hashed again ([`verify`]), so a build that
//! changed the toolchain (it runs with the user's rights) is caught before the next one uses it.
//! MinGit, the git the build fetches its sources with, is one of the pieces.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

use decanter_core::runner::{Progress, Stage};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::Error;
use super::pins::{self, Archive};
use crate::archive;
use crate::download;
use crate::os::windows::package;
use crate::paths::Root;

const MAX_RECORD_BYTES: u64 = 4096;
/// A piece's list of file hashes: about 110 bytes a line, 52k lines for the package.
const MAX_LIST_BYTES: u64 = 32 << 20;
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// Hashing the whole toolchain again (about 2 GB in 52k files) takes seconds from a warm disk cache,
/// but can take minutes on the first build after decanter starts, from a cold cache with the
/// antivirus scanning every file it opens (#1003); a disk that stops answering ends the build here.
const VERIFY_TIMEOUT: Duration = Duration::from_mins(20);
const VERIFY_TICK: Duration = Duration::from_secs(1);
const VERIFY_LOG_EVERY: Duration = Duration::from_secs(30);

/// The absolute paths a build runs and passes.
#[derive(Debug, Clone)]
pub struct Tools {
    pub git: PathBuf,
    pub cmake: PathBuf,
    pub ninja: PathBuf,
    pub llvm_mingw: PathBuf,
    /// The toolchain package's folder, with `psport-toolchain.cmake` and `clang64/`.
    pub package: PathBuf,
    /// The SHA-256 each file of `clang64/bin` had when the package was installed, by its name in
    /// lowercase, which the DLLs copied into a build must still have.
    pub sysroot_bin: BTreeMap<String, String>,
}

/// How far [`verify`] went, read by the build while it runs on a thread of its own.
#[derive(Default)]
struct Checked {
    files: AtomicU32,
    total: AtomicU32,
    stop: AtomicBool,
}

/// Stops [`verify`] once the build that waits for it ends, Detener and the timeout included, since a
/// blocking thread outlives the future that started it.
struct StopOnDrop(Arc<Checked>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    name: String,
    url: String,
    sha256: String,
    installed_at: u64,
}

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    name: String,
    number: u32,
    toolchain_file: String,
    sysroot: String,
    tools: Vec<ManifestTool>,
}

#[derive(Deserialize)]
struct ManifestTool {
    name: String,
    sha256: String,
    folder: String,
}

/// Where every piece is kept.
#[must_use]
pub fn dir(root: &Root) -> PathBuf {
    root.toolchain().join("win32")
}

/// Whether every piece this decanter pins is in the library, checked: what the Ports tab shows
/// before a build downloads them.
pub async fn is_installed(root: &Root) -> bool {
    let Some(package) = pins::package() else {
        return false;
    };
    let cache = dir(root);
    for archive in pins::tools().iter().chain([&pins::mingit(), &package]) {
        if installed(&cache, archive).await.is_none() {
            return false;
        }
    }
    true
}

/// `checked` of the installed toolchain's `total` files hashed again; `total` is 0 until their lists
/// are read.
fn verifying(checked: u32, total: u32) -> Progress {
    Progress {
        stage: Stage::Verifying,
        received: u64::from(checked),
        total: Some(u64::from(total)),
        item: None,
    }
}

/// Every piece in place, downloading and extracting what is missing, then the package's manifest
/// checked against the tools pinned here.
///
/// # Errors
/// When the package isn't pinned yet, or a piece can't be downloaded, doesn't match its pin, or
/// holds anything its checks refuse.
pub async fn ensure(root: &Root, on_progress: &mut dyn FnMut(Progress)) -> Result<Tools, Error> {
    let Some(package) = pins::package() else {
        return Err(Error::NotPinned);
    };
    let cache = dir(root);
    let [llvm_mingw, cmake, ninja] = pins::tools();
    let mingit = pins::mingit();
    let mut folders = Vec::with_capacity(5);
    for archive in [&llvm_mingw, &cmake, &ninja, &mingit, &package] {
        match install(&cache, archive, on_progress).await {
            Ok(folder) => folders.push(folder),
            Err(err) => return Err(err),
        }
    }
    let [llvm_mingw_dir, cmake_dir, ninja_dir, mingit_dir, package_dir] = match <[PathBuf; 5]>::try_from(folders) {
        Ok(folders) => folders,
        Err(_) => return Err(Error::Manifest("a toolchain piece went missing".to_owned())),
    };
    let checked = Arc::new(Checked::default());
    let _stop = StopOnDrop(Arc::clone(&checked));
    let mut task = {
        let (cache, package_dir, checked) = (cache.clone(), package_dir.clone(), Arc::clone(&checked));
        tokio::task::spawn_blocking(move || {
            // The package last, since its list is the one kept for `sysroot_bin`.
            let pieces = [&llvm_mingw, &cmake, &ninja, &mingit, &package];
            let mut lists = Vec::with_capacity(pieces.len());
            for archive in pieces {
                match expected(&cache, archive) {
                    Ok(list) => lists.push(list),
                    Err(err) => return Err(err),
                }
            }
            let total = lists.iter().map(BTreeMap::len).sum::<usize>();
            checked
                .total
                .store(u32::try_from(total).unwrap_or(u32::MAX), Ordering::Relaxed);
            let mut files = BTreeMap::new();
            for (archive, list) in pieces.into_iter().zip(lists) {
                match verify(&cache, archive, list, &checked) {
                    Ok(verified) => files = verified,
                    Err(err) => return Err(err),
                }
            }
            check_manifest(&package_dir).map(|()| files)
        })
    };
    let started = Instant::now();
    let mut logged = started;
    tracing::info!("checking the Windows toolchain's files before the build");
    on_progress(verifying(0, 0));
    let outcome = loop {
        if let Ok(result) = tokio::time::timeout(VERIFY_TICK, &mut task).await {
            break result;
        }
        let (files, total) = (
            checked.files.load(Ordering::Relaxed),
            checked.total.load(Ordering::Relaxed),
        );
        if started.elapsed() > VERIFY_TIMEOUT {
            tracing::warn!(
                files,
                total,
                "checking the Windows toolchain's files ran past its limit"
            );
            return Err(Error::TimedOut {
                step: "toolchain check",
                seconds: VERIFY_TIMEOUT.as_secs(),
            });
        }
        if logged.elapsed() >= VERIFY_LOG_EVERY {
            logged = Instant::now();
            tracing::info!(
                files,
                total,
                seconds = started.elapsed().as_secs(),
                "still checking the Windows toolchain's files"
            );
        }
        on_progress(verifying(files, total));
    };
    tracing::info!(
        files = checked.files.load(Ordering::Relaxed),
        ms = started.elapsed().as_millis(),
        "Windows toolchain files checked"
    );
    let package_files = match outcome {
        Ok(Ok(files)) => files,
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(io::Error::other(err).into()),
    };
    let sysroot_bin = package_files
        .into_iter()
        .filter_map(|(path, sha256)| {
            let name = path.strip_prefix("clang64/bin/")?;
            (!name.contains('/')).then(|| (name.to_ascii_lowercase(), sha256))
        })
        .collect();
    Ok(Tools {
        git: mingit_git(&mingit_dir),
        cmake: cmake_dir.join("bin").join("cmake.exe"),
        ninja: ninja_dir.join("ninja.exe"),
        llvm_mingw: llvm_mingw_dir,
        package: package_dir,
        sysroot_bin,
    })
}

/// The git inside MinGit's `folder`: the launcher that puts MinGit's own folders first on `PATH`.
pub(super) fn mingit_git(folder: &Path) -> PathBuf {
    folder.join("cmd").join("git.exe")
}

/// `archive`'s folder in `cache`, installing it first unless its record names the same SHA-256.
/// Nothing reaches the folder unless every check passed.
///
/// # Errors
/// When it can't be downloaded, doesn't match its pin, or holds anything its checks refuse.
pub async fn install(cache: &Path, archive: &Archive, on_progress: &mut dyn FnMut(Progress)) -> Result<PathBuf, Error> {
    if let Some(folder) = installed(cache, archive).await {
        return Ok(folder);
    }
    if !pins::is_sha256(&archive.sha256) {
        return Err(Error::NotPinned);
    }
    if let Err(err) = tokio::fs::create_dir_all(cache).await {
        return Err(err.into());
    }
    let pid = std::process::id();
    let key = &archive.key;
    // One install of a piece at a time, across decanter processes: the second finds it installed.
    let lock_path = cache.join(format!(".{key}.lock"));
    let lock = match tokio::task::spawn_blocking({
        let (cache, key, lock_path) = (cache.to_owned(), key.clone(), lock_path.clone());
        move || {
            let lock = package::lock_install(&lock_path);
            if lock.is_ok() {
                sweep_stale(&cache, &key, pid);
            }
            lock
        }
    })
    .await
    {
        Ok(Ok(lock)) => lock,
        Ok(Err(err)) => return Err(err.into()),
        Err(err) => return Err(io::Error::other(err).into()),
    };
    if let Some(folder) = installed(cache, archive).await {
        return Ok(folder);
    }
    let zip = cache.join(format!(".{key}.{pid}.zip"));
    let staging = cache.join(format!(".{key}.{pid}"));
    let leftovers = super::Leftovers::new(vec![zip.clone(), staging.clone()]);
    tracing::info!(
        name = archive.name,
        url = archive.url,
        "downloading a Windows toolchain piece"
    );
    let name = archive.name.clone();
    let downloaded = download::download(
        &archive.url,
        &zip,
        &archive.sha256,
        archive.size,
        package::github_only(),
        |received, total| {
            on_progress(Progress {
                stage: Stage::Downloading,
                received,
                total,
                item: Some(name.clone()),
            });
        },
    )
    .await;
    let result = match downloaded {
        Ok(()) => {
            on_progress(Progress {
                stage: Stage::Unpacking,
                received: 0,
                total: None,
                item: Some(archive.name.clone()),
            });
            unpack(cache, archive, &zip, &staging).await
        }
        Err(err) => Err(err.into()),
    };
    drop(leftovers);
    drop(lock);
    match &result {
        Ok(folder) => tracing::info!(name = archive.name, dir = %folder.display(), "Windows toolchain piece installed"),
        Err(err) => tracing::warn!(name = archive.name, %err, "Windows toolchain piece not installed"),
    }
    result
}

/// Removes what installs of `key` by other processes left in `cache` (`.<key>.<pid>` and its `.zip`,
/// hundreds of megabytes for the package): this one holds the key's lock, so none of them runs.
/// Best effort, said in the log. Blocking file I/O.
fn sweep_stale(cache: &Path, key: &str, pid: u32) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    let prefix = format!(".{key}.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        let (owner, is_zip) = rest.strip_suffix(".zip").map_or((rest, false), |owner| (owner, true));
        if owner.parse::<u32>().is_ok_and(|owner| owner != pid) {
            let removed = if is_zip {
                std::fs::remove_file(entry.path())
            } else {
                std::fs::remove_dir_all(entry.path())
            };
            match removed {
                Ok(()) => tracing::info!(key, owner, "interrupted toolchain install removed"),
                Err(err) => tracing::warn!(key, owner, %err, "could not remove an interrupted toolchain install"),
            }
        }
    }
}

async fn unpack(cache: &Path, archive: &Archive, zip: &Path, staging: &Path) -> Result<PathBuf, Error> {
    let top = if archive.folder.is_empty() {
        archive::Top::Flat
    } else {
        archive::Top::Folder(&archive.folder)
    };
    let unpacked = match archive::unpack(zip, staging, top, archive.names, archive.limits).await {
        Ok(unpacked) => unpacked,
        Err(err) => return Err(err.into()),
    };
    if let Some(missing) = archive
        .required
        .iter()
        .find(|required| !unpacked.files.contains_key(required.as_str()))
    {
        return Err(archive::Error::Layout(format!("no {missing}")).into());
    }
    let list = {
        let (dir, files) = (unpacked.dir.clone(), unpacked.files);
        tokio::task::spawn_blocking(move || hash_list(&dir, files.keys())).await
    };
    let list = match list {
        Ok(Ok(list)) => list,
        Ok(Err(err)) => return Err(err.into()),
        Err(err) => return Err(io::Error::other(err).into()),
    };
    let target = cache.join(&archive.key);
    let record = cache.join(format!("{}.json", archive.key));
    // Only a folder without a valid record is replaced (an older pin's, or an interrupted install's),
    // and its record goes first, so it never counts while it is replaced.
    let _ = tokio::fs::remove_file(&record).await;
    let _ = tokio::fs::remove_dir_all(&target).await;
    if let Err(err) = tokio::fs::rename(&unpacked.dir, &target).await {
        return Err(err.into());
    }
    let list_path = list_path(cache, archive);
    match tokio::task::spawn_blocking(move || crate::files::write_atomic(&list_path, list.as_bytes())).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err.into()),
        Err(err) => return Err(io::Error::other(err).into()),
    }
    let text = match serde_json::to_vec_pretty(&Record {
        name: archive.name.clone(),
        url: archive.url.clone(),
        sha256: archive.sha256.clone(),
        installed_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    }) {
        Ok(text) => text,
        Err(err) => return Err(io::Error::other(err).into()),
    };
    match tokio::task::spawn_blocking(move || crate::files::write_atomic(&record, &text)).await {
        Ok(Ok(())) => Ok(target),
        Ok(Err(err)) => Err(err.into()),
        Err(err) => Err(io::Error::other(err).into()),
    }
}

/// `archive`'s folder when its record names `archive`'s SHA-256 and its required files and its list
/// of hashes are files of their own; [`verify`] checks the rest before each build.
pub async fn installed(cache: &Path, archive: &Archive) -> Option<PathBuf> {
    let path = cache.join(format!("{}.json", archive.key));
    let record = match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_RECORD_BYTES => tokio::fs::read(&path).await.ok()?,
        Ok(_) | Err(_) => return None,
    };
    let record: Record = match serde_json::from_slice(&record) {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "unreadable record of a Windows toolchain piece");
            return None;
        }
    };
    if record.sha256 != archive.sha256 {
        tracing::info!(name = archive.name, "Windows toolchain piece recorded for another pin");
        return None;
    }
    let folder = cache.join(&archive.key);
    if !tokio::fs::symlink_metadata(list_path(cache, archive))
        .await
        .is_ok_and(|meta| meta.is_file())
    {
        return None;
    }
    for required in &archive.required {
        if !tokio::fs::symlink_metadata(folder.join(required))
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return None;
        }
    }
    Some(folder)
}

fn list_path(cache: &Path, archive: &Archive) -> PathBuf {
    cache.join(format!("{}.files", archive.key))
}

/// `<sha256>  <path>` per file of `dir`, as `sha256sum` writes them. Blocking file I/O.
fn hash_list<'path>(dir: &Path, files: impl Iterator<Item = &'path String>) -> io::Result<String> {
    let mut list = String::new();
    for file in files {
        let sha256 = match sha256_of(&dir.join(file)) {
            Ok(sha256) => sha256,
            Err(err) => return Err(err),
        };
        let _ = writeln!(list, "{sha256}  {file}");
    }
    Ok(list)
}

/// Blocking file I/O.
pub(super) fn sha256_of(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(crate::download::hex_digest(&hasher.finalize()))
}

/// `archive`'s list of hashes as its install wrote it, by path. Blocking file I/O.
///
/// # Errors
/// [`Error::Tampered`] when the list is missing, too large or malformed.
fn expected(cache: &Path, archive: &Archive) -> Result<BTreeMap<String, String>, Error> {
    let tampered = || Error::Tampered(format!("{}/list of hashes", archive.key));
    let list = match std::fs::symlink_metadata(list_path(cache, archive)) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_LIST_BYTES => {
            std::fs::read_to_string(list_path(cache, archive))
        }
        Ok(_) | Err(_) => return Err(tampered()),
    };
    let Ok(list) = list else {
        return Err(tampered());
    };
    let mut expected = BTreeMap::new();
    for line in list.lines() {
        match line.split_once("  ") {
            Some((sha256, path)) if pins::is_sha256(sha256) => {
                expected.insert(path.to_owned(), sha256.to_owned());
            }
            Some(_) | None => return Err(tampered()),
        }
    }
    Ok(expected)
}

/// Every file of `archive`'s installed folder exactly as `expected` recorded it at install: the
/// same files, no others and no links, each with its recorded SHA-256. Each file is counted in
/// `checked` as it passes, and `checked.stop` ends the walk early. Blocking file I/O.
///
/// # Errors
/// [`Error::Tampered`] naming the first file that differs.
fn verify(
    cache: &Path,
    archive: &Archive,
    expected: BTreeMap<String, String>,
    checked: &Checked,
) -> Result<BTreeMap<String, String>, Error> {
    let tampered = |what: &str| Error::Tampered(format!("{}/{what}", archive.key));
    let root = cache.join(&archive.key);
    let mut found: usize = 0;
    let mut pending = vec![(root, String::new())];
    while let Some((folder, prefix)) = pending.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => return Err(err.into()),
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(err) => return Err(err.into()),
            };
            if kind.is_dir() {
                pending.push((entry.path(), relative));
                continue;
            }
            if checked.stop.load(Ordering::Relaxed) {
                return Err(io::Error::from(io::ErrorKind::Interrupted).into());
            }
            let Some(sha256) = expected.get(&relative).filter(|_| kind.is_file()) else {
                return Err(tampered(&relative));
            };
            match sha256_of(&entry.path()) {
                Ok(found_sha256) if &found_sha256 == sha256 => {
                    found += 1;
                    checked.files.fetch_add(1, Ordering::Relaxed);
                }
                Ok(_) | Err(_) => return Err(tampered(&relative)),
            }
        }
    }
    if found == expected.len() {
        Ok(expected)
    } else {
        Err(tampered(&format!("{found} of its {} files", expected.len())))
    }
}

/// The package's `manifest.json` names this package and the tools pinned here by the same hashes and
/// folders: a package made for other tools would build with the wrong compiler. Blocking file I/O.
fn check_manifest(package_dir: &Path) -> Result<(), Error> {
    let path = package_dir.join("manifest.json");
    let bytes = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_MANIFEST_BYTES => std::fs::read(&path),
        Ok(_) => return Err(Error::Manifest("manifest.json is not a file under 1 MiB".to_owned())),
        Err(err) => Err(err),
    };
    let manifest: Manifest = match bytes.map(|bytes| serde_json::from_slice(&bytes)) {
        Ok(Ok(manifest)) => manifest,
        Ok(Err(err)) => return Err(Error::Manifest(err.to_string())),
        Err(err) => return Err(err.into()),
    };
    let pin = &pins::TOOLCHAIN;
    let expected = pins::package_folder(pin.number);
    if manifest.schema != 1 || manifest.number != pin.number || manifest.name != expected {
        return Err(Error::Manifest(format!(
            "it describes {} (schema {}, number {}), not {expected}",
            manifest.name, manifest.schema, manifest.number
        )));
    }
    if manifest.toolchain_file != "psport-toolchain.cmake" || manifest.sysroot != "clang64" {
        return Err(Error::Manifest(format!(
            "its toolchain file is {} and its sysroot {}",
            manifest.toolchain_file, manifest.sysroot
        )));
    }
    for (name, tool) in pins::TOOL_NAMES.into_iter().zip(pins::tools()) {
        let matches = manifest
            .tools
            .iter()
            .any(|listed| listed.name == name && listed.sha256 == tool.sha256 && listed.folder == tool.folder);
        if !matches {
            return Err(Error::Manifest(format!("it pins another {name} than this decanter")));
        }
    }
    Ok(())
}

/// Every piece and its record, so the next build downloads them again.
///
/// # Errors
/// When the folder can't be removed; a missing one is not an error.
pub async fn remove(root: &Root) -> io::Result<()> {
    match tokio::fs::remove_dir_all(dir(root)).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
