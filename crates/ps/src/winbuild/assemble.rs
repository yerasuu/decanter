//! A built tree made into a folder `psport.exe` runs from, the same shape as the package psport's CI
//! publishes ([`crate::package`]): the three programs and `libpsport-gpu.dll` from the build, the
//! DLLs they load from the toolchain package's `clang64/bin` (the closure of their PE import tables,
//! [`super::pe`]), a `BUILD-INFO.txt` listing those DLLs, and the checkout's `patches/`, which
//! `psport.exe` reads a title's patch file from, exiting when it is missing. The folder must then
//! pass the package's own checks, so a built folder is accepted exactly as a downloaded one. Nothing
//! in it runs here.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::{Error, Head};
use crate::archive::{self, Names};
use crate::bbport::direct::{Folder, GPU_LIBRARY};
use crate::package::{self, BUILD_INFO, PROGRAMS};

/// The loader psport ships nowhere: the GPU driver's own, as psport's `tools/win32-bundle.sh` leaves
/// it.
const SYSTEM_LOADER: &str = "vulkan-1.dll";
/// Any one program or DLL read for its imports.
const MAX_IMAGE_BYTES: u64 = 512 << 20;
/// DLLs in a closure; psport's package carries a few dozen.
const MAX_DLLS: usize = 256;
/// psport's `install_dir` (`src/main/game_info.c`) takes the nearest folder above the executable
/// holding it as the one its titles' patch files are relative to.
const PATCHES: &str = "patches";
/// Files in [`PATCHES`]; psport's checkout holds one.
const MAX_PATCHES: usize = 64;
/// Any one of them.
const MAX_PATCH_BYTES: u64 = 16 << 20;

/// The [`PATCHES`] of the checkout in `sources` copied into `staging`, before [`folder`] fills and
/// checks it. Blocking file I/O.
///
/// # Errors
/// When the checkout's [`PATCHES`] is missing, holds anything but plain files the package's names
/// allow, or can't be copied.
pub fn patches(sources: &Path, staging: &Path) -> Result<(), Error> {
    let from = sources.join(PATCHES);
    let to = staging.join(PATCHES);
    let names = match patch_files(&from) {
        Ok(names) => names,
        Err(err) => return Err(err),
    };
    if let Err(err) = std::fs::create_dir_all(&to) {
        return Err(Error::Output(format!("{}: {err}", to.display())));
    }
    for name in names {
        if let Err(err) = std::fs::copy(from.join(&name), to.join(&name)) {
            return Err(Error::Output(format!("{PATCHES}/{name}: {err}")));
        }
    }
    Ok(())
}

/// `staging` filled from the build in `out` and the DLLs of `sysroot_bin`, each with the SHA-256
/// `expected` holds for its lowercase name, then checked. Blocking file I/O.
///
/// # Errors
/// When a program is missing, a DLL it needs has a name the package refuses or isn't the one
/// installed ([`Error::Tampered`]), or the folder fails the package's checks.
pub fn folder(
    out: &Path,
    sysroot_bin: &Path,
    expected: &BTreeMap<String, String>,
    staging: &Path,
    head: &Head,
    toolchain: &str,
) -> Result<Vec<String>, Error> {
    let programs: Vec<(PathBuf, &str)> = PROGRAMS
        .iter()
        .map(|program| (out.join(program), *program))
        .chain(std::iter::once((out.join("gpu").join(GPU_LIBRARY), GPU_LIBRARY)))
        .collect();
    let available = match dlls_in(sysroot_bin) {
        Ok(available) => available,
        Err(err) => return Err(err),
    };
    let mut pending: Vec<PathBuf> = programs.iter().map(|(path, _)| path.clone()).collect();
    let mut included: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut system: Vec<String> = Vec::new();
    while let Some(image) = pending.pop() {
        let bytes = match read_image(&image) {
            Ok(bytes) => bytes,
            Err(err) => return Err(err),
        };
        let Some(imports) = super::pe::imports(&bytes) else {
            return Err(Error::Output(format!("{} is not a PE image", image.display())));
        };
        for dll in imports {
            let key = dll.to_ascii_lowercase();
            if key == GPU_LIBRARY || included.keys().any(|known| known.eq_ignore_ascii_case(&dll)) {
                continue;
            }
            match available.get(&key) {
                Some((name, path)) if key != SYSTEM_LOADER => {
                    if !archive::valid_component(name, Names::Plain) {
                        return Err(Error::Output(format!("a DLL name the package refuses: {name}")));
                    }
                    if included.len() >= MAX_DLLS {
                        return Err(Error::Output(format!("more than {MAX_DLLS} DLLs")));
                    }
                    included.insert(name.clone(), path.clone());
                    pending.push(path.clone());
                }
                Some(_) | None => {
                    if !system.iter().any(|known| known.eq_ignore_ascii_case(&dll)) {
                        system.push(dll);
                    }
                }
            }
        }
    }
    if let Err(err) = std::fs::create_dir_all(staging) {
        return Err(err.into());
    }
    for (path, name) in &programs {
        if let Err(err) = std::fs::copy(path, staging.join(name)) {
            return Err(Error::Output(format!("{name}: {err}")));
        }
    }
    // Read once, checked against the hash recorded when the package was installed, and written from
    // those same bytes: the build ran with the user's rights and may have replaced it since.
    for (name, path) in &included {
        let bytes = match read_image(path) {
            Ok(bytes) => bytes,
            Err(err) => return Err(err),
        };
        let recorded = expected.get(&name.to_ascii_lowercase());
        if recorded.is_none_or(|recorded| *recorded != format!("{:x}", Sha256::digest(&bytes))) {
            return Err(Error::Tampered(format!("clang64/bin/{name}")));
        }
        if let Err(err) = std::fs::write(staging.join(name), bytes) {
            return Err(Error::Output(format!("{name}: {err}")));
        }
    }
    let mut info = format!(
        "psport {commit} for Windows, built by decanter\nRemote: {remote}\nBranch: {branch}\nToolchain: {toolchain}\nDLLs included ({count}):\n",
        commit = head.commit,
        remote = super::git::REMOTE,
        branch = head.branch,
        count = included.len(),
    );
    for name in included.keys() {
        let _ = writeln!(info, "  {name}");
    }
    let _ = writeln!(info, "System DLLs: {}", system.join(" "));
    if let Err(err) = std::fs::write(staging.join(BUILD_INFO), info) {
        return Err(err.into());
    }
    match check(staging) {
        Ok(()) => Ok(included.into_keys().collect()),
        Err(err) => Err(err),
    }
}

