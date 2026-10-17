//! The one-time move of `~/.local/share/decanter/links.db` into the
//! libraries it describes: one database per library root, holding only the
//! profiles whose library is that root.
//!
//! Runs only while `libraries.toml` doesn't exist, and writing it is the
//! commit point: until then every copy is a `.tmp` beside its final name
//! (or a finished database nothing points at yet), and the old folder is
//! never written to beyond a `MIGRATED-TO` note left after the commit.
//! Deleting `libraries.toml` goes back to the old database.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io;
use std::iter;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};

use crate::bootstrap;
use crate::db::{Db, has_column, table_exists};
use crate::platform::{folder_identity, others_running};

/// Profile ids by the library root they keep their games in.
type Groups = BTreeMap<PathBuf, Vec<String>>;
type ProfileRoot = (String, Option<String>);

/// Rows keyed by the account (or, for external games, the profile) owning them.
const OWNER_TABLES: [&str; 7] = [
    "games_cache",
    "achievement_cache",
    "hook_diagnostics",
    "favorites",
    "playtime",
    "hidden",
    "steam_shortcuts",
];

/// Rows keyed by the profile itself.
const PROFILE_TABLES: [&str; 3] = ["collections", "profile_libraries", "accounts"];

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Db(#[from] crate::db::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("decanter or a game it started is still running")]
    Busy,
    #[error("integrity check of the copy for {} failed: {report}", root.display())]
    Integrity { root: PathBuf, report: String },
    #[error("{table} in the copy for {} has {found} rows, expected {expected}", root.display())]
    RowCount {
        root: PathBuf,
        table: String,
        expected: i64,
        found: i64,
    },
}

#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    NotNeeded,
    /// Another decanter, or a game it started, is running, or a library's
    /// disk isn't there: tried again on the next start.
    Postponed,
    Migrated(Vec<PathBuf>),
}

/// Moves the old database into the libraries, if it hasn't been yet, along
/// with the old `umu-run` to where `umu_run_path` puts it in a library's
/// data folder — `None` where nothing runs through umu.
///
/// # Errors
/// Fails when a copy can't be made or doesn't verify; nothing points at
/// the copies then, and the old database stays in use.
pub fn run(umu_run_path: Option<fn(&Path) -> PathBuf>) -> Result<Outcome, Error> {
    let (Some(config), Some(legacy_db)) = (bootstrap::config_path(), bootstrap::legacy_db_path()) else {
        return Ok(Outcome::NotNeeded);
    };
    if config.exists() || !legacy_db.is_file() {
        return Ok(Outcome::NotNeeded);
    }
    bootstrap::exclusive(|| migrate(&legacy_db, umu_run_path)).unwrap_or_else(|| {
        tracing::info!("another decanter holds the runtime lock, migration postponed");
        Ok(Outcome::Postponed)
    })
}

