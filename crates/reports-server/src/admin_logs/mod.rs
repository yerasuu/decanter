//! The private log viewer (#1060, #1063): the raw logs players attach to their reports, behind the admin gate. Pages
//! only read; there is no script. A log is untrusted text, rendered through `logtext` and `escape_html` with every
//! pass over it linear and the heavy ones on the blocking pool, so a hostile 5 MiB log neither breaks a page nor
//! holds a runtime worker.

mod diff;
mod search;

use std::fmt::Write as _;

use axum::extract::{Path, Query, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::admin::{Admin, ListParams as AdminParams, parse_filter};
use crate::app::{AppState, internal, not_found};
use crate::html::{escape_html, layout};
use crate::logtext::{Mark, classify, is_binary, render_line};
use crate::store::{AdminFilter, LogFilter, LogRow, Product};

pub use diff::diff;
pub use search::{Searches, search};

const TITLE: &str = "Logs adjuntos";
const LIST_PAGE: usize = 50;
const MAX_PAGE: usize = 1_000_000;
/// Characters of one line a page shows; the rest is cut with a marker and the download link.
const MAX_LINE_CHARS: usize = 2000;
/// Highlighted lines listed at the top of a log; past it the list says how many more there are.
const MAX_JUMPS: usize = 200;
const JUMP_PREVIEW_CHARS: usize = 120;

const STYLE: &str = "<style>.log{font:12px/1.35 ui-monospace,monospace;margin:.5rem 0}\
.log td{border:0;padding:0 .4rem;white-space:pre-wrap;overflow-wrap:anywhere}\
.log td.n{text-align:right;white-space:nowrap;color:#666}.log td.m{white-space:nowrap;font-weight:600;font-size:.7rem}\
tr.fault{background:#f6c9cc}tr.error{background:#fbe0de}tr.failed{background:#efe3fb}tr.calls{background:#d9e6fd}\
tr.warn{background:#fff1c2}.ctl{outline:1px dotted #b00;color:#b00}.cut{color:#b00;font-style:italic}\
.jump{font:12px/1.4 ui-monospace,monospace}tr.hit,tr.add{background:#d1f0d9}tr.del{background:#fbe0de}\
tr.chg{background:#fff1c2}tr.fold td{color:#666;font-style:italic}.diff td{width:46%}.diff td.n,.diff td.m{width:auto}\
@media(prefers-color-scheme:dark){tr.hit,tr.add{background:#1e5631}tr.del{background:#4a2422}tr.chg{background:#4a3d12}tr.fault{background:#5c1f24}tr.error{background:#4a2422}tr.failed{background:#3a2950}\
tr.calls{background:#1f3357}tr.warn{background:#4a3d12}.log td.n{color:#aaa}.ctl,.cut{color:#ff8a80}}</style>";

#[derive(Deserialize)]
pub struct ListParams {
    product: Option<String>,
    title: Option<String>,
    day: Option<String>,
    kind: Option<String>,
    fingerprint: Option<String>,
    page: Option<String>,
}

#[derive(Deserialize)]
pub struct ViewParams {
    page: Option<String>,
}

fn is_kind(text: &str) -> bool {
    (1..=16).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_lowercase())
}

/// The admin list's validation, plus the failure kind; `None` for any value of the wrong shape.
fn parse_scope(
    product: Option<String>,
    title: Option<String>,
    day: Option<String>,
    kind: Option<String>,
    fingerprint: Option<String>,
) -> Option<LogFilter> {
    let base = parse_filter(AdminParams {
        limit: None,
        offset: None,
        fingerprint,
        day,
        product,
        title,
    })?;
    let kind = match kind.filter(|text| !text.is_empty()) {
        Some(text) if is_kind(&text) => Some(text),
        Some(_) => return None,
        None => None,
    };
    Some(LogFilter { base, kind })
}

fn parse_page(raw: Option<&str>) -> Option<usize> {
    raw.filter(|value| !value.is_empty()).map_or(Some(1), |value| {
        value.parse::<usize>().ok().filter(|page| (1..=MAX_PAGE).contains(page))
    })
}

