//! The public pages: server-rendered, no script, every dynamic string escaped.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::app::{AppState, internal};
use crate::html::{TITLE, escape_html, fields_table, layout, message_text};
use crate::secrets::is_hex_token;
use crate::store::{Filter, GroupRow};

const MAX_PAGE: u32 = 10_000;

#[derive(Deserialize)]
pub struct ListParams {
    product: Option<String>,
    title: Option<String>,
    resolved: Option<String>,
    page: Option<String>,
}

#[derive(Deserialize)]
pub struct PageParams {
    page: Option<String>,
}

fn html_page(status: StatusCode, title: &str, body: &str) -> Response {
    (status, Html(layout(title, body))).into_response()
}

fn not_found_page() -> Response {
    html_page(
        StatusCode::NOT_FOUND,
        "No encontrado",
        "<p>No existe ese informe o grupo.</p>",
    )
}

fn bad_request(what: &str) -> Response {
    let body = format!("<p>Parámetro no válido: <code>{}</code>.</p>", escape_html(what));
    html_page(StatusCode::BAD_REQUEST, "Petición no válida", &body)
}

pub fn is_title_id(text: &str) -> bool {
    text.len() == 9
        && text.bytes().take(4).all(|b| b.is_ascii_uppercase())
        && text.bytes().skip(4).all(|b| b.is_ascii_digit())
}

fn parse_page(raw: Option<&str>) -> Option<u32> {
    raw.filter(|value| !value.is_empty()).map_or(Some(1), |value| {
        value.parse::<u32>().ok().filter(|page| (1..=MAX_PAGE).contains(page))
    })
}

/// Every parameter has one accepted form; an empty value (an untouched form field) means "no filter".
fn parse_filter(params: &ListParams) -> Result<Filter, &'static str> {
    let product = match params.product.as_deref().filter(|value| !value.is_empty()) {
        None => None,
        Some("decanter") => Some("decanter"),
        Some("psport") => Some("psport"),
        Some(_) => return Err("product"),
    };
    let title = match params.title.as_deref().filter(|value| !value.is_empty()) {
        None => None,
        Some(value) if is_title_id(value) => Some(value.to_owned()),
        Some(_) => return Err("title"),
    };
    let resolved = match params.resolved.as_deref().filter(|value| !value.is_empty()) {
        None => None,
        Some("0") => Some(false),
        Some("1") => Some(true),
        Some(_) => return Err("resolved"),
    };
    Ok(Filter {
        product,
        title,
        resolved,
    })
}

/// Built only from validated values: it is both the query string of the pager's links and the index cache key.
fn filter_key(filter: &Filter, page: u32) -> String {
    let mut parts = Vec::new();
    if let Some(product) = filter.product {
        parts.push(format!("product={product}"));
    }
    if let Some(title) = &filter.title {
        parts.push(format!("title={title}"));
    }
    if let Some(resolved) = filter.resolved {
        parts.push(format!("resolved={}", u8::from(resolved)));
    }
    if page > 1 {
        parts.push(format!("page={page}"));
    }
    parts.join("&")
}

/// Escaped anyway because it lands inside an attribute.
fn list_query(filter: &Filter, page: u32) -> String {
    let key = filter_key(filter, page);
    if key.is_empty() {
        key
    } else {
        escape_html(&format!("?{key}"))
    }
}

fn pager(base: &str, query: impl Fn(u32) -> String, page: u32, has_next: bool) -> String {
    let mut out = String::from("<p>");
    if page > 1 {
        let _ = write!(out, "<a href=\"{base}{}\">&larr; Anterior</a> ", query(page - 1));
    }
    let _ = write!(out, "Página {page}");
    if has_next {
        let _ = write!(out, " <a href=\"{base}{}\">Siguiente &rarr;</a>", query(page + 1));
    }
    out.push_str("</p>");
    out
}

const fn selected(current: bool) -> &'static str {
    if current { " selected" } else { "" }
}

fn filter_form(filter: &Filter) -> String {
    let product = filter.product;
    let resolved = filter.resolved;
    let title = escape_html(filter.title.as_deref().unwrap_or_default());
    format!(
        "<form method=\"get\" action=\"/\">\
         <label>Producto<select name=\"product\"><option value=\"\">todos</option>\
         <option value=\"decanter\"{}>decanter</option><option value=\"psport\"{}>psport</option></select></label>\
         <label>Título<input name=\"title\" value=\"{title}\" maxlength=\"9\" pattern=\"[A-Z]{{4}}[0-9]{{5}}\" placeholder=\"CUSA00000\"></label>\
         <label>Estado<select name=\"resolved\"><option value=\"\">todos</option>\
         <option value=\"0\"{}>sin resolver</option><option value=\"1\"{}>resueltos</option></select></label>\
         <button type=\"submit\">Filtrar</button></form>",
        selected(product == Some("decanter")),
        selected(product == Some("psport")),
        selected(resolved == Some(false)),
        selected(resolved == Some(true)),
    )
}

fn game_label(title_id: Option<&str>, name: Option<&str>, titles: u32) -> String {
    let Some(title_id) = title_id else {
        return "&mdash;".to_owned();
    };
    let mut label = format!("<code>{}</code>", escape_html(title_id));
    if let Some(name) = name {
        let _ = write!(label, " {}", escape_html(name));
    }
    if titles > 1 {
        let _ = write!(label, " (+{} más)", titles - 1);
    }
    label
}

