//! Applying a pack's plan (design, sections 5.3 to 5.7), one entry at a time in the resolved
//! sequence: download into the game's `incoming/`, check against the entry's hashes, read and
//! import through decanter-mods' archive import, then place every present entry in the pack's
//! block and set it on or off. Nothing is extracted here, and the set is written in its final order
//! after every entry, so a cancel or a crash loses at most the entry in flight, whose staging and
//! partial import remove themselves. A failed entry is skipped and flagged: what requires it is
//! blocked and left off, the rest goes on, and running apply again does only what remains.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use decanter_modpack::{Entry, EntryKey, LocalNotes, Pack, Plan, Source, StepState, Track};
use decanter_mods::{ArchivePreview, GameMods, ModId, ModOrigin, OrderChange, Progress};

use crate::i18n::Text;

/// Conflict lines a run's summary keeps; the rest is summed up.
const MAX_CONFLICT_LINES: usize = 50;
const MD5_CHUNK: usize = 1 << 20;
/// The hosts a hand-downloaded file's origin names: the site the user downloads from, since no
/// API was asked for it.
const NEXUS_SITE: &str = "www.nexusmods.com";
const MODIO_SITE: &str = "mod.io";
const THUNDERSTORE_SITE: &str = "thunderstore.io";

/// What runs off the UI, shared with the panel that draws it and cancels it.
#[derive(Default)]
pub(super) struct Work {
    cancelled: AtomicBool,
    pub(super) thunderstore: decanter_thunderstore::Transfer,
    pub(super) nexus: decanter_nexus::Transfer,
    pub(super) progress: Progress,
    /// The step of the plan in flight.
    pub(super) current: AtomicUsize,
}

