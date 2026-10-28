//! Text search across the attached logs of one scope (a title or a fingerprint). Logs live as encrypted blobs, so a
//! search costs one read per log: it reads the scope newest first, one log at a time, and stops at
//! `logs.search_max_logs`, `logs.search_max_mib` or `logs.search_max_matches`, saying which on the page. One search
//! runs at a time per account, whether through its sessions or its Bearer key, so a maintainer cannot pile them up
//! against uploads.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Mutex, PoisonError};

use axum::extract::{Query, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use serde::Deserialize;

use super::{BAD_FILTER, NAV, TITLE, bad_request, game_cell, html_page, parse_scope, search_form};
use crate::admin::Admin;
use crate::app::{AppState, internal};
use crate::html::escape_html;
use crate::logtext::{Found, search as scan};
use crate::secrets::{Hash, sha256};
use crate::store::{LogFilter, LogRow};

/// Characters a query may have; a search is for a phrase out of a log, not for a log.
pub const MAX_QUERY_CHARS: usize = 200;
/// Characters of a matching or context line shown.
const LINE_CHARS: usize = 500;

#[derive(Default)]
pub struct Searches {
    running: Mutex<HashSet<Hash>>,
}

/// Holds an account's slot until the search ends, however it ends.
pub struct Running<'a> {
    searches: &'a Searches,
    key: Hash,
}

impl Searches {
    /// The slot of `key`, or `None` while a search under that key runs.
    pub fn try_begin(&self, key: Hash) -> Option<Running<'_>> {
        let inserted = self.running.lock().unwrap_or_else(PoisonError::into_inner).insert(key);
        // Built only once the slot is ours: a `Running` dropped on a refusal would free the other search's slot.
        if inserted {
            Some(Running { searches: self, key })
        } else {
            None
        }
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.searches
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

#[must_use]
pub fn slot_key(admin: &Admin) -> Hash {
    sha256(admin.user.as_bytes())
}

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    product: Option<String>,
    title: Option<String>,
    day: Option<String>,
    kind: Option<String>,
    fingerprint: Option<String>,
}

/// Why a search read fewer logs, or showed fewer matches, than its scope holds.
#[derive(Clone, Copy)]
enum Stop {
    Logs(u32),
    Bytes(u32),
    Matches(u32),
}

struct LogHits {
    row: LogRow,
    found: Found,
}

struct Outcome {
    total: usize,
    searched: usize,
    read_bytes: u64,
    hits: usize,
    stop: Option<Stop>,
    logs: Vec<LogHits>,
}

fn is_query(text: &str) -> bool {
    let chars = text.chars().count();
    (1..=MAX_QUERY_CHARS).contains(&chars) && !text.chars().any(char::is_control)
}

async fn run(state: &AppState, filter: &LogFilter, needle: &str) -> anyhow::Result<Outcome> {
    let bounds = &state.config.logs;
    let max_bytes = u64::from(bounds.search_max_mib) * 1024 * 1024;
    let rows = state.stores.logs(filter, 0, usize::MAX).await?;
    let mut outcome = Outcome {
        total: rows.len(),
        searched: 0,
        read_bytes: 0,
        hits: 0,
        stop: None,
        logs: Vec::new(),
    };
    for (position, row) in rows.into_iter().enumerate() {
        if position >= bounds.search_max_logs as usize {
            outcome.stop = Some(Stop::Logs(bounds.search_max_logs));
            break;
        }
        let Some(size) = state.stores.log_size(&row).await? else {
            continue;
        };
        if outcome.read_bytes + size > max_bytes {
            outcome.stop = Some(Stop::Bytes(bounds.search_max_mib));
            break;
        }
        let Some(bytes) = state.stores.log_bytes(&row).await? else {
            continue;
        };
        outcome.read_bytes += size;
        outcome.searched += 1;
        let left = (bounds.search_max_matches as usize).saturating_sub(outcome.hits);
        let owned = needle.to_owned();
        let found =
            tokio::task::spawn_blocking(move || scan(&String::from_utf8_lossy(&bytes), &owned, left, LINE_CHARS))
                .await?;
        outcome.hits += found.hits;
        if found.hits > 0 {
            outcome.logs.push(LogHits { row, found });
        }
        if outcome.hits >= bounds.search_max_matches as usize {
            outcome.stop = Some(Stop::Matches(bounds.search_max_matches));
            break;
        }
    }
    Ok(outcome)
}