fn migrate(legacy_db: &Path, umu_run_path: Option<fn(&Path) -> PathBuf>) -> Result<Outcome, Error> {
    if others_running() {
        tracing::info!("decanter or a game it started is running, migration postponed");
        return Ok(Outcome::Postponed);
    }
    let legacy_dir = legacy_db.parent().unwrap_or_else(|| Path::new("/"));
    let source = Connection::open_with_flags(legacy_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let Some(groups) = group_by_root(&source, &legacy_dir.join("steamapps"))? else {
        return Ok(Outcome::Postponed);
    };
    let active = active_profile(&source)?;
    let counts = row_counts(&source)?;
    let legacy_written = fs::metadata(legacy_db)?.modified()?;
    for (root, keep) in groups.iter().filter(|&(root, _)| !written_since(root, legacy_written)) {
        copy_into(&source, &counts, root, keep, active.as_deref())?;
    }
    drop(source);
    for root in groups.keys() {
        copy_art(legacy_dir, root);
        if let Some(path_in) = umu_run_path {
            copy_umu_run(legacy_dir, &path_in(&bootstrap::library_data_dir(root)));
        }
    }

    let active_root = active
        .as_ref()
        .and_then(|profile| groups.iter().find(|(_, ids)| ids.contains(profile)))
        .or_else(|| groups.iter().next())
        .map(|(root, _)| root.clone());
    let profile = active.filter(|profile| groups.values().any(|ids| ids.contains(profile)));
    let roots: Vec<PathBuf> = groups.into_keys().collect();
    bootstrap::commit(roots.clone(), active_root, profile)?;
    tracing::info!(roots = ?roots, "migrated the database into the libraries");

    let note: String = iter::once("decanter now keeps its data in:\n".to_owned())
        .chain(
            roots
                .iter()
                .map(|root| format!("{}\n", bootstrap::library_data_dir(root).display())),
        )
        .collect();
    if let Err(err) = fs::write(legacy_dir.join("MIGRATED-TO"), note) {
        tracing::warn!(%err, "cannot leave the MIGRATED-TO note");
    }
    Ok(Outcome::Migrated(roots))
}

/// The profiles by the root their library is in, or `None` when one of
/// those roots isn't there. The old default library is made if missing:
/// a profile that never installed anything never needed it.
fn group_by_root(source: &Connection, default_root: &Path) -> Result<Option<Groups>, Error> {
    let mut by_path = Groups::new();
    for (id, library_path) in profiles(source)? {
        let root = library_path.map_or_else(|| default_root.to_path_buf(), PathBuf::from);
        by_path.entry(root).or_default().push(id);
    }
    // One folder reached by two paths (a symlink) would otherwise get two
    // databases written to the same file, the second over the first.
    let mut folders = HashMap::new();
    let mut groups = Groups::new();
    for (root, ids) in by_path {
        if root == default_root {
            fs::create_dir_all(&root)?;
        } else if !root.is_dir() {
            tracing::warn!(root = %root.display(), "a library's disk isn't there, migration postponed");
            return Ok(None);
        }
        let first = folders.entry(folder_identity(&root)?).or_insert_with(|| root.clone());
        groups.entry(first.clone()).or_default().extend(ids);
    }
    Ok(Some(groups))
}

fn active_profile(source: &Connection) -> Result<Option<String>, Error> {
    if !table_exists(source, "settings")? {
        return Ok(None);
    }
    Ok(source
        .query_row(
            "SELECT value FROM settings WHERE key = 'active_profile_id'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether `root` has a database written after `legacy_written`: then
/// `libraries.toml` went missing after a migration, and links.db only holds
/// what came before it.
fn written_since(root: &Path, legacy_written: SystemTime) -> bool {
    let newer = fs::metadata(bootstrap::db_path(root))
        .and_then(|meta| meta.modified())
        .is_ok_and(|written| written > legacy_written);
    if newer {
        tracing::info!(root = %root.display(), "the library's database is newer than links.db, kept");
    }
    newer
}

/// A cache, so a copy that fails only costs downloading it again.
fn copy_art(legacy_dir: &Path, root: &Path) {
    let from = legacy_dir.join("art");
    let to = bootstrap::library_data_dir(root).join("art");
    if !from.is_dir() || to.exists() {
        return;
    }
    // Copied aside, like umu-run, so a copy cut short isn't taken for the
    // whole cache by the next run's `exists` check.
    let partial = to.with_added_extension("part");
    if let Err(err) = crate::copy_dir(&from, &partial, |_| {}).and_then(|()| fs::rename(&partial, &to)) {
        tracing::warn!(root = %root.display(), %err, "cannot copy the art cache into the library");
    }
}

/// Without it the first launch downloads it again.
fn copy_umu_run(legacy_dir: &Path, to: &Path) {
    let from = legacy_dir.join("umu-run");
    if !from.is_file() || to.exists() {
        return;
    }
    // A truncated copy under the final name would be run on every launch.
    let partial = to.with_added_extension("part");
    let copied = to
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::copy(&from, &partial))
        .and_then(|_| File::open(&partial)?.sync_all())
        .and_then(|()| fs::rename(&partial, to));
    if let Err(err) = copied {
        tracing::warn!(to = %to.display(), %err, "cannot copy umu-run into the library");
    }
}

/// Every profile, and the library root it chose if it chose one.
fn profiles(source: &Connection) -> Result<Vec<ProfileRoot>, Error> {
    if !table_exists(source, "profiles")? {
        return Ok(Vec::new());
    }
    let query = if has_column(source, "profiles", "library_path")? {
        "SELECT id, library_path FROM profiles"
    } else {
        "SELECT id, NULL FROM profiles"
    };
    let mut statement = source.prepare(query)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn row_counts(conn: &Connection) -> Result<HashMap<String, i64>, Error> {
    let mut statement = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let tables = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut counts = HashMap::new();
    for table in tables {
        let table = table?;
        let count = conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| row.get(0))?;
        counts.insert(table, count);
    }
    Ok(counts)
}

fn check_counts(root: &Path, conn: &Connection, expected: &HashMap<String, i64>) -> Result<(), Error> {
    let found = row_counts(conn)?;
    for (table, &expected) in expected {
        let found = found.get(table).copied().unwrap_or(0);
        if found != expected {
            return Err(Error::RowCount {
                root: root.to_path_buf(),
                table: table.clone(),
                expected,
                found,
            });
        }
    }
    Ok(())
}

/// `root`'s database: a full copy of the old one, less every row of the
/// profiles kept elsewhere, checked and synced before it takes its name.
fn copy_into(
    source: &Connection,
    counts: &HashMap<String, i64>,
    root: &Path,
    keep: &[String],
    active: Option<&str>,
) -> Result<(), Error> {
    let dir = bootstrap::library_data_dir(root);
    fs::create_dir_all(&dir)?;
    let target = bootstrap::db_path(root);
    let partial = target.with_extension("db.tmp");
    match fs::remove_file(&partial) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err.into()),
        Ok(()) | Err(_) => {}
    }
    source.execute("VACUUM INTO ?1", params![partial.to_string_lossy()])?;
    check_counts(root, &Connection::open(&partial)?, counts)?;

    // Brings an old-shaped copy up to the current schema before any row is
    // picked out of it by the columns it has now.
    let mut db = Db::open(&partial)?;
    let mut expected = row_counts(&db.conn)?;
    for (table, deleted) in drop_other_profiles(&mut db.conn, keep, active)? {
        if let Some(count) = expected.get_mut(table) {
            *count -= i64::try_from(deleted).unwrap_or(i64::MAX);
        }
    }
    check_counts(root, &db.conn, &expected)?;
    let report: String = db.conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if report != "ok" {
        return Err(Error::Integrity {
            root: root.to_path_buf(),
            report,
        });
    }
    drop(db);
    File::open(&partial)?.sync_all()?;

    // From a migration rolled back to an older decanter that went on using
    // links.db: kept, not overwritten.
    if target.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        fs::rename(&target, target.with_extension(format!("db.{stamp}.bak")))?;
    }
    fs::rename(&partial, &target)?;
    File::open(&dir)?.sync_all()?;
    tracing::info!(root = %root.display(), profiles = keep.len(), "library database written");
    Ok(())
}

