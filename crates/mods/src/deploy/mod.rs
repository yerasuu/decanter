//! A Steam game's mods, copied into its install (decision #1725), since Steam starts the game and no
//! play tree can stand in for the install. Before a mod's file replaces one of the install's, that
//! file is saved aside in `deployed/backups/`, named by its SHA-256, and `deployed/ledger.toml`
//! lists every file decanter placed; turning mods off puts the saved files back and deletes what
//! mods added. Of two enabled mods with a path the later one is copied, the saved file staying the
//! install's own, so whichever leaves, the path goes to the next one or back to the game.
//!
//! Each pass first settles the ledger against the install by content: a file of decanter's that
//! is gone, or that Steam put the game's own back over (a verify), is copied again; one that is
//! anything else (an update, the game rewriting it) is left as it is and its mods need attention,
//! its saved copy dropped since the install's file is now the one to keep. Nothing is written
//! outside the install and the game's mods folder, no link in the install is followed or written
//! through, and a file is only replaced by renaming a finished copy over it, so a pass cut short
//! leaves every file whole and a ledger the next pass finishes or undoes from.

mod ledger;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::base::sha256_hex;
use crate::game::GameMods;
use crate::import::hex;
use crate::manifest::{FILES, ModFile, ModManifest};
use crate::merge::{Owner, Wanted, merge_over};
use crate::path::{ModId, ModPath};
use crate::set::ModSet;
use crate::tree::check_inside;
use crate::{Error, FREE_SPACE_MARGIN, Result};
use ledger::{Ledger, Placed, Stamp};

const DEPLOYED: &str = "deployed";
const LEDGER: &str = "ledger.toml";
const BACKUPS: &str = "backups";
/// The one partial copy a pass has in a folder of the install at a time, renamed over its target.
const PARTIAL: &str = ".decanter-deploy.tmp";
const COPY_BUFFER: usize = 1 << 20;

/// Whether a pass may put this account's mods in place of another account's, since the install is
/// one for every profile that shows the Steam game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Takeover {
    /// The Mods tab's own passes, so that opening it in one profile never undoes another's mods.
    Never,
    /// A reapply the user asked for.
    Allowed,
}

/// What one pass over a Steam game's install found and did.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Deployment {
    /// The account whose mods the install holds, when it is not the one asked for and the pass
    /// left them.
    pub other_account: Option<String>,
    /// Files of mods in the install after the pass.
    pub placed: usize,
    /// Files copied again since they had gone or Steam had put the game's own back over them.
    pub reapplied: usize,
    /// The install's own files put back, and files mods had added deleted.
    pub restored: usize,
    /// Files decanter placed that something else changed, left as they are; their mods need
    /// attention.
    pub changed: usize,
    /// Paths a mod has that the install has as a link or a folder, or under one, left out.
    pub skipped: usize,
}

fn ledger_path(game: &GameMods) -> PathBuf {
    game.dir.join(DEPLOYED).join(LEDGER)
}

fn backups_dir(game: &GameMods) -> PathBuf {
    game.dir.join(DEPLOYED).join(BACKUPS)
}

/// The folded paths mods added to the install, which are not the game's files.
pub(crate) fn added(game: &GameMods) -> Result<HashSet<String>> {
    Ok(added_in(&Ledger::read(&ledger_path(game))?))
}

fn added_in(ledger: &Ledger) -> HashSet<String> {
    ledger
        .files
        .iter()
        .filter(|&(_, placed)| placed.base.is_none())
        .map(|(path, _)| path.folded())
        .collect()
}

/// Whether the install still holds files decanter placed, whose saved originals the mods folder
/// keeps.
pub(crate) fn holds_files(game: &GameMods) -> Result<bool> {
    Ok(!Ledger::read(&ledger_path(game))?.files.is_empty())
}

