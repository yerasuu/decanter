//! The maintainer's page for a browser: the admin list with its filters, and every action as a form that carries
//! the session's CSRF token. No script; the JSON routes stay for tools.

use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::admin::{Admin, ListParams, parse_filter};
use crate::app::{AppState, internal};
use crate::html::{escape_html, layout};
use crate::session::CSRF_FIELD;
use crate::store::{AdminFilter, AdminRow, Product};

const PAGE_SIZE: u32 = 50;
const MAX_PAGE: u32 = 10_000;
const TITLE: &str = "Administración de informes";

#[derive(Deserialize)]
pub struct PageParams {
    fingerprint: Option<String>,
    day: Option<String>,
    product: Option<String>,
    title: Option<String>,
    page: Option<String>,
}

fn parse_page(raw: Option<&str>) -> Option<u32> {
    raw.filter(|value| !value.is_empty()).map_or(Some(1), |value| {
        value.parse::<u32>().ok().filter(|page| (1..=MAX_PAGE).contains(page))
    })
}

/// Built only from validated values, so it is safe as a return path; still escaped where it lands in markup.
fn admin_url(filter: &AdminFilter, page: u32) -> String {
    let mut parts = Vec::with_capacity(5);
    if let Some(product) = filter.product {
        parts.push(format!("product={}", product.name()));
    }
    for (name, value) in [
        ("title", &filter.title),
        ("day", &filter.day),
        ("fingerprint", &filter.fingerprint),
    ] {
        if let Some(value) = value {
            parts.push(format!("{name}={value}"));
        }
    }
    if page > 1 {
        parts.push(format!("page={page}"));
    }
    if parts.is_empty() {
        "/admin".to_owned()
    } else {
        format!("/admin?{}", parts.join("&"))
    }
}

/// One POST button. `confirm` adds a checkbox the browser requires before sending, for what cannot be undone.
fn action(path: &str, label: &str, csrf: &str, return_to: &str, confirm: bool) -> String {
    let check = if confirm {
        "<input type=\"checkbox\" name=\"confirm\" required title=\"confirmar\">"
    } else {
        ""
    };
    format!(
        "<form method=\"post\" action=\"{}\" style=\"display:inline-flex;gap:.2rem\">\
         <input type=\"hidden\" name=\"{CSRF_FIELD}\" value=\"{}\"><input type=\"hidden\" name=\"return\" value=\"{}\">\
         {check}<button type=\"submit\">{}</button></form>",
        escape_html(path),
        escape_html(csrf),
        escape_html(return_to),
        escape_html(label),
    )
}

fn actions(base: &str, csrf: &str, return_to: &str, hidden: bool, resolved: bool) -> String {
    let mut out = if hidden {
        action(&format!("{base}/restore"), "restaurar", csrf, return_to, false)
    } else {
        action(&format!("{base}/hide"), "ocultar", csrf, return_to, false)
    };
    if !resolved {
        out.push_str(&action(&format!("{base}/resolve"), "resolver", csrf, return_to, false));
    }
    out.push_str(&action(&format!("{base}/delete"), "borrar", csrf, return_to, true));
    out
}

const fn selected(current: bool) -> &'static str {
    if current { " selected" } else { "" }
}

fn filter_form(filter: &AdminFilter) -> String {
    let text = |value: &Option<String>| escape_html(value.as_deref().unwrap_or_default());
    format!(
        "<form method=\"get\" action=\"/admin\">\
         <label>Producto<select name=\"product\"><option value=\"\">todos</option>\
         <option value=\"decanter\"{}>decanter</option><option value=\"psport\"{}>psport</option></select></label>\
         <label>Título<input name=\"title\" value=\"{}\" maxlength=\"9\" placeholder=\"CUSA00000\"></label>\
         <label>Día recibido<input name=\"day\" value=\"{}\" maxlength=\"10\" placeholder=\"AAAA-MM-DD\"></label>\
         <label>Huella<input name=\"fingerprint\" value=\"{}\" maxlength=\"16\"></label>\
         <button type=\"submit\">Filtrar</button></form>",
        selected(filter.product == Some(Product::Decanter)),
        selected(filter.product == Some(Product::Psport)),
        text(&filter.title),
        text(&filter.day),
        text(&filter.fingerprint),
    )
}

