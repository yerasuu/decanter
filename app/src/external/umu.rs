//! Finding an external game in the UMU database by its name, so it can
//! launch under the id protonfixes keys that game's fixes off — the thing a
//! storefront id does for a GOG game.
//!
//! Every Proton build that carries protonfixes ships the database as
//! `protonfixes/umu-database.csv`; the newest one installed is read, and
//! only without any is the canonical copy fetched.

use std::path::PathBuf;

use super::UmuEntry;

const REMOTE: &str = "https://raw.githubusercontent.com/Open-Wine-Components/umu-database/main/umu-database.csv";
/// Matches offered for one name: the database lists a game once per store,
/// and a name can match a sequel or two besides.
const MAX_MATCHES: usize = 8;

/// One database row a name matched.
#[derive(Clone)]
pub struct Match {
    pub title: String,
    pub entry: UmuEntry,
    /// The title (or acronym) is the name searched for, not just close.
    pub exact: bool,
}

/// Rows whose title (or common acronym) matches `name`, best first: exact,
/// then one title starting with the other, then one containing the other.
pub async fn search(name: &str) -> Result<Vec<Match>, String> {
    let csv = crate::achv_try!(database().await);
    let query = normalize(name);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut scored: Vec<(u8, Match)> = csv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = parse_row(line);
            let (title, store, id) = (fields.first()?, fields.get(1)?, fields.get(3)?);
            if id.is_empty() {
                return None;
            }
            let acronym = fields.get(4).map(|acronym| normalize(acronym)).unwrap_or_default();
            let score = rank(&normalize(title), &acronym, &query)?;
            Some((
                score,
                Match {
                    title: title.clone(),
                    entry: UmuEntry {
                        id: id.clone(),
                        store: (store != "none" && !store.is_empty()).then(|| store.clone()),
                    },
                    exact: score == 0,
                },
            ))
        })
        .collect();
    scored.sort_by_key(|(score, found)| (*score, found.entry.store.is_some(), found.title.len()));
    let mut seen = std::collections::HashSet::new();
    Ok(scored
        .into_iter()
        .map(|(_, found)| found)
        .filter(|found| seen.insert((found.entry.id.clone(), found.entry.store.clone())))
        .take(MAX_MATCHES)
        .collect())
}

fn rank(title: &str, acronym: &str, query: &str) -> Option<u8> {
    if title == query || acronym == query {
        Some(0)
    } else if title.starts_with(query) || (long_enough(title) && query.starts_with(title)) {
        Some(1)
    } else if title.contains(query) || (long_enough(title) && query.contains(title)) {
        Some(2)
    } else {
        None
    }
}

/// Whether a title is long enough to count as found inside a longer query —
/// otherwise a two-letter title like "Ys" matches half of all names.
fn long_enough(title: &str) -> bool {
    title.chars().count() >= 4
}

/// Lowercase letters and digits with single spaces between words, so
/// "Arknights: Endfield" and "arknights endfield" compare equal.
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

async fn database() -> Result<String, String> {
    if let Some(local) = newest_local() {
        match tokio::fs::read_to_string(&local).await {
            Ok(text) => {
                tracing::debug!(path = %local.display(), "read the umu database from an installed proton");
                return Ok(text);
            }
            Err(err) => tracing::warn!(path = %local.display(), %err, "could not read an installed umu database"),
        }
    }
    tracing::debug!("no installed proton carries the umu database, fetching it");
    let response = crate::achv_try!(
        reqwest::get(REMOTE)
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|err| format!("descargando la base de datos de UMU: {err}"))
    );
    response
        .text()
        .await
        .map_err(|err| format!("leyendo la base de datos de UMU: {err}"))
}

/// The most recently updated copy among the Proton builds Steam can see.
fn newest_local() -> Option<PathBuf> {
    std::fs::read_dir(decanter_runtime::steam_compat_dir()?)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("protonfixes/umu-database.csv"))
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// One CSV line's fields. Titles like `"Warhammer 40,000: Space Marine"`
/// are quoted for their commas; `""` inside quotes is a literal quote.
fn parse_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match (ch, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => fields.push(std::mem::take(&mut field)),
            _ => field.push(ch),
        }
    }
    fields.push(field);
    fields
}