pub(crate) fn run(game: &GameMods, account: &str, takeover: Takeover) -> Result<Deployment> {
    let install = real_install(&game.install)?;
    let mut ledger = Ledger::read(&ledger_path(game))?;
    let other = ledger
        .account
        .clone()
        .filter(|other| other != account && !ledger.files.is_empty());
    if let Some(other) = other
        && takeover == Takeover::Never
    {
        tracing::info!(
            account,
            other,
            files = ledger.files.len(),
            "another account's mods are in the steam install, left as they are"
        );
        return Ok(Deployment {
            placed: ledger.files.len(),
            other_account: Some(other),
            ..Deployment::default()
        });
    }
    let set_path = game.set_path(account)?;
    let mut set = ModSet::read(&set_path)?;
    let enabled = if set.switched_off {
        Vec::new()
    } else {
        game.manifests(set.enabled())?
    };
    let mut report = Deployment::default();
    let settled = settle(&install, &mut ledger)?;
    report.changed = settled.changed.len();
    if !settled.changed.is_empty() {
        let merged = merge_over(game, &enabled, &added_in(&ledger))?;
        mark_attention(&mut set, &enabled, &merged, &settled.changed);
        set.write(&set_path)?;
    }
    let mods: Vec<ModManifest> = enabled
        .into_iter()
        .filter(|manifest| !set.attention.contains_key(&manifest.id))
        .collect();
    ledger.account = Some(account.to_owned());
    let outcome = apply(game, &install, &mut ledger, &mods, &settled, &mut report);
    ledger.account = (!ledger.files.is_empty()).then(|| account.to_owned());
    // Written also after a failure, so that every file placed so far stays known as decanter's.
    finish(game, &ledger)?;
    outcome?;
    report.placed = ledger.files.len();
    tracing::info!(
        account,
        install = %install.display(),
        mods = mods.len(),
        placed = report.placed,
        reapplied = report.reapplied,
        restored = report.restored,
        changed = report.changed,
        skipped = report.skipped,
        "mods copied into the steam install"
    );
    Ok(report)
}

fn real_install(install: &Path) -> Result<PathBuf> {
    let real = install.canonicalize().map_err(Error::io(install))?;
    if !real.is_dir() {
        return Err(Error::NotAnInstall(install.display().to_string()));
    }
    Ok(real)
}

/// Every enabled mod with a path something else changed needs attention, not only the one whose
/// file was there, since the next one would otherwise copy over the change at once.
fn mark_attention(set: &mut ModSet, enabled: &[ModManifest], merged: &BTreeMap<String, Wanted>, changed: &[String]) {
    for wanted in changed.iter().filter_map(|key| merged.get(key)) {
        for provider in &wanted.providers {
            if let Owner::Mod(index) = *provider
                && let Some(manifest) = enabled.get(index)
            {
                let paths = set.attention.entry(manifest.id.clone()).or_default();
                if !paths.contains(&wanted.path) {
                    paths.push(wanted.path.clone());
                }
            }
        }
    }
}

#[derive(Default)]
struct Settled {
    /// Folded paths something else changed, whose mods need attention.
    changed: Vec<String>,
    /// Folded paths whose placed file had gone or been put back by Steam.
    reverted: HashSet<String>,
}

enum Seen {
    Ours(Stamp),
    /// Still the file a cut-short pass was replacing.
    Previous(Stamp),
    /// The install's own file, as saved aside.
    Base,
    /// Gone, where the install had no file of its own.
    Absent,
    Changed,
}

/// Leaves in the ledger only files that are decanter's on disk, each with its stamp.
fn settle(install: &Path, ledger: &mut Ledger) -> Result<Settled> {
    let mut settled = Settled::default();
    let mut swept = HashSet::new();
    for (path, mut placed) in core::mem::take(&mut ledger.files) {
        let seen = seen(install, &path, &placed)?;
        if !matches!(seen, Seen::Changed)
            && let Some(parent) = path.under(install).parent()
            && swept.insert(parent.to_path_buf())
        {
            remove_partial(&parent.join(PARTIAL))?;
        }
        match seen {
            Seen::Ours(stamp) => {
                placed.stamp = Some(stamp);
                placed.previous = None;
                ledger.files.insert(path, placed);
            }
            Seen::Previous(stamp) => {
                if let Some(previous) = placed.previous.take() {
                    placed.hash = previous;
                }
                placed.stamp = Some(stamp);
                ledger.files.insert(path, placed);
            }
            Seen::Base | Seen::Absent => {
                if !placed.is_leaving && placed.stamp.is_some() {
                    tracing::info!(path = %path, mod_id = %placed.owner, "a mod's file is no longer in the steam install, copied again");
                    settled.reverted.insert(path.folded());
                }
            }
            Seen::Changed => {
                tracing::warn!(path = %path, mod_id = %placed.owner, leaving = placed.is_leaving, "a file decanter placed in the steam install was changed by something else, left as it is");
                if !placed.is_leaving {
                    settled.changed.push(path.folded());
                }
            }
        }
    }
    Ok(settled)
}

