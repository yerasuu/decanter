//! Recording achievement unlocks (and a debugging breadcrumb trail)
//! to a JSONL file beside the proxy DLL.

use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::ffi::own_directory;

/// Appends one unlock. Best effort in every direction: a missing path, an
/// unwritable file or a non-UTF-8 achievement id all end with the game
/// continuing exactly as it would have.
pub(crate) fn record_unlock(name: *const c_char) {
    if name.is_null() {
        return;
    }
    // SAFETY: the pointer comes from the game, which owns a NUL-terminated
    // string here per the Galaxy API contract.
    let Ok(name) = (unsafe { std::ffi::CStr::from_ptr(name) }).to_str() else {
        return;
    };
    append(&format!(
        r#"{{"kind":"unlock","at":{},"product_id":"{}","achievement":"{}"}}"#,
        now(),
        escape(&product_id()),
        escape(name),
    ));
}

/// A hook-installation failure worth carrying home rather than just
/// tracing: decanter's own reader singles out `"kind":"hook_diagnostic"`
/// lines to collect, since these are exactly the cases in
/// `docs/galaxy64-abi.md`'s own family — a `Galaxy64.dll` build the
/// vtable scan doesn't recognize — that adapting `abi_scan` for a new
/// case needs real forensic detail from, not just "it didn't work".
/// `detail` is caller-formatted free text (already escaped internally)
/// so each call site can carry whatever's actually relevant to its own
/// failure mode.
pub(crate) fn report_diagnostic(stage: &str, detail: &str) {
    append(&format!(
        r#"{{"kind":"hook_diagnostic","at":{},"product_id":"{}","stage":"{}","detail":"{}"}}"#,
        now(),
        escape(&product_id()),
        escape(stage),
        escape(detail),
    ));
}

fn product_id() -> String {
    std::env::var("DECANTER_PRODUCT_ID").unwrap_or_default()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn append(line: &str) {
    use std::io::Write;
    let Some(path) = log_path() else {
        return;
    };
    // decanter's folder in a library it hasn't used yet may not exist; the
    // first line of a session would otherwise be lost, and every one after.
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{line}");
    let _ = file.flush();
}

/// A breadcrumb, so a run that never unlocks anything still says how far it
/// got. Written to the same file and told apart by carrying `note` rather
/// than `achievement`. Flushed per line: the point is to survive a hang.
pub(crate) fn trace(note: &str) {
    append(&format!(
        r#"{{"kind":"trace","at":{},"note":"{}"}}"#,
        now(),
        escape(note)
    ));
}

/// Traces the first time a given export is entered, so the per-frame pumps
/// do not bury the sequence in noise.
pub(crate) fn trace_once(seen: &AtomicUsize, what: &str) {
    if seen.swap(1, Ordering::Relaxed) == 0 {
        trace(what);
    }
}

/// Achievement ids are conventionally bare identifiers, but nothing
/// enforces that -- escape rather than trust it and emit broken JSON.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn log_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("DECANTER_ACHIEVEMENT_LOG")
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    // Beside the DLL, so a run with nothing configured still leaves the
    // evidence somewhere findable rather than dropping it.
    own_directory().map(|dir| dir.join("decanter-achievements.jsonl"))
}