impl Work {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.thunderstore.cancel();
        self.nexus.cancel();
        self.progress.cancel();
        tracing::info!("mod pack apply cancel asked");
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// What the providers of this apply are, read once when the plan was made.
pub(super) struct Providers {
    pub(super) thunderstore: Option<Thunderstore>,
    pub(super) nexus: Option<Nexus>,
    /// What could not be read of them, shown with every plan made from them.
    pub(super) warnings: Vec<Text>,
}

pub(super) struct Thunderstore {
    pub(super) client: decanter_thunderstore::Client,
    pub(super) listing: Arc<decanter_thunderstore::Listing>,
    pub(super) community: decanter_thunderstore::CommunityId,
}

pub(super) struct Nexus {
    pub(super) client: decanter_nexus::Client,
    pub(super) is_premium: bool,
}

pub(super) enum Job {
    /// Every entry the plan can download.
    Fetch,
    /// The file or folder the user picked for one entry.
    File { key: EntryKey, path: PathBuf },
    /// One Premium download from Nexus, on the user's click.
    NexusDownload { key: EntryKey },
}

/// How a run ended, for the line after it.
#[derive(Default)]
pub(super) struct Summary {
    pub(super) installed: Vec<String>,
    pub(super) failed: Vec<(String, Text)>,
    pub(super) conflicts: Vec<String>,
    pub(super) more_conflicts: usize,
    pub(super) cancelled: bool,
}

pub(super) struct Run {
    pub(super) mods: GameMods,
    pub(super) account: String,
    pub(super) pack: Pack,
    pub(super) notes: LocalNotes,
    pub(super) plan: Plan,
    pub(super) providers: Arc<Providers>,
    pub(super) work: Arc<Work>,
}

enum StepError {
    Cancelled,
    Failed(Text),
}

impl StepError {
    fn failed(err: impl core::fmt::Display) -> Self {
        Self::Failed(Text::verbatim(err))
    }
}

impl From<decanter_mods::Error> for StepError {
    fn from(err: decanter_mods::Error) -> Self {
        if matches!(err, decanter_mods::Error::Cancelled) {
            Self::Cancelled
        } else {
            Self::failed(err)
        }
    }
}

impl From<decanter_thunderstore::Error> for StepError {
    fn from(err: decanter_thunderstore::Error) -> Self {
        if matches!(err, decanter_thunderstore::Error::Cancelled) {
            Self::Cancelled
        } else {
            Self::failed(err)
        }
    }
}

impl From<decanter_nexus::Error> for StepError {
    fn from(err: decanter_nexus::Error) -> Self {
        if matches!(err, decanter_nexus::Error::Cancelled) {
            Self::Cancelled
        } else {
            Self::Failed(crate::nexus::failure_text(&err))
        }
    }
}

struct Imported {
    id: ModId,
    /// The archive was already this mod of the game, so nothing was added.
    is_duplicate: bool,
}

pub(super) async fn execute(run: Run, job: Job) -> Summary {
    let Run {
        mods,
        account,
        pack,
        mut notes,
        mut plan,
        providers,
        work,
    } = run;
    let mut summary = Summary::default();
    let mut queue: Vec<(EntryKey, Option<PathBuf>)> = match job {
        Job::Fetch => plan
            .steps()
            .iter()
            .filter(|step| step.blocked_by.is_none() && matches!(step.state, StepState::Fetch { .. }))
            .map(|step| (step.key.clone(), None))
            .collect(),
        Job::File { key, path } => vec![(key, Some(path))],
        Job::NexusDownload { key } => vec![(key, None)],
    };
    tracing::info!(pack = %pack.id, entries = queue.len(), "mod pack apply started");
    let arranged = settle(&mods, &account, &plan, &queue, &mut notes, &pack.id).await;
    if let Err(text) = arranged {
        summary.failed.push((pack.name.clone(), text));
        return summary;
    }
    queue.reverse();
    while let Some((key, path)) = queue.pop() {
        if work.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        let Some(index) = plan.steps().iter().position(|step| step.key == key) else {
            continue;
        };
        work.current.store(index, Ordering::Relaxed);
        let Some(step) = plan.steps().get(index) else {
            continue;
        };
        if step.blocked_by.is_some() {
            continue;
        }
        let replaces = match &step.state {
            StepState::Fetch { replaces } | StepState::Manual { replaces } => replaces.clone(),
            StepState::Unavailable { installed } | StepState::Failed { installed } => installed.clone(),
            _ => None,
        };
        let Some(entry) = pack.entry(&key).cloned() else {
            continue;
        };
        let name = entry.name.clone().unwrap_or_else(|| key.to_string());
        let outcome = match path {
            Some(path) => from_file(&mods, &account, &entry, path, &work).await,
            None if matches!(entry.source, Source::Nexus { .. }) => {
                from_nexus(&mods, &account, &entry, &providers, &work).await
            }
            None => from_thunderstore(&mods, &account, &entry, &providers, &work).await,
        };
        let outcome = match outcome {
            Ok(imported) => finish_import(&mods, &entry, replaces, imported).await,
            Err(err) => Err(err),
        };
        match outcome {
            Ok(imported) => {
                tracing::info!(pack = %pack.id, entry = %key, mod_id = %imported.id, duplicate = imported.is_duplicate, "pack entry installed");
                if !imported.is_duplicate {
                    summary.installed.push(name.clone());
                }
                plan.installed(&key, imported.id.clone());
                record_conflicts(&mods, &account, &imported.id, &name, &mut summary).await;
            }
            Err(StepError::Cancelled) => {
                summary.cancelled = true;
                break;
            }
            Err(StepError::Failed(text)) => {
                tracing::warn!(pack = %pack.id, entry = %key, reason = %text, "pack entry failed, skipped and flagged");
                plan.failed(&key);
                summary.failed.push((name, text));
            }
        }
        if let Err(text) = settle(&mods, &account, &plan, &queue, &mut notes, &pack.id).await {
            summary.failed.push((pack.name.clone(), text));
            break;
        }
    }
    tracing::info!(
        pack = %pack.id,
        installed = summary.installed.len(),
        failed = summary.failed.len(),
        cancelled = summary.cancelled,
        "mod pack apply finished"
    );
    summary
}

/// Off the UI: the set as the plan leaves it if every entry still `queued` failed, and the notes.
async fn settle(
    mods: &GameMods,
    account: &str,
    plan: &Plan,
    queued: &[(EntryKey, Option<PathBuf>)],
    notes: &mut LocalNotes,
    pack: &decanter_modpack::PackId,
) -> Result<(), Text> {
    let mut settled = plan.clone();
    for (key, _) in queued {
        settled.failed(key);
    }
    let (mods, account, mut written, pack) = (mods.clone(), account.to_owned(), notes.clone(), pack.clone());
    let notes_out = tokio::task::spawn_blocking(move || {
        arrange(&mods, &account, &settled, &mut written)?;
        written
            .write(&decanter_modpack::local_file(mods.dir(), &pack))
            .map_err(Text::verbatim)?;
        Ok::<_, Text>(written)
    })
    .await
    .map_err(Text::verbatim)??;
    *notes = notes_out;
    Ok(())
}

/// Every present entry into the pack's block, in the plan's order, on or off as the plan says;
/// mods the pack does not know keep their slots (design, section 5.5).
fn arrange(mods: &GameMods, account: &str, plan: &Plan, notes: &mut LocalNotes) -> Result<(), Text> {
    let members: Vec<(&EntryKey, &ModId, bool, bool)> = plan
        .steps()
        .iter()
        .filter_map(|step| match &step.state {
            StepState::Have { id, adopted } => Some((&step.key, id, *adopted, step.enable)),
            _ => None,
        })
        .collect();
    let ids: Vec<ModId> = members.iter().map(|&(_, id, _, _)| id.clone()).collect();
    let slots = mods.load_order(account).map_err(Text::verbatim)?;
    let anchor = slots
        .iter()
        .position(|slot| ids.contains(&slot.id))
        .unwrap_or(slots.len());
    for (offset, id) in ids.iter().enumerate() {
        mods.place(account, id, anchor.saturating_add(offset))
            .map_err(Text::verbatim)?;
    }
    let enabled: HashMap<ModId, bool> = mods
        .load_order(account)
        .map_err(Text::verbatim)?
        .into_iter()
        .map(|slot| (slot.id, slot.enabled))
        .collect();
    for &(key, id, adopted, enable) in &members {
        if enabled.get(id).copied() != Some(enable) {
            mods.set_enabled(account, id, enable).map_err(Text::verbatim)?;
        }
        notes.record(key.clone(), id.clone(), !adopted);
    }
    Ok(())
}

/// The account's enabled mods now, and as they would be with `members` moved into one block at
/// the first one's slot: what applying a pack, or saving a reordered one, changes.
pub(super) fn reorder_change(mods: &GameMods, account: &str, members: &[ModId]) -> decanter_mods::Result<OrderChange> {
    let slots = mods.load_order(account)?;
    let before: Vec<ModId> = slots
        .iter()
        .filter(|slot| slot.enabled)
        .map(|slot| slot.id.clone())
        .collect();
    // Every slot before the first member is not one, so the block lands at that index among the rest.
    let anchor = slots
        .iter()
        .position(|slot| members.contains(&slot.id))
        .unwrap_or(slots.len());
    let mut after: Vec<ModId> = slots
        .iter()
        .filter(|slot| !members.contains(&slot.id))
        .map(|slot| slot.id.clone())
        .collect();
    let block = members
        .iter()
        .filter(|id| slots.iter().any(|slot| slot.id == **id))
        .cloned();
    after.splice(anchor.min(after.len())..anchor.min(after.len()), block);
    after.retain(|id| before.contains(id));
    mods.preview_order(&before, &after)
}

/// The newer version takes the old one's slot and the entry's recorded DLL modes are set.
async fn finish_import(
    mods: &GameMods,
    entry: &Entry,
    replaces: Option<ModId>,
    imported: Imported,
) -> Result<Imported, StepError> {
    let mods = mods.clone();
    let overrides = entry.dll_overrides.clone();
    tokio::task::spawn_blocking(move || {
        if !imported.is_duplicate {
            if let Some(old) = replaces.filter(|old| *old != imported.id) {
                mods.replace(&old, &imported.id)?;
                tracing::info!(old = %old, new = %imported.id, "pack entry replaced the installed version in its slot");
            }
            for wanted in &overrides {
                mods.set_dll_mode(&imported.id, &wanted.dll, wanted.mode)?;
            }
        }
        Ok(imported)
    })
    .await
    .map_err(StepError::failed)?
}

/// One line per mod the new one wins files over or loses them to.
async fn record_conflicts(mods: &GameMods, account: &str, id: &ModId, name: &str, summary: &mut Summary) {
    let (mods, account, id_owned) = (mods.clone(), account.to_owned(), id.clone());
    let read = tokio::task::spawn_blocking(move || {
        let names: HashMap<ModId, String> = mods
            .list()?
            .into_iter()
            .map(|manifest| (manifest.id, manifest.name))
            .collect();
        Ok::<_, decanter_mods::Error>((mods.conflicts(&account)?.remove(&id_owned), names))
    })
    .await;
    let (conflicts, names) = match read {
        Ok(Ok((Some(conflicts), names))) => (conflicts, names),
        Ok(Ok((None, _))) => return,
        Ok(Err(err)) => {
            tracing::warn!(%err, "conflicts after a pack entry could not be read");
            return;
        }
        Err(err) => {
            tracing::warn!(%err, "reading conflicts after a pack entry stopped");
            return;
        }
    };
    let name_of = |other: &ModId| names.get(other).cloned().unwrap_or_else(|| other.to_string());
    let lines = conflicts
        .overrides
        .iter()
        .map(|(other, paths)| {
            rust_i18n::t!(
                "mods.packs.conflict_wins",
                name = name,
                other = name_of(other),
                count = paths.len()
            )
            .into_owned()
        })
        .chain(conflicts.overridden_by.iter().map(|(other, paths)| {
            rust_i18n::t!(
                "mods.packs.conflict_loses",
                name = name,
                other = name_of(other),
                count = paths.len()
            )
            .into_owned()
        }));
    for line in lines {
        if summary.conflicts.len() < MAX_CONFLICT_LINES {
            summary.conflicts.push(line);
        } else {
            summary.more_conflicts = summary.more_conflicts.saturating_add(1);
        }
    }
}

async fn from_thunderstore(
    mods: &GameMods,
    account: &str,
    entry: &Entry,
    providers: &Providers,
    work: &Arc<Work>,
) -> Result<Imported, StepError> {
    let thunderstore = providers
        .thunderstore
        .as_ref()
        .ok_or_else(|| StepError::Failed(Text::new("mods.packs.no_listing")))?;
    let (package, version) = super::apply::thunderstore_version(&thunderstore.listing, entry)
        .ok_or_else(|| StepError::Failed(Text::new("mods.packs.unavailable")))?;
    let (package, version) = (package.clone(), version.clone());
    let staged = mods.clone();
    let staging = tokio::task::spawn_blocking(move || decanter_thunderstore::Staging::new(&staged))
        .await
        .map_err(StepError::failed)??;
    work.thunderstore.expect(version.file_size.unwrap_or_default());
    let downloaded = decanter_thunderstore::download(
        &thunderstore.client,
        &staging,
        &package.id,
        &version,
        &work.thunderstore,
    )
    .await?;
    if entry.track == Track::Pinned
        && let Some(wanted) = entry.sha256.as_ref()
        && wanted.as_str() != downloaded.sha256
    {
        return Err(StepError::Failed(Text::new("mods.packs.hash_mismatch")));
    }
    let (mods, account, entry, work) = (mods.clone(), account.to_owned(), entry.clone(), Arc::clone(work));
    tokio::task::spawn_blocking(move || {
        let preview = mods.preview_package(&downloaded.path, &work.progress, &decanter_thunderstore::LEAVE_OUT)?;
        let origin = decanter_thunderstore::origin(&package.id, &version)?
            .with_credits(decanter_thunderstore::credits(&package, &version));
        let imported = import(&mods, &account, &preview.with_origin(origin), &entry, &work);
        drop(staging);
        imported
    })
    .await
    .map_err(StepError::failed)?
}

async fn from_nexus(
    mods: &GameMods,
    account: &str,
    entry: &Entry,
    providers: &Providers,
    work: &Arc<Work>,
) -> Result<Imported, StepError> {
    let Source::Nexus {
        domain, mod_id, file, ..
    } = &entry.source
    else {
        return Err(StepError::Failed(Text::new("mods.packs.unavailable")));
    };
    let nexus = providers
        .nexus
        .as_ref()
        .filter(|nexus| nexus.is_premium)
        .ok_or_else(|| StepError::Failed(Text::new("mods.nexus.premium_only")))?;
    let refused = || StepError::Failed(Text::new("mods.packs.unavailable"));
    let domain: decanter_nexus::GameDomain = domain.parse().map_err(|_err| refused())?;
    let mod_id = decanter_nexus::NexusModId::new(*mod_id).ok_or_else(refused)?;
    let file_id = decanter_nexus::FileId::new(*file).ok_or_else(refused)?;
    let listed = decanter_nexus::files(&nexus.client, &domain, mod_id).await?;
    let file = listed.get(file_id).cloned().ok_or_else(refused)?;
    if let (Some(wanted), Some(listed)) = (entry.md5.as_ref(), file.md5.as_deref())
        && wanted.as_str() != listed
    {
        return Err(StepError::Failed(Text::new("mods.packs.hash_mismatch")));
    }
    let info = decanter_nexus::mod_info(&nexus.client, &domain, mod_id).await?;
    let staged = mods.clone();
    let staging = tokio::task::spawn_blocking(move || decanter_nexus::Staging::new(&staged))
        .await
        .map_err(StepError::failed)??;
    let link = decanter_nexus::download_link(&nexus.client, &domain, mod_id, file_id).await?;
    let downloaded = decanter_nexus::download(&nexus.client, &staging, &file, link, &work.nexus).await?;
    if let Some(wanted) = entry.sha256.as_ref()
        && wanted.as_str() != downloaded.sha256
    {
        return Err(StepError::Failed(Text::new("mods.packs.hash_mismatch")));
    }
    let (mods, account, entry, work) = (mods.clone(), account.to_owned(), entry.clone(), Arc::clone(work));
    tokio::task::spawn_blocking(move || {
        let preview = mods.preview_archive(&downloaded.path, &work.progress)?;
        let origin =
            decanter_nexus::origin(&domain, mod_id, &file, &downloaded.host)?.with_credits(info.credits(&domain));
        let imported = import(&mods, &account, &preview.with_origin(origin), &entry, &work);
        drop(staging);
        imported
    })
    .await
    .map_err(StepError::failed)?
}

/// A file the user downloaded by hand, or a folder for a local entry that was one, checked against
/// what the entry recorded before it is imported.
async fn from_file(
    mods: &GameMods,
    account: &str,
    entry: &Entry,
    path: PathBuf,
    work: &Arc<Work>,
) -> Result<Imported, StepError> {
    let (mods, account, entry, work) = (mods.clone(), account.to_owned(), entry.clone(), Arc::clone(work));
    tokio::task::spawn_blocking(move || {
        if path.is_dir() {
            if !matches!(entry.source, Source::Local { .. }) || entry.sha256.is_some() {
                return Err(StepError::Failed(Text::new("mods.packs.wants_archive")));
            }
            let manifest = mods.import_folder(&path, &account, entry.enabled)?;
            return Ok(Imported {
                id: manifest.id,
                is_duplicate: false,
            });
        }
        let preview = if matches!(entry.source, Source::Thunderstore { .. }) {
            mods.preview_package(&path, &work.progress, &decanter_thunderstore::LEAVE_OUT)?
        } else {
            mods.preview_archive(&path, &work.progress)?
        };
        verify(&entry, &preview, &path)?;
        let preview = match manual_origin(&entry) {
            Some(origin) => preview.with_origin(origin),
            None => preview,
        };
        import(&mods, &account, &preview, &entry, &work)
    })
    .await
    .map_err(StepError::failed)?
}

/// The entry's hashes are a hard check (design, section 6.3): its archive's SHA-256 when known,
/// else the provider's MD5, else its size; an entry with none takes the file as given.
fn verify(entry: &Entry, preview: &ArchivePreview, path: &Path) -> Result<(), StepError> {
    let mismatch = || StepError::Failed(Text::new("mods.packs.hash_mismatch"));
    let is_pinned = entry.track == Track::Pinned;
    if let Some(wanted) = entry.sha256.as_ref().filter(|_| is_pinned) {
        return if wanted.as_str() == preview.source_sha256() {
            Ok(())
        } else {
            Err(mismatch())
        };
    }
    if let Some(wanted) = entry.md5.as_ref().filter(|_| is_pinned) {
        return if md5_of(path).map_err(StepError::failed)? == wanted.as_str() {
            Ok(())
        } else {
            Err(mismatch())
        };
    }
    if let Some(wanted) = entry.size.filter(|_| is_pinned) {
        let size = std::fs::metadata(path).map_err(StepError::failed)?.len();
        return if size == wanted { Ok(()) } else { Err(mismatch()) };
    }
    tracing::warn!(entry = %entry.key, "pack entry has no hash or size, the picked file is taken as given");
    Ok(())
}

fn md5_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut context = md5::Context::new();
    let mut buffer = vec![0_u8; MD5_CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.consume(buffer.get(..read).unwrap_or_default());
    }
    Ok(format!("{:x}", context.finalize()))
}