fn seen(install: &Path, path: &ModPath, placed: &Placed) -> Result<Seen> {
    let gone = if placed.base.is_none() {
        Seen::Absent
    } else {
        Seen::Changed
    };
    match missing_parents(install, path)? {
        None => return Ok(Seen::Changed),
        Some(missing) if !missing.is_empty() => return Ok(gone),
        Some(_) => {}
    }
    let on_disk = path.under(install);
    let metadata = match on_disk.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(gone),
        Err(err) => return Err(Error::io(&on_disk)(err)),
    };
    if !metadata.is_file() {
        return Ok(Seen::Changed);
    }
    let stamp = Stamp::of(&metadata);
    if placed.stamp == Some(stamp) {
        return Ok(Seen::Ours(stamp));
    }
    let hash = sha256_hex(&on_disk)?;
    Ok(if hash == placed.hash {
        Seen::Ours(stamp)
    } else if placed.previous.as_ref() == Some(&hash) {
        Seen::Previous(stamp)
    } else if placed.base.as_ref() == Some(&hash) {
        Seen::Base
    } else {
        Seen::Changed
    })
}

/// The folders of `path` the install lacks, outermost first; `None` when one of them is a file or
/// a link, which would lead a copy somewhere else.
fn missing_parents(install: &Path, path: &ModPath) -> Result<Option<Vec<ModPath>>> {
    let mut missing = Vec::new();
    let mut prefix = String::new();
    let mut components = path.components().peekable();
    while let Some(name) = components.next() {
        if components.peek().is_none() {
            break;
        }
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(name);
        let dir = ModPath::parse(&prefix)?;
        if !missing.is_empty() {
            missing.push(dir);
            continue;
        }
        let on_disk = dir.under(install);
        match on_disk.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => missing.push(dir),
            Err(err) => return Err(Error::io(&on_disk)(err)),
        }
    }
    Ok(Some(missing))
}

/// A mod's file to copy over a path that holds one of decanter's or nothing yet.
struct Transfer {
    path: ModPath,
    source: PathBuf,
    hash: String,
    executable: bool,
    size: u64,
}

/// A path no file of decanter's is at yet.
struct Fresh<'mods> {
    path: ModPath,
    owner: ModId,
    source: PathBuf,
    file: &'mods ModFile,
    missing: Vec<ModPath>,
    /// The install's own file there, saved aside before the copy.
    base: Option<fs::Metadata>,
}