fn row(report: &AdminRow, csrf: &str, return_to: &str) -> String {
    let id = escape_html(&report.id);
    let fingerprint = escape_html(&report.fingerprint);
    let mut state = String::new();
    if report.hidden {
        state.push_str("oculto ");
    }
    match &report.resolved_at {
        Some(day) => {
            let _ = write!(state, "<span class=\"mark\">resuelto el {}</span>", escape_html(day));
        }
        None => state.push_str("abierto"),
    }
    let attachment = if report.has_attachment {
        format!("<a href=\"/admin/reports/{id}/attachment\">descargar</a>")
    } else {
        "&mdash;".to_owned()
    };
    format!(
        "<tr><td><a href=\"/r/{id}\"><code>{id}</code></a></td><td>{}</td><td>{}</td><td>{}</td>\
         <td><code>{}</code></td><td>{}</td><td>{}</td><td><a href=\"/admin?fingerprint={fingerprint}\">\
         <code>{fingerprint}</code></a></td><td>{state}</td><td>{attachment}</td><td>{}</td></tr>",
        escape_html(&report.product),
        report.title_id.as_deref().map_or_else(
            || "&mdash;".to_owned(),
            |title| format!("<code>{}</code>", escape_html(title))
        ),
        escape_html(&report.failure_kind),
        escape_html(&report.template_id),
        escape_html(&report.received_day),
        escape_html(&report.created_day),
        actions(
            &format!("/admin/reports/{}", report.id),
            csrf,
            return_to,
            report.hidden,
            report.resolved_at.is_some()
        ),
    )
}

fn render(filter: &AdminFilter, rows: &[AdminRow], page: u32, has_next: bool, csrf: &str) -> String {
    let return_to = admin_url(filter, page);
    let mut body = String::new();
    if !csrf.is_empty() {
        let _ = write!(
            body,
            "<form method=\"post\" action=\"/admin/logout\"><input type=\"hidden\" name=\"{CSRF_FIELD}\" \
             value=\"{}\"><button type=\"submit\">Cerrar sesión</button></form>",
            escape_html(csrf)
        );
    }
    body.push_str("<p><a href=\"/admin/logs\">Logs adjuntos</a>: ver, buscar y comparar los logs de los informes</p>");
    body.push_str(&filter_form(filter));
    if let Some(fingerprint) = &filter.fingerprint {
        let base = format!("/admin/groups/{fingerprint}");
        let _ = write!(
            body,
            "<h2>Grupo <code>{}</code></h2><div>Todos sus informes, ocultos incluidos, en los dos productos: {}{}{}{}</div>",
            escape_html(fingerprint),
            action(&format!("{base}/hide"), "ocultar el grupo", csrf, &return_to, false),
            action(
                &format!("{base}/restore"),
                "restaurar el grupo",
                csrf,
                &return_to,
                false
            ),
            action(&format!("{base}/resolve"), "resolver el grupo", csrf, &return_to, false),
            action(&format!("{base}/delete"), "borrar el grupo", csrf, &return_to, true),
        );
    }
    if rows.is_empty() {
        body.push_str("<p>No hay informes que coincidan.</p>");
    } else {
        body.push_str(
            "<table><thead><tr><th>Informe</th><th>Producto</th><th>Título</th><th>Tipo</th><th>Plantilla</th>\
             <th>Recibido</th><th>Fecha</th><th>Huella</th><th>Estado</th><th>Adjunto</th><th>Acciones</th></tr>\
             </thead><tbody>",
        );
        for report in rows {
            body.push_str(&row(report, csrf, &return_to));
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("<p>");
    if page > 1 {
        let _ = write!(
            body,
            "<a href=\"{}\">&larr; Anterior</a> ",
            escape_html(&admin_url(filter, page - 1))
        );
    }
    let _ = write!(body, "Página {page}");
    if has_next {
        let _ = write!(
            body,
            " <a href=\"{}\">Siguiente &rarr;</a>",
            escape_html(&admin_url(filter, page + 1))
        );
    }
    body.push_str("</p>");
    body
}

fn bad_request() -> Response {
    let body = "<p>Algún filtro no tiene la forma aceptada (huella de 16 hex, día AAAA-MM-DD, producto \
                decanter o psport, título CUSA00000, página de 1 a 10000).</p><p><a href=\"/admin\">Volver</a></p>";
    (StatusCode::BAD_REQUEST, Html(layout(TITLE, body))).into_response()
}

pub async fn page(admin: Admin, State(state): State<AppState>, Query(params): Query<PageParams>) -> Response {
    let Some(page) = parse_page(params.page.as_deref()) else {
        return bad_request();
    };
    let Some(filter) = parse_filter(ListParams {
        limit: None,
        offset: None,
        fingerprint: params.fingerprint,
        day: params.day,
        product: params.product,
        title: params.title,
    }) else {
        return bad_request();
    };
    let offset = (page - 1) * PAGE_SIZE;
    let mut rows = match state.stores.admin_list(&filter, PAGE_SIZE + 1, offset).await {
        Ok(rows) => rows,
        Err(error) => return internal("admin_page", &error),
    };
    let has_next = rows.len() > PAGE_SIZE as usize;
    rows.truncate(PAGE_SIZE as usize);
    let body = render(
        &filter,
        &rows,
        page,
        has_next,
        admin.csrf.as_deref().unwrap_or_default(),
    );
    Html(layout(TITLE, &body)).into_response()
}