fn group_row(group: &GroupRow) -> String {
    let state = if group.resolved == group.count {
        "<span class=\"mark\">resuelto</span>".to_owned()
    } else {
        format!("{} de {} resueltos", group.resolved, group.count)
    };
    format!(
        "<tr><td><a href=\"/g/{}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td>{state}</td></tr>",
        escape_html(&group.fingerprint),
        group.count,
        escape_html(&group.first_day),
        escape_html(&group.last_day),
        escape_html(&group.product),
        game_label(group.title_id.as_deref(), group.game_name.as_deref(), group.titles),
        escape_html(&group.failure_kind),
        escape_html(&group.message),
    )
}

fn render_index(filter: &Filter, groups: &[GroupRow], page: u32, has_next: bool) -> String {
    let mut body = filter_form(filter);
    if groups.is_empty() {
        body.push_str("<p>No hay informes que coincidan.</p>");
    } else {
        body.push_str(
            "<table><thead><tr><th>Informes</th><th>Primero</th><th>Último</th><th>Producto</th><th>Juego</th>\
             <th>Tipo</th><th>Mensaje</th><th>Estado</th></tr></thead><tbody>",
        );
        for group in groups {
            body.push_str(&group_row(group));
        }
        body.push_str("</tbody></table>");
    }
    body.push_str(&pager("/", |p| list_query(filter, p), page, has_next));
    body
}

async fn build_index(state: &AppState, filter: Filter, page: u32) -> Result<String, Box<Response>> {
    let size = state.config.page_size;
    let offset = (page - 1) * size;
    let mut groups = state
        .store
        .groups(&filter, size + 1, offset)
        .await
        .map_err(|error| Box::new(internal("groups", &error)))?;
    let has_next = groups.len() > size as usize;
    groups.truncate(size as usize);
    Ok(render_index(&filter, &groups, page, has_next))
}

pub async fn index(State(state): State<AppState>, Query(params): Query<ListParams>) -> Response {
    let filter = match parse_filter(&params) {
        Ok(filter) => filter,
        Err(name) => return bad_request(name),
    };
    let Some(page) = parse_page(params.page.as_deref()) else {
        return bad_request("page");
    };
    let key = filter_key(&filter, page);
    if let Some(body) = state.index_cache.get(&key, Instant::now()) {
        return html_page(StatusCode::OK, TITLE, &body);
    }
    let _gate = if state.index_cache.enabled() {
        let gate = state.index_gate.lock().await;
        if let Some(body) = state.index_cache.get(&key, Instant::now()) {
            return html_page(StatusCode::OK, TITLE, &body);
        }
        Some(gate)
    } else {
        None
    };
    match build_index(&state, filter, page).await {
        Ok(body) => {
            let response = html_page(StatusCode::OK, TITLE, &body);
            state.index_cache.put(key, Arc::from(body), Instant::now());
            response
        }
        Err(response) => *response,
    }
}

pub async fn report(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !is_hex_token(&id, 32) {
        return not_found_page();
    }
    let stored = match state.store.find_public(&id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return not_found_page(),
        Err(error) => return internal("find_public", &error),
    };
    let value: Value = serde_json::from_str(&stored.json).unwrap_or(Value::Null);
    let raw = serde_json::to_string_pretty(&value).unwrap_or_default();
    let mut body = String::new();
    if let Some(day) = &stored.resolved_at {
        let _ = write!(
            body,
            "<p><span class=\"mark\">Resuelto el {}</span></p>",
            escape_html(day)
        );
    }
    let _ = write!(
        body,
        "<p>Recibido el {}. Informe del {}. Grupo: <a href=\"/g/{}\"><code>{}</code></a>. Mensaje: <code>{}</code></p>",
        escape_html(&stored.received_day),
        escape_html(&stored.created_day),
        escape_html(&stored.fingerprint),
        escape_html(&stored.fingerprint),
        escape_html(&message_text(&value)),
    );
    body.push_str(&fields_table(&value));
    let _ = write!(body, "<h2>JSON</h2><pre>{}</pre>", escape_html(&raw));
    html_page(
        StatusCode::OK,
        &format!("Informe {}", stored.id.get(..8).unwrap_or_default()),
        &body,
    )
}

pub async fn group(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    Query(params): Query<PageParams>,
) -> Response {
    if !is_hex_token(&fingerprint, 16) {
        return not_found_page();
    }
    let Some(page) = parse_page(params.page.as_deref()) else {
        return bad_request("page");
    };
    let size = state.config.page_size;
    let offset = (page - 1) * size;
    let mut members = match state.store.group_members(&fingerprint, size + 1, offset).await {
        Ok(members) => members,
        Err(error) => return internal("group_members", &error),
    };
    if members.is_empty() {
        return not_found_page();
    }
    let has_next = members.len() > size as usize;
    members.truncate(size as usize);
    let mut body = String::from(
        "<table><thead><tr><th>Informe</th><th>Fecha</th><th>Juego</th><th>Estado</th></tr></thead><tbody>",
    );
    for member in &members {
        let state_cell = member.resolved_at.as_deref().map_or_else(
            || "abierto".to_owned(),
            |day| format!("<span class=\"mark\">resuelto el {}</span>", escape_html(day)),
        );
        let _ = write!(
            body,
            "<tr><td><a href=\"/r/{id}\"><code>{id}</code></a></td><td>{}</td><td>{}</td><td>{state_cell}</td></tr>",
            escape_html(&member.created_day),
            member
                .title_id
                .as_deref()
                .map_or_else(|| "&mdash;".to_owned(), |t| format!("<code>{}</code>", escape_html(t))),
            id = escape_html(&member.id),
        );
    }
    body.push_str("</tbody></table>");
    let base = format!("/g/{fingerprint}");
    body.push_str(&pager(
        &base,
        |p| if p > 1 { format!("?page={p}") } else { String::new() },
        page,
        has_next,
    ));
    html_page(StatusCode::OK, &format!("Grupo {fingerprint}"), &body)
}