/// Plans the pass, records it, then carries it out.
fn apply(
    game: &GameMods,
    install: &Path,
    ledger: &mut Ledger,
    mods: &[ModManifest],
    settled: &Settled,
    report: &mut Deployment,
) -> Result<()> {
    if mods.is_empty() && ledger.files.is_empty() {
        return Ok(());
    }
    let wanted = merge_over(game, mods, &added_in(ledger))?;
    let Plan {
        mut copies,
        fresh,
        restores,
    } = plan(game, ledger, mods, &wanted)?;
    let mut ready = Vec::with_capacity(fresh.len());
    for entry in fresh {
        let Some(missing) = missing_parents(install, &entry.path)? else {
            tracing::warn!(path = %entry.path, mod_id = %entry.owner, "mod file left out, the steam install has a file or a link where its folder goes");
            report.skipped = report.skipped.saturating_add(1);
            continue;
        };
        let on_disk = entry.path.under(install);
        let base = match on_disk.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => Some(metadata),
            Ok(_) => {
                tracing::warn!(path = %entry.path, mod_id = %entry.owner, "mod file left out, the steam install has a link or a folder there");
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
            Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => None,
            Err(err) => return Err(Error::io(&on_disk)(err)),
        };
        ready.push(Fresh { missing, base, ..entry });
    }
    let copied = copies
        .iter()
        .map(|copy| copy.size)
        .chain(ready.iter().map(|entry| entry.file.size))
        .fold(0_u64, u64::saturating_add);
    check_space(install, copied)?;
    let saved = ready
        .iter()
        .filter_map(|entry| entry.base.as_ref().map(fs::Metadata::len))
        .fold(0_u64, u64::saturating_add);
    let backups = backups_dir(game);
    if saved > 0 {
        fs::create_dir_all(&backups).map_err(Error::io(&backups))?;
        check_space(&backups, saved)?;
    }
    for entry in ready {
        let (base, base_executable) = match entry.base.as_ref() {
            Some(metadata) => (
                Some(save_aside(&entry.path.under(install), &backups)?),
                is_executable(metadata),
            ),
            None => (None, false),
        };
        ledger.dirs.extend(entry.missing);
        ledger.files.insert(
            entry.path.clone(),
            Placed {
                owner: entry.owner,
                hash: entry.file.sha256.clone(),
                base,
                base_executable,
                stamp: None,
                previous: None,
                is_leaving: false,
            },
        );
        copies.push(Transfer {
            path: entry.path,
            source: entry.source,
            hash: entry.file.sha256.clone(),
            executable: entry.file.executable || base_executable,
            size: entry.file.size,
        });
    }
    if copies.is_empty() && restores.is_empty() {
        return Ok(());
    }
    ledger.write(&ledger_path(game))?;
    for path in &restores {
        restore(install, &backups, ledger, path)?;
        report.restored = report.restored.saturating_add(1);
    }
    for copy in &copies {
        place(install, ledger, copy)?;
        if settled.reverted.contains(&copy.path.folded()) {
            report.reapplied = report.reapplied.saturating_add(1);
        }
    }
    remove_empty_dirs(install, ledger);
    Ok(())
}

struct Plan<'mods> {
    /// Files of decanter's whose path another file now wins.
    copies: Vec<Transfer>,
    /// Paths a mod wins that hold nothing of decanter's yet.
    fresh: Vec<Fresh<'mods>>,
    /// Files of decanter's no enabled mod wants any more.
    restores: Vec<ModPath>,
}

fn plan<'mods>(
    game: &GameMods,
    ledger: &mut Ledger,
    mods: &'mods [ModManifest],
    wanted: &BTreeMap<String, Wanted>,
) -> Result<Plan<'mods>> {
    let mut by_key: HashMap<String, ModPath> = ledger.files.keys().map(|path| (path.folded(), path.clone())).collect();
    let mut sources = HashMap::new();
    let mut roots = Vec::with_capacity(mods.len());
    for manifest in mods {
        let files_dir = game.mod_dir(&manifest.id).join(FILES);
        roots.push(files_dir.canonicalize().map_err(Error::io(&files_dir))?);
        for file in &manifest.files {
            sources.insert(file.path.under(&files_dir), file);
        }
    }
    let mut copies = Vec::new();
    let mut fresh = Vec::new();
    for (key, entry) in wanted {
        let Some(&Owner::Mod(index)) = entry.providers.last() else {
            continue;
        };
        let (Some(manifest), Some(root), Some(&file)) = (mods.get(index), roots.get(index), sources.get(&entry.source))
        else {
            return Err(Error::Escapes(entry.source.clone()));
        };
        check_inside(&entry.source, root)?;
        if let Some(path) = by_key.remove(key)
            && let Some(placed) = ledger.files.get_mut(&path)
        {
            placed.is_leaving = false;
            placed.owner = manifest.id.clone();
            if placed.hash != file.sha256 {
                placed.previous = Some(core::mem::replace(&mut placed.hash, file.sha256.clone()));
                placed.stamp = None;
                copies.push(Transfer {
                    executable: file.executable || placed.base_executable,
                    path,
                    source: entry.source.clone(),
                    hash: file.sha256.clone(),
                    size: file.size,
                });
            }
            continue;
        }
        fresh.push(Fresh {
            path: entry.path.clone(),
            owner: manifest.id.clone(),
            source: entry.source.clone(),
            file,
            missing: Vec::new(),
            base: None,
        });
    }
    let restores: Vec<ModPath> = by_key.into_values().collect();
    for path in &restores {
        if let Some(placed) = ledger.files.get_mut(path) {
            placed.is_leaving = true;
        }
    }
    Ok(Plan {
        copies,
        fresh,
        restores,
    })
}

