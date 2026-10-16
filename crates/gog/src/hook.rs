//! Deploying decanter's own achievement-tracking proxy DLL
//! (`decanter-hook`, built as `Galaxy64.dll` -- see that crate's own doc
//! for why it exists at all) into an installed game, and reading back
//! what it recorded after a session.

use std::path::Path;

use anyhow::Context as _;
use decanter_core::{Db, HookDiagnostic, Storefront};

/// The proxy DLL, cross-compiled from `crates/hook` by this crate's own
/// `build.rs`. Embedded rather than fetched: it's decanter's own small
/// artifact, version-locked to this exact build, not a third-party tool
/// worth keeping independently up to date the way `decanter-runtime`
/// fetches `umu-run`.
///
/// Built only for Linux, where games run under Proton: elsewhere installs
/// leave a game's own `Galaxy64.dll` in place.
#[cfg(target_os = "linux")]
static GALAXY64_DLL: Option<&[u8]> = Some(include_bytes!(env!("GALAXY64_DLL_PATH")));
#[cfg(not(target_os = "linux"))]
static GALAXY64_DLL: Option<&[u8]> = None;

/// What `decanter-hook` renames the real DLL to before dropping its own
/// proxy in as `Galaxy64.dll`. Duplicated as a literal, kept in sync by
/// hand with that crate's own `REAL_DLL` constant: it builds a Windows
/// cdylib, not a Rust library, so nothing here can depend on it.
/// `install::verify` reuses this same constant, rather than a second
/// copy, for the one other place in this crate that has to know it.
pub(crate) const REAL_DLL_BACKUP: &str = "Galaxy64.orig.dll";

/// Finds `Galaxy64.dll` among `relative_paths` (an install's own file
/// list -- the same one [`super::install::program`]'s launch-executable
/// search reads) and swaps decanter's achievement-tracking proxy in for
/// it, if it isn't already there.
///
/// Best-effort about there being nothing to do: most GOG games ship no
/// Galaxy integration at all, and this runs after every install
/// regardless of that.
///
/// # Errors
/// Fails if a `Galaxy64.dll` was found but renaming or writing over it
/// failed. Unlike "no Galaxy64.dll at all", that's worth surfacing: a
/// half-completed swap (the real DLL renamed aside, the proxy never
/// written) would leave the game unable to start at all.
pub async fn deploy(install_root: &Path, relative_paths: &[String]) -> anyhow::Result<bool> {
    let Some(relative) = relative_paths.iter().find(|path| {
        Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("Galaxy64.dll"))
    }) else {
        return Ok(false);
    };
    let real_dll = install_root.join(relative);
    let Some(proxy) = GALAXY64_DLL else {
        tracing::info!(path = %real_dll.display(), "achievement hook not built for this system, not deploying it");
        return Ok(false);
    };
    let backup = real_dll.with_file_name(REAL_DLL_BACKUP);
    if backup.is_file() {
        // Already deployed -- a reinstall or repair landed the same file
        // again, and re-renaming now would back up decanter's own proxy
        // instead of the genuine DLL.
        tracing::debug!(path = %real_dll.display(), "achievement hook already deployed, leaving it alone");
        return Ok(true);
    }
    tokio::fs::rename(&real_dll, &backup)
        .await
        .with_context(|| format!("renaming the real {} aside", real_dll.display()))?;
    if let Err(err) = tokio::fs::write(&real_dll, proxy).await {
        // Best-effort recovery: put the real DLL back rather than leave
        // the game with neither the original nor the proxy in place.
        let _ = tokio::fs::rename(&backup, &real_dll).await;
        return Err(err).context("writing decanter's achievement-hook proxy");
    }
    tracing::info!(path = %real_dll.display(), "deployed the achievement hook");
    Ok(true)
}

/// Where decanter tells a running game's hook to write its own log --
/// see `decanter_hook::log`'s own doc for the line format. Beside the
/// prefix rather than the install directory: the install directory is
/// shared across every account that owns the game (see
/// `decanter_core::Library`'s own doc), but the prefix, and so this log,
/// already isn't.
#[must_use]
pub fn achievement_log_path(prefix: &decanter_runtime::Prefix) -> std::path::PathBuf {
    prefix.path().join("decanter-hook.jsonl")
}