fn render(filter: &LogFilter, query: &str, outcome: &Outcome, page_lines: usize) -> String {
    let mut body = String::from(NAV);
    body.push_str(&search_form(filter, query));
    let _ = write!(
        body,
        "<p>Se buscó «{}» en {} de {} logs del ámbito ({} MiB leídos): {} coincidencias en {} logs.</p>",
        escape_html(query),
        outcome.searched,
        outcome.total,
        outcome.read_bytes.div_ceil(1024 * 1024),
        outcome.hits,
        outcome.logs.len(),
    );
    if let Some(stop) = outcome.stop {
        let why = match stop {
            Stop::Logs(n) => format!("el tope de {n} logs por búsqueda"),
            Stop::Bytes(n) => format!("el tope de {n} MiB leídos por búsqueda"),
            Stop::Matches(n) => format!("el tope de {n} coincidencias"),
        };
        let _ = write!(
            body,
            "<p class=\"cut\">La búsqueda paró en {why}: buscados {} de {} logs. Acota el ámbito (día, tipo o \
             producto) para ver el resto.</p>",
            outcome.searched, outcome.total
        );
    }
    for log in &outcome.logs {
        let id = escape_html(&log.row.id);
        let _ = write!(
            body,
            "<h3><a href=\"/admin/logs/{id}\"><code>{id}</code></a> {} &middot; {} &middot; {} &middot; {} \
             coincidencias</h3>",
            game_cell(&log.row),
            escape_html(&log.row.received_day),
            escape_html(&log.row.failure_kind),
            log.found.hits
        );
        for snippet in &log.found.snippets {
            body.push_str("<table class=\"log\"><tbody>");
            for line in snippet {
                let number = line.number;
                let _ = write!(
                    body,
                    "<tr class=\"{}\"><td class=\"n\"><a href=\"/admin/logs/{id}?page={}#L{number}\">{number}</a></td>\
                     <td class=\"m\">{}</td><td>{}</td></tr>",
                    if line.hit { "hit" } else { "" },
                    (number - 1) / page_lines + 1,
                    if line.hit { "&raquo;" } else { "" },
                    line.html
                );
            }
            body.push_str("</tbody></table>");
        }
    }
    body
}

fn busy() -> Response {
    let mut response = html_page(
        StatusCode::TOO_MANY_REQUESTS,
        TITLE,
        "<p>Ya hay una búsqueda en curso en esta sesión: espera a que termine.</p>",
    );
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

pub async fn search(admin: Admin, State(state): State<AppState>, Query(params): Query<SearchParams>) -> Response {
    let Some(filter) = parse_scope(
        params.product,
        params.title,
        params.day,
        params.kind,
        params.fingerprint,
    ) else {
        return bad_request(BAD_FILTER);
    };
    let Some(query) = params.q.filter(|q| !q.is_empty()) else {
        let body = format!("{NAV}{}", search_form(&filter, ""));
        return html_page(StatusCode::OK, TITLE, &body);
    };
    if !is_query(&query) {
        return bad_request("La búsqueda debe tener de 1 a 200 caracteres, sin caracteres de control.");
    }
    if filter.base.title.is_none() && filter.base.fingerprint.is_none() {
        return bad_request(
            "Busca dentro de un juego (título CUSA00000) o de una huella: una búsqueda en todos los logs del bucket \
             no se hace.",
        );
    }
    let Some(_running) = state.log_searches.try_begin(slot_key(&admin)) else {
        return busy();
    };
    let needle = query.to_ascii_lowercase();
    match run(&state, &filter, &needle).await {
        Ok(outcome) => {
            tracing::info!(
                logs = outcome.searched,
                hits = outcome.hits,
                capped = outcome.stop.is_some(),
                "log search"
            );
            let page_lines = state.config.logs.page_lines as usize;
            html_page(StatusCode::OK, TITLE, &render(&filter, &query, &outcome, page_lines))
        }
        Err(error) => internal("log_search", &error),
    }
}