fn check_space(dir: &Path, needed: u64) -> Result<()> {
    if needed == 0 {
        return Ok(());
    }
    match decanter_core::disk_usage(dir) {
        Some((free, _)) if needed.saturating_add(FREE_SPACE_MARGIN) > free => Err(Error::NoSpace { needed, free }),
        Some(_) => Ok(()),
        None => {
            tracing::warn!(path = %dir.display(), needed, "free space unknown, copying the mods anyway");
            Ok(())
        }
    }
}

/// Copies the install's file at `on_disk` into `backups` under its hash, which it returns.
fn save_aside(on_disk: &Path, backups: &Path) -> Result<String> {
    let partial = backups.join(format!(".{}.tmp", std::process::id()));
    remove_partial(&partial)?;
    let hash = copy_hashed(on_disk, &partial, false)?;
    let kept = backups.join(&hash);
    let outcome = if kept.is_file() {
        fs::remove_file(&partial).map_err(Error::io(&partial))
    } else {
        fs::rename(&partial, &kept).map_err(Error::io(&kept))
    };
    outcome?;
    Ok(hash)
}

/// Puts the install's own file back at `path`, or deletes what a mod added there.
fn restore(install: &Path, backups: &Path, ledger: &mut Ledger, path: &ModPath) -> Result<()> {
    let Some(placed) = ledger.files.get(path) else {
        return Ok(());
    };
    let on_disk = path.under(install);
    match placed.base.as_ref() {
        Some(base) => replace_with(&backups.join(base), &on_disk, base, placed.base_executable)?,
        None => match fs::remove_file(&on_disk) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::io(&on_disk)(err)),
        },
    }
    ledger.files.remove(path);
    Ok(())
}

fn place(install: &Path, ledger: &mut Ledger, copy: &Transfer) -> Result<()> {
    let on_disk = copy.path.under(install);
    let missing = missing_parents(install, &copy.path)?.ok_or_else(|| Error::Escapes(on_disk.clone()))?;
    for dir in missing {
        let folder = dir.under(install);
        #[expect(
            clippy::create_dir,
            reason = "one level at a time, since create_dir_all would go through a link to a folder"
        )]
        let created = fs::create_dir(&folder);
        match created {
            Ok(()) => {}
            Err(err)
                if err.kind() == io::ErrorKind::AlreadyExists
                    && folder.symlink_metadata().is_ok_and(|metadata| metadata.is_dir()) => {}
            Err(err) => return Err(Error::io(&folder)(err)),
        }
        ledger.dirs.insert(dir);
    }
    replace_with(&copy.source, &on_disk, &copy.hash, copy.executable)?;
    let stamp = Stamp::of(&on_disk.symlink_metadata().map_err(Error::io(&on_disk))?);
    if let Some(placed) = ledger.files.get_mut(&copy.path) {
        placed.stamp = Some(stamp);
        placed.previous = None;
    }
    Ok(())
}