/// Applies whatever `path` accumulated during one session: every unlock
/// goes into `db`'s local achievement cache (see
/// [`decanter_core::Db::mark_local_unlock`] for why that's the only way
/// this game's own unlock ever gets recorded anywhere, under Wine), and
/// every hook-installation failure goes into `db`'s own diagnostics
/// table for later review. The file itself is removed once read --
/// decanter-hook recreates it fresh next launch, and leaving old entries
/// in place would re-apply (harmlessly) every past unlock but
/// re-*record* every past diagnostic, once per session, forever.
///
/// Returns how many unlocks were actually applied. Best-effort about the
/// file itself: no log at all just means nothing was hooked, or nothing
/// ran, and the same missing file also means "no diagnostics either".
pub async fn process_log(db: &Db, account_id: &str, product_id: &str, path: &Path) -> usize {
    let Ok(contents) = tokio::fs::read_to_string(path).await else {
        return 0;
    };

    let mut applied = 0;
    for line in contents.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            tracing::debug!(
                account_id,
                product_id,
                line,
                "unreadable line in decanter-hook's log, skipping"
            );
            continue;
        };
        match entry.get("kind").and_then(serde_json::Value::as_str) {
            Some("unlock") => {
                if apply_unlock(db, account_id, product_id, &entry) {
                    applied += 1;
                }
            }
            Some("hook_diagnostic") => record_diagnostic(db, account_id, product_id, &entry),
            // "trace" and anything else this version doesn't know about
            // yet: decanter-hook's own breadcrumb trail, not decanter's
            // concern.
            _ => {}
        }
    }

    if let Err(err) = tokio::fs::remove_file(path).await {
        tracing::debug!(account_id, product_id, path = %path.display(), %err, "couldn't remove decanter-hook's log after reading it");
    }
    applied
}

fn apply_unlock(db: &Db, account_id: &str, product_id: &str, entry: &serde_json::Value) -> bool {
    let (Some(achievement_key), Some(at)) = (
        entry.get("achievement").and_then(serde_json::Value::as_str),
        entry.get("at").and_then(serde_json::Value::as_i64),
    ) else {
        return false;
    };
    let unlocked_at = format_unlocked_at(at);
    match db.mark_local_unlock(account_id, Storefront::Gog, product_id, achievement_key, &unlocked_at) {
        Ok(true) => {
            tracing::info!(
                account_id,
                product_id,
                achievement_key,
                "applied a hook-reported achievement unlock"
            );
            true
        }
        Ok(false) => {
            // Not a failure: this game's achievement list (names, icons,
            // descriptions) just hasn't been fetched from GOG yet, so
            // there's no row to attach this unlock to. The next real
            // sync (`poll_achievements`) will create it locked, same as
            // it always would -- this unlock is simply lost, since
            // there's nothing to retry it against later.
            tracing::debug!(
                account_id,
                product_id,
                achievement_key,
                "hook reported an unlock for an achievement not yet cached"
            );
            false
        }
        Err(err) => {
            tracing::warn!(account_id, product_id, achievement_key, %err, "failed to apply a hook-reported unlock");
            false
        }
    }
}

fn record_diagnostic(db: &Db, account_id: &str, product_id: &str, entry: &serde_json::Value) {
    let (Some(stage), Some(detail), Some(at)) = (
        entry.get("stage").and_then(serde_json::Value::as_str),
        entry.get("detail").and_then(serde_json::Value::as_str),
        entry.get("at").and_then(serde_json::Value::as_i64),
    ) else {
        return;
    };
    let diagnostic = HookDiagnostic {
        recorded_at: at,
        stage: stage.to_string(),
        detail: detail.to_string(),
    };
    if let Err(err) = db.record_hook_diagnostic(account_id, Storefront::Gog, product_id, &diagnostic) {
        tracing::warn!(account_id, product_id, %err, "failed to record a hook diagnostic");
    }
}

/// RFC 3339, the nearest standard shape to GOG's own `unlocked_at`
/// (`+0000` offsets); the UI parses both (see
/// `app/src/views/library/data.rs`).
fn format_unlocked_at(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .unwrap_or_default()
        .to_rfc3339()
}
