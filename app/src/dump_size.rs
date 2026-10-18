//! PS dumps' sizes, kept in the library database with the fingerprint they
//! were measured at: Almacenamiento and the PS section's rescans only stat
//! a dump, and walk its files only when it is new or changed.

use std::path::Path;

use decanter_core::Db;

/// The dump in `dir`'s size, measured again and recorded in `db` only when
/// its fingerprint moved. Blocking.
pub fn size(db: Option<&Db>, dir: &Path) -> u64 {
    let (eboot, sfo, json) = (
        dir.join("eboot.bin"),
        dir.join("sce_sys/param.sfo"),
        dir.join("sce_sys/param.json"),
    );
    let fingerprint = decanter_core::install_fingerprint(&[dir], &[&eboot, &sfo, &json]);
    let known = db.and_then(|db| {
        db.dump_size(dir)
            .inspect_err(|err| tracing::warn!(dir = %dir.display(), %err, "could not read a PS dump's recorded size"))
            .ok()
            .flatten()
    });
    if let Some((size, measured_at)) = known
        && measured_at == fingerprint
    {
        return size;
    }
    let size = decanter_core::directory_size(dir)
        .inspect_err(|err| tracing::warn!(dir = %dir.display(), %err, "could not measure a PS dump"))
        .unwrap_or(0);
    if let Some(db) = db
        && let Err(err) = db.set_dump_size(dir, size, &fingerprint)
    {
        tracing::warn!(dir = %dir.display(), %err, "could not record a PS dump's size");
    }
    tracing::debug!(dir = %dir.display(), size, "PS dump measured");
    size
}

/// Measures every dump in `dirs` that is new or changed since last time.
pub fn refresh<'dir>(dirs: impl IntoIterator<Item = &'dir Path>) {
    let db = crate::core::db()
        .inspect_err(|err| tracing::warn!(%err, "no database to record PS dump sizes in"))
        .ok();
    for dir in dirs {
        size(db.as_ref(), dir);
    }
}

/// Carries a dump's recorded size along when it moved whole to `new`.
pub fn moved(old: &Path, new: &Path) {
    let moved = crate::core::db()
        .map_err(|err| err.to_string())
        .and_then(|db| db.move_dump_size(old, new).map_err(|err| err.to_string()));
    if let Err(err) = moved {
        tracing::warn!(from = %old.display(), to = %new.display(), %err, "could not carry the PS dump's size along");
    }
}