/// The scope as query pairs, built only from validated values.
fn scope_query(filter: &LogFilter) -> String {
    let base = &filter.base;
    let mut parts = Vec::with_capacity(5);
    if let Some(product) = base.product {
        parts.push(format!("product={}", product.name()));
    }
    for (name, value) in [
        ("title", &base.title),
        ("day", &base.day),
        ("kind", &filter.kind),
        ("fingerprint", &base.fingerprint),
    ] {
        if let Some(value) = value {
            parts.push(format!("{name}={value}"));
        }
    }
    parts.join("&")
}

fn list_url(filter: &LogFilter, page: usize) -> String {
    let mut query = scope_query(filter);
    if page > 1 {
        if !query.is_empty() {
            query.push('&');
        }
        let _ = write!(query, "page={page}");
    }
    if query.is_empty() {
        "/admin/logs".to_owned()
    } else {
        format!("/admin/logs?{query}")
    }
}

/// An admin page: never stored by a browser or a proxy, whatever authenticated it.
fn html_page(status: StatusCode, title: &str, body: &str) -> Response {
    let mut response = (status, Html(layout(title, &format!("{STYLE}{body}")))).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn bad_request(message: &str) -> Response {
    let body = format!(
        "<p>{}</p><p><a href=\"/admin/logs\">Volver a los logs</a></p>",
        escape_html(message)
    );
    html_page(StatusCode::BAD_REQUEST, TITLE, &body)
}

const BAD_FILTER: &str = "Algún filtro no tiene la forma aceptada (huella de 16 hex, día AAAA-MM-DD, producto decanter o \
                          psport, título CUSA00000, tipo en minúsculas, página dentro del rango).";

fn size_text(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let (unit, scale) = if bytes < 1024 * 1024 {
        ("KiB", 1024)
    } else {
        ("MiB", 1024 * 1024)
    };
    let tenths = bytes * 10 / scale;
    format!("{}.{} {unit}", tenths / 10, tenths % 10)
}

fn game_cell(row: &LogRow) -> String {
    let title = row.title_id.as_deref().map_or_else(
        || "&mdash;".to_owned(),
        |title| format!("<code>{}</code>", escape_html(title)),
    );
    match &row.game_name {
        Some(name) => format!("{title} {}", escape_html(name)),
        None => title,
    }
}

const NAV: &str = "<p><a href=\"/admin\">Informes</a> &middot; <a href=\"/admin/logs\">Logs adjuntos</a></p>";

fn filter_form(filter: &LogFilter) -> String {
    let text = |value: &Option<String>| escape_html(value.as_deref().unwrap_or_default());
    let selected = |product| {
        if filter.base.product == Some(product) {
            " selected"
        } else {
            ""
        }
    };
    format!(
        "<form method=\"get\" action=\"/admin/logs\">\
         <label>Producto<select name=\"product\"><option value=\"\">todos</option>\
         <option value=\"decanter\"{}>decanter</option><option value=\"psport\"{}>psport</option></select></label>\
         <label>Título<input name=\"title\" value=\"{}\" maxlength=\"9\" placeholder=\"CUSA00000\"></label>\
         <label>Día recibido<input name=\"day\" value=\"{}\" maxlength=\"10\" placeholder=\"AAAA-MM-DD\"></label>\
         <label>Tipo<input name=\"kind\" value=\"{}\" maxlength=\"16\" placeholder=\"crash\"></label>\
         <label>Huella<input name=\"fingerprint\" value=\"{}\" maxlength=\"16\"></label>\
         <button type=\"submit\">Filtrar</button></form>",
        selected(Product::Decanter),
        selected(Product::Psport),
        text(&filter.base.title),
        text(&filter.base.day),
        text(&filter.kind),
        text(&filter.base.fingerprint),
    )
}

/// The search form over `filter`'s scope; a search needs a title or a fingerprint in it.
fn search_form(filter: &LogFilter, query: &str) -> String {
    let text = |value: &Option<String>| escape_html(value.as_deref().unwrap_or_default());
    format!(
        "<form method=\"get\" action=\"/admin/logs/search\">\
         <label>Buscar texto<input name=\"q\" value=\"{}\" maxlength=\"{}\" required placeholder=\"Guest fault\"></label>\
         <label>Título<input name=\"title\" value=\"{}\" maxlength=\"9\" placeholder=\"CUSA00000\"></label>\
         <label>Huella<input name=\"fingerprint\" value=\"{}\" maxlength=\"16\"></label>\
         <input type=\"hidden\" name=\"product\" value=\"{}\"><input type=\"hidden\" name=\"day\" value=\"{}\">\
         <input type=\"hidden\" name=\"kind\" value=\"{}\"><button type=\"submit\">Buscar en los logs</button></form>",
        escape_html(query),
        search::MAX_QUERY_CHARS,
        text(&filter.base.title),
        text(&filter.base.fingerprint),
        filter.base.product.map_or("", Product::name),
        text(&filter.base.day),
        text(&filter.kind),
    )
}

fn list_row(row: &LogRow) -> String {
    let id = escape_html(&row.id);
    format!(
        "<tr><td><a href=\"/admin/logs/{id}\"><code>{id}</code></a></td><td>{}</td><td>{}</td><td>{}</td>\
         <td>{}</td><td><code>{}</code></td><td>{}</td><td><a href=\"/admin/logs/{id}\">ver</a> &middot; \
         <a href=\"/r/{id}\">informe</a> &middot; <a href=\"/admin/reports/{id}/attachment\">descargar</a></td>\
         <td><input type=\"radio\" name=\"a\" value=\"{id}\" title=\"A\"></td>\
         <td><input type=\"radio\" name=\"b\" value=\"{id}\" title=\"B\"></td></tr>",
        escape_html(&row.product),
        game_cell(row),
        escape_html(&row.received_day),
        escape_html(&row.failure_kind),
        escape_html(&row.template_id),
        row.size.map_or_else(|| "&mdash;".to_owned(), size_text),
    )
}

fn render_list(filter: &LogFilter, rows: &[LogRow], page: usize, has_next: bool) -> String {
    let mut body = String::from(NAV);
    body.push_str(&filter_form(filter));
    body.push_str(&search_form(filter, ""));
    if rows.is_empty() {
        body.push_str("<p>No hay logs adjuntos que coincidan.</p>");
    } else {
        body.push_str(
            "<form method=\"get\" action=\"/admin/logs/diff\"><table><thead><tr><th>Informe</th><th>Producto</th>\
             <th>Título y juego</th><th>Recibido</th><th>Tipo</th><th>Plantilla</th><th>Tamaño</th><th>Enlaces</th>\
             <th>A</th><th>B</th></tr></thead><tbody>",
        );
        for row in rows {
            body.push_str(&list_row(row));
        }
        body.push_str(
            "</tbody></table><label><span><input type=\"checkbox\" name=\"normalize\" value=\"1\" checked> ignorar \
             horas, direcciones y números largos</span></label><button type=\"submit\">Comparar A con B</button></form>",
        );
    }
    body.push_str("<p>");
    if page > 1 {
        let _ = write!(
            body,
            "<a href=\"{}\">&larr; Anterior</a> ",
            escape_html(&list_url(filter, page - 1))
        );
    }
    let _ = write!(body, "Página {page}");
    if has_next {
        let _ = write!(
            body,
            " <a href=\"{}\">Siguiente &rarr;</a>",
            escape_html(&list_url(filter, page + 1))
        );
    }
    body.push_str("</p>");
    body
}

pub async fn list(_: Admin, State(state): State<AppState>, Query(params): Query<ListParams>) -> Response {
    let Some(page) = parse_page(params.page.as_deref()) else {
        return bad_request(BAD_FILTER);
    };
    let Some(filter) = parse_scope(
        params.product,
        params.title,
        params.day,
        params.kind,
        params.fingerprint,
    ) else {
        return bad_request(BAD_FILTER);
    };
    let skip = (page - 1) * LIST_PAGE;
    let mut rows = match state.stores.logs(&filter, skip, LIST_PAGE + 1).await {
        Ok(rows) => rows,
        Err(error) => return internal("logs_list", &error),
    };
    let has_next = rows.len() > LIST_PAGE;
    rows.truncate(LIST_PAGE);
    if let Err(error) = state.stores.with_sizes(&mut rows).await {
        return internal("logs_list", &error);
    }
    html_page(StatusCode::OK, TITLE, &render_list(&filter, &rows, page, has_next))
}

fn meta_table(row: &LogRow, size: usize) -> String {
    let id = escape_html(&row.id);
    let fingerprint = escape_html(&row.fingerprint);
    format!(
        "<table><tbody><tr><th>Informe</th><td><a href=\"/r/{id}\"><code>{id}</code></a></td></tr>\
         <tr><th>Producto</th><td>{}</td></tr><tr><th>Título y juego</th><td>{}</td></tr>\
         <tr><th>Recibido</th><td>{}</td></tr><tr><th>Tipo</th><td>{}</td></tr>\
         <tr><th>Plantilla</th><td><code>{}</code></td></tr>\
         <tr><th>Huella</th><td><a href=\"/admin/logs?fingerprint={fingerprint}\"><code>{fingerprint}</code></a></td></tr>\
         <tr><th>Tamaño</th><td>{}</td></tr></tbody></table>\
         <p><a href=\"/admin/reports/{id}/attachment\">Descargar el log en bruto</a></p>",
        escape_html(&row.product),
        game_cell(row),
        escape_html(&row.received_day),
        escape_html(&row.failure_kind),
        escape_html(&row.template_id),
        size_text(size as u64),
    )
}

/// The page of the log at `page` (from 1); `None` when the log has no such page. One pass over the whole log for
/// the counts and the jump list, rendering only the lines of the page.
#[must_use]
pub fn render_log(row: &LogRow, bytes: &[u8], page: usize, page_lines: usize) -> Option<String> {
    let mut body = String::from(NAV);
    let _ = write!(body, "<h2>Log de <code>{}</code></h2>", escape_html(&row.id));
    body.push_str(&meta_table(row, bytes.len()));
    let scope = LogFilter {
        base: AdminFilter {
            title: row.title_id.clone(),
            fingerprint: row.title_id.is_none().then(|| row.fingerprint.clone()),
            ..AdminFilter::default()
        },
        kind: None,
    };
    body.push_str(&search_form(&scope, ""));
    let _ = write!(
        body,
        "<form method=\"get\" action=\"/admin/logs/diff\"><input type=\"hidden\" name=\"a\" value=\"{}\">\
         <label>Comparar con el log<input name=\"b\" maxlength=\"33\" required placeholder=\"p…\"></label>\
         <label><span><input type=\"checkbox\" name=\"normalize\" value=\"1\" checked> ignorar horas, direcciones y \
         números largos</span></label><button type=\"submit\">Comparar</button></form>",
        escape_html(&row.id)
    );
    if is_binary(bytes) {
        let _ = write!(
            body,
            "<p>El adjunto parece binario ({}), no texto: descárgalo para verlo.</p>",
            size_text(bytes.len() as u64)
        );
        return (page == 1).then_some(body);
    }
    let id = escape_html(&row.id);
    let text = String::from_utf8_lossy(bytes);
    let first = (page - 1) * page_lines;
    let mut table = String::new();
    let mut jumps = String::new();
    let mut counts = [0_usize; Mark::ALL.len()];
    let mut total = 0;
    for (index, line) in text.lines().enumerate() {
        total = index + 1;
        let mark = classify(line);
        if let Some(mark) = mark {
            let slot = Mark::ALL.iter().position(|m| *m == mark).unwrap_or_default();
            if let Some(count) = counts.get_mut(slot) {
                *count += 1;
            }
            if counts.iter().sum::<usize>() <= MAX_JUMPS {
                let _ = write!(
                    jumps,
                    "<li><a href=\"/admin/logs/{id}?page={}#L{total}\">{total}</a> [{}] ",
                    index / page_lines + 1,
                    mark.label()
                );
                render_line(line, JUMP_PREVIEW_CHARS, &mut jumps);
                jumps.push_str("</li>");
            }
        }
        if (first..first + page_lines).contains(&index) {
            let _ = write!(
                table,
                "<tr id=\"L{total}\" class=\"{}\"><td class=\"n\"><a href=\"#L{total}\">{total}</a></td>\
                 <td class=\"m\">{}</td><td>",
                mark.map_or("", Mark::class),
                mark.map_or("", Mark::label)
            );
            let cut = render_line(line, MAX_LINE_CHARS, &mut table);
            if cut > 0 {
                let _ = write!(
                    table,
                    "<span class=\"cut\"> &hellip; [línea cortada: {cut} bytes más; \
                     <a href=\"/admin/reports/{id}/attachment\">descarga el log</a>]</span>"
                );
            }
            table.push_str("</td></tr>");
        }
    }
    let pages = total.div_ceil(page_lines).max(1);
    if page > pages {
        return None;
    }
    let _ = write!(
        body,
        "<p>{total} líneas en {pages} páginas de {page_lines}. Resaltadas:"
    );
    for (mark, count) in Mark::ALL.iter().zip(counts) {
        let _ = write!(body, " {} {count};", mark.label());
    }
    body.push_str("</p>");
    let highlighted: usize = counts.iter().sum();
    if highlighted > 0 {
        let _ = write!(body, "<h3>Líneas resaltadas</h3><ol class=\"jump\">{jumps}</ol>");
        if highlighted > MAX_JUMPS {
            let _ = write!(body, "<p>Y {} más.</p>", highlighted - MAX_JUMPS);
        }
    }
    let nav_pages = log_pager(&id, page, pages);
    let _ = write!(
        body,
        "{nav_pages}<table class=\"log\"><tbody>{table}</tbody></table>{nav_pages}"
    );
    Some(body)
}

fn log_pager(id: &str, page: usize, pages: usize) -> String {
    let mut out = String::from("<p>");
    if page > 1 {
        let _ = write!(
            out,
            "<a href=\"/admin/logs/{id}?page={}\">&larr; Anterior</a> ",
            page - 1
        );
    }
    let _ = write!(out, "Página {page} de {pages}");
    if page < pages {
        let _ = write!(
            out,
            " <a href=\"/admin/logs/{id}?page={}\">Siguiente &rarr;</a>",
            page + 1
        );
    }
    out.push_str("</p>");
    out
}

/// One log, numbered and highlighted; the uniform 404 for an unknown, hidden or unattached id.
pub async fn view(
    _: Admin,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<ViewParams>,
) -> Response {
    let Some(page) = parse_page(params.page.as_deref()) else {
        return bad_request("La página no es un número válido.");
    };
    let (row, bytes) = match state.stores.log(&id).await {
        Ok(Some(found)) => found,
        Ok(None) => return not_found(),
        Err(error) => return internal("log_read", &error),
    };
    let page_lines = state.config.logs.page_lines as usize;
    let rendered = tokio::task::spawn_blocking(move || render_log(&row, &bytes, page, page_lines)).await;
    match rendered {
        Ok(Some(body)) => html_page(StatusCode::OK, TITLE, &body),
        Ok(None) => bad_request("El log no tiene esa página."),
        Err(error) => internal("log_render", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_binary_units() {
        assert_eq!(size_text(12), "12 B");
        assert_eq!(size_text(1536), "1.5 KiB");
        assert_eq!(size_text(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn scope_accepts_empty_values_and_refuses_other_shapes() {
        let empty = || Some(String::new());
        assert!(parse_scope(empty(), empty(), empty(), empty(), empty()).is_some());
        assert!(parse_scope(None, None, None, Some("crash".to_owned()), None).is_some());
        for kind in ["Crash", "a b", "x".repeat(17).as_str(), "<b>"] {
            assert!(
                parse_scope(None, None, None, Some(kind.to_owned()), None).is_none(),
                "{kind}"
            );
        }
    }
}