/// Copies `source` beside `target` and renames the copy over it once it holds `expected`, so that
/// `target` is either file whole whenever the pass stops.
fn replace_with(source: &Path, target: &Path, expected: &str, executable: bool) -> Result<()> {
    let partial = target
        .parent()
        .map(|parent| parent.join(PARTIAL))
        .ok_or_else(|| Error::Escapes(target.to_path_buf()))?;
    remove_partial(&partial)?;
    let hash = copy_hashed(source, &partial, executable)?;
    if hash != expected {
        remove_partial(&partial)?;
        return Err(Error::malformed(
            source,
            &"is not the file decanter recorded; check the mod's files, or verify the game in Steam",
        ));
    }
    fs::rename(&partial, target).map_err(|err| {
        if let Err(cleanup) = fs::remove_file(&partial) {
            tracing::warn!(path = %partial.display(), %cleanup, "could not remove a partial copy");
        }
        Error::io(target)(err)
    })
}

/// A partial copy a pass cut short left; never a folder, which decanter does not make there.
fn remove_partial(partial: &Path) -> Result<()> {
    match partial.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => Err(Error::NotAFile(partial.to_path_buf())),
        Ok(_) => fs::remove_file(partial).map_err(Error::io(partial)),
        Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(()),
        Err(err) => Err(Error::io(partial)(err)),
    }
}

/// Writes a new file at `destination`, synced before it is renamed anywhere, and returns the hash
/// of what it copied.
fn copy_hashed(source: &Path, destination: &Path, executable: bool) -> Result<String> {
    let mut reader = fs::File::open(source).map_err(Error::io(source))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(if executable { 0o755 } else { 0o644 })
    };
    #[cfg(not(unix))]
    let _ = executable;
    let mut writer = options.open(destination).map_err(Error::io(destination))?;
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut hasher = Sha256::new();
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(Error::io(source)(err)),
        };
        let chunk = buffer.get(..count).unwrap_or_default();
        hasher.update(chunk);
        writer.write_all(chunk).map_err(Error::io(destination))?;
    }
    writer.sync_all().map_err(Error::io(destination))?;
    Ok(hex(&hasher.finalize()))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
const fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

/// The folders decanter made that no file of decanter's is in any more, deepest first; one the
/// game has written into stays, listed, until it is empty.
fn remove_empty_dirs(install: &Path, ledger: &mut Ledger) {
    let mut in_use = HashSet::new();
    for path in ledger.files.keys() {
        let mut prefix = String::new();
        let mut components = path.components().peekable();
        while let Some(name) = components.next() {
            if components.peek().is_none() {
                break;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            in_use.insert(prefix.clone());
        }
    }
    let unused: Vec<ModPath> = ledger
        .dirs
        .iter()
        .rev()
        .filter(|dir| !in_use.contains(dir.as_str()))
        .cloned()
        .collect();
    for dir in unused {
        let folder = dir.under(install);
        match fs::remove_dir(&folder) {
            Ok(()) => {
                ledger.dirs.remove(&dir);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                ledger.dirs.remove(&dir);
            }
            Err(err) => tracing::debug!(path = %folder.display(), %err, "folder decanter made kept, it is not empty"),
        }
    }
}

/// Writes the ledger, or removes it with the backups once the install holds nothing of decanter's,
/// and deletes every saved file no entry names.
fn finish(game: &GameMods, ledger: &Ledger) -> Result<()> {
    let path = ledger_path(game);
    if ledger.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::io(&path)(err)),
        }
    } else {
        ledger.write(&path)?;
    }
    let backups = backups_dir(game);
    let entries = match fs::read_dir(&backups) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(Error::io(&backups)(err)),
    };
    let named: HashSet<&str> = ledger
        .files
        .values()
        .filter_map(|placed| placed.base.as_deref())
        .collect();
    for entry in entries {
        let entry = entry.map_err(Error::io(&backups))?;
        let is_named = entry.file_name().to_str().is_some_and(|name| named.contains(name));
        if !is_named && let Err(err) = fs::remove_file(entry.path()) {
            tracing::warn!(path = %entry.path().display(), %err, "could not delete a saved file no longer needed");
        }
    }
    if ledger.is_empty() {
        for dir in [backups, game.dir.join(DEPLOYED)] {
            if let Err(err) = fs::remove_dir(&dir) {
                tracing::debug!(path = %dir.display(), %err, "deploy folder kept");
            }
        }
    }
    Ok(())
}