/// The DLLs in `dir` by their lowercased names: plain files only.
fn dlls_in(dir: &Path) -> Result<HashMap<String, (String, PathBuf)>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => return Err(Error::Output(format!("{}: {err}", dir.display()))),
    };
    let mut dlls = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dll = name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll"));
        if is_dll && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            dlls.insert(name.to_ascii_lowercase(), (name, entry.path()));
        }
    }
    Ok(dlls)
}

/// The names in `dir`, a folder of its own holding only plain files of names the package accepts.
fn patch_files(dir: &Path) -> Result<Vec<String>, Error> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(Error::Output(format!("{} is not a folder", dir.display()))),
        Err(err) => return Err(Error::Output(format!("{}: {err}", dir.display()))),
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => return Err(Error::Output(format!("{}: {err}", dir.display()))),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => return Err(Error::Output(format!("{}: {err}", dir.display()))),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_plain =
            std::fs::symlink_metadata(entry.path()).is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_PATCH_BYTES);
        if !is_plain || !archive::valid_component(&name, Names::Plain) {
            return Err(Error::Output(format!(
                "{PATCHES}/{name}, which isn't a plain file under {MAX_PATCH_BYTES} bytes"
            )));
        }
        if names.len() >= MAX_PATCHES {
            return Err(Error::Output(format!("more than {MAX_PATCHES} files in {PATCHES}")));
        }
        names.push(name);
    }
    Ok(names)
}

fn read_image(path: &Path) -> Result<Vec<u8>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_IMAGE_BYTES => {}
        Ok(_) => {
            return Err(Error::Output(format!(
                "{} is not a file under {MAX_IMAGE_BYTES} bytes",
                path.display()
            )));
        }
        Err(err) => return Err(Error::Output(format!("{}: {err}", path.display()))),
    }
    std::fs::read(path).map_err(|err| Error::Output(format!("{}: {err}", path.display())))
}

/// `dir` as the package's checks see an extracted package, then as Jugar sees a folder: plain files
/// only besides [`PATCHES`], which must be there, the programs and DLLs it may hold, and both files
/// `psport.exe` needs to start ([`Folder::try_new`]).
pub fn check(dir: &Path) -> Result<(), Error> {
    if let Err(err) = patch_files(&dir.join(PATCHES)) {
        return Err(err);
    }
    let mut files = BTreeMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => return Err(err.into()),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(err) => return Err(err.into()),
        };
        if name == PATCHES && meta.is_dir() {
            continue;
        }
        if !meta.is_file() || !archive::valid_component(&name, Names::Plain) {
            return Err(Error::Output(format!("{name}, which a psport folder doesn't hold")));
        }
        files.insert(name, meta.len());
    }
    if let Err(err) = package::check_programs(dir, &files) {
        return Err(Error::Package(err));
    }
    match Folder::try_new(dir.to_owned()) {
        Ok(_) => Ok(()),
        Err(err) => Err(Error::Output(err.to_string())),
    }
}