/// The old data folder, while it's still there after the migration.
#[must_use]
pub fn legacy_leftover() -> Option<PathBuf> {
    let dir = bootstrap::legacy_dir()?;
    let migrated = matches!(bootstrap::location(), bootstrap::Location::Library(_));
    (migrated && dir.join("MIGRATED-TO").is_file()).then_some(dir)
}

/// Deletes the old data folder, all but what holds one of `libraries` or
/// a root in `libraries.toml`: the old default library lived inside it.
///
/// # Errors
/// [`Error::Busy`] while an older decanter or a game it started may still
/// be using it, or the error deleting something in it.
pub fn remove_legacy(libraries: &[PathBuf]) -> Result<(), Error> {
    let Some(dir) = legacy_leftover() else {
        return Ok(());
    };
    if others_running() {
        return Err(Error::Busy);
    }
    let known = bootstrap::known_roots();
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if libraries.iter().chain(&known).any(|root| root.starts_with(&path)) {
            continue;
        }
        if path.is_dir() && !path.is_symlink() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    if let Err(err) = fs::remove_dir(&dir)
        && err.kind() != io::ErrorKind::DirectoryNotEmpty
    {
        return Err(err.into());
    }
    tracing::info!(dir = %dir.display(), "removed the old data folder");
    Ok(())
}

/// Deletes every row of the profiles not in `keep`, returning how many
/// went per table.
fn drop_other_profiles(
    conn: &mut Connection,
    keep: &[String],
    active: Option<&str>,
) -> Result<HashMap<&'static str, usize>, Error> {
    let tx = conn.transaction()?;
    tx.execute("CREATE TEMP TABLE kept (id TEXT PRIMARY KEY)", [])?;
    for id in keep {
        tx.execute("INSERT INTO kept (id) VALUES (?1)", params![id])?;
    }
    tx.execute_batch(
        "CREATE TEMP TABLE gone_owners AS
             SELECT id FROM profiles WHERE id NOT IN (SELECT id FROM kept)
             UNION SELECT id FROM accounts WHERE profile_id NOT IN (SELECT id FROM kept);",
    )?;
    let mut removed = HashMap::new();
    for table in OWNER_TABLES {
        removed.insert(
            table,
            tx.execute(
                &format!("DELETE FROM {table} WHERE account_id IN (SELECT id FROM gone_owners)"),
                [],
            )?,
        );
    }
    removed.insert(
        "collection_members",
        tx.execute(
            "DELETE FROM collection_members WHERE collection_id IN
                 (SELECT id FROM collections WHERE profile_id NOT IN (SELECT id FROM kept))",
            [],
        )?,
    );
    for table in PROFILE_TABLES {
        removed.insert(
            table,
            tx.execute(
                &format!("DELETE FROM {table} WHERE profile_id NOT IN (SELECT id FROM kept)"),
                [],
            )?,
        );
    }
    // Per-profile view state is stored as `<name>:<profile id>`.
    removed.insert(
        "settings",
        tx.execute(
            "DELETE FROM settings WHERE EXISTS (
                 SELECT 1 FROM profiles WHERE profiles.id NOT IN (SELECT id FROM kept)
                     AND substr(settings.key, length(settings.key) - length(profiles.id)) = ':' || profiles.id)",
            [],
        )?,
    );
    if active.is_none_or(|profile| !keep.iter().any(|id| id == profile))
        && let Some(first) = keep.first()
    {
        tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'active_profile_id'",
            params![first],
        )?;
    }
    removed.insert(
        "profiles",
        tx.execute("DELETE FROM profiles WHERE id NOT IN (SELECT id FROM kept)", [])?,
    );
    tx.execute_batch("DROP TABLE temp.kept; DROP TABLE temp.gone_owners;")?;
    tx.commit()?;
    Ok(removed)
}