/// The origin a provider's file gets when the user brought it, in the provider crate's own
/// spelling, so the next apply, the update checks and the credits recognise it.
fn manual_origin(entry: &Entry) -> Option<ModOrigin> {
    let made = match &entry.source {
        Source::Thunderstore { package, version } => {
            ModOrigin::new(decanter_thunderstore::PLATFORM, package, version, THUNDERSTORE_SITE)
        }
        Source::Nexus {
            domain,
            mod_id,
            file,
            version,
        } => ModOrigin::new(
            decanter_nexus::PLATFORM,
            &format!("{domain}-{mod_id}-{file}"),
            &version.clone().unwrap_or_else(|| format!("file-{file}")),
            NEXUS_SITE,
        ),
        Source::Modio {
            game,
            mod_id,
            file,
            version,
        } => ModOrigin::new(
            decanter_modio::PLATFORM,
            &format!("{game}-{mod_id}"),
            &version
                .as_ref()
                .map_or_else(|| file.to_string(), |version| format!("{file}-{version}")),
            MODIO_SITE,
        ),
        _ => return None,
    };
    made.inspect_err(|err| tracing::warn!(entry = %entry.key, %err, "pack entry origin not kept"))
        .ok()
}

fn import(
    mods: &GameMods,
    account: &str,
    preview: &ArchivePreview,
    entry: &Entry,
    work: &Work,
) -> Result<Imported, StepError> {
    if let Some((id, _)) = preview.duplicate_of() {
        tracing::info!(entry = %entry.key, mod_id = %id, "pack entry archive is already a mod of the game");
        return Ok(Imported {
            id: id.clone(),
            is_duplicate: true,
        });
    }
    let name = entry.name.clone().unwrap_or_else(|| preview.name().to_owned());
    let manifest = mods.import_archive(
        preview,
        &name,
        entry.target.clone(),
        account,
        entry.enabled,
        &work.progress,
    )?;
    Ok(Imported {
        id: manifest.id,
        is_duplicate: false,
    })
}
