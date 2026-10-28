//! The audit trail's two ends on the server: [`record`], called once a change has landed, and `/admin/audit`, the
//! admins' page of it (JSON at `/admin/audit/records` for scripts). Records live in the accounts store
//! (`store::audit`); the log lines of each change stay as they were.

use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::app::{AppState, api_error, internal};
use crate::html::{escape_html, layout};
use crate::session::{Admin, CanChange};
use crate::store::{AccountStore, AuditRecord, Role, current_minute, is_audit_id};

pub const PAGE_SIZE: usize = 100;
const TITLE: &str = "Registro de cambios";
/// The `role` of a record made by the `user` subcommand, and its `user`, since the CLI runs as no account.
pub const CLI: &str = "cli";

#[derive(Clone, Copy)]
pub enum Action {
    Hide,
    Restore,
    Resolve,
    Delete,
    HideGroup,
    RestoreGroup,
    ResolveGroup,
    DeleteGroup,
    Login,
    Logout,
    UserAdd(Role),
    UserRemove,
    UserReset,
    UserRole(Role),
}

impl Action {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::Restore => "restore",
            Self::Resolve => "resolve",
            Self::Delete => "delete",
            Self::HideGroup => "hide_group",
            Self::RestoreGroup => "restore_group",
            Self::ResolveGroup => "resolve_group",
            Self::DeleteGroup => "delete_group",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::UserAdd(Role::Admin) => "user_add_admin",
            Self::UserAdd(Role::Viewer) => "user_add_viewer",
            Self::UserRemove => "user_remove",
            Self::UserReset => "user_reset",
            Self::UserRole(Role::Admin) => "user_role_admin",
            Self::UserRole(Role::Viewer) => "user_role_viewer",
        }
    }
}

/// The Spanish label of a stored action; an action this version does not know is shown as stored.
fn label(action: &str) -> Option<&'static str> {
    Some(match action {
        "hide" => "ocultar el informe",
        "restore" => "restaurar el informe",
        "resolve" => "resolver el informe",
        "delete" => "borrar el informe",
        "hide_group" => "ocultar el grupo",
        "restore_group" => "restaurar el grupo",
        "resolve_group" => "resolver el grupo",
        "delete_group" => "borrar el grupo",
        "login" => "iniciar sesión",
        "logout" => "cerrar sesión",
        "user_add_admin" => "crear cuenta de administración",
        "user_add_viewer" => "crear cuenta de lectura",
        "user_remove" => "borrar cuenta",
        "user_reset" => "nueva clave",
        "user_role_admin" => "pasar a administración",
        "user_role_viewer" => "pasar a lectura",
        _ => return None,
    })
}

/// The record of a change that has already landed, made now.
///
/// # Errors
///
/// The bucket fails or the random source is unavailable.
pub async fn append(
    store: &AccountStore,
    (user, role): (&str, &str),
    action: Action,
    target: &str,
    count: u32,
) -> anyhow::Result<()> {
    let record = AuditRecord {
        user: user.to_owned(),
        role: role.to_owned(),
        action: action.name().to_owned(),
        target: target.to_owned(),
        count,
        at: String::new(),
    };
    store.append_audit(&record, current_minute()).await
}

/// [`append`] for the server: a failure is logged and the change it records stands.
pub async fn record(state: &AppState, (user, role): (&str, Role), action: Action, target: &str, count: u32) {
    if let Err(error) = append(state.accounts.store(), (user, role.name()), action, target, count).await {
        tracing::warn!(
            target: "decanter_reports::audit",
            action = action.name(),
            %error,
            "audit record not written; the change itself stands"
        );
    }
}

#[derive(Deserialize)]
pub struct AuditParams {
    after: Option<String>,
}

/// The cursor, when it has the one shape the listing hands out; `Err` for anything else.
fn cursor(params: AuditParams) -> Result<Option<String>, ()> {
    match params.after.filter(|text| !text.is_empty()) {
        Some(text) if is_audit_id(&text) => Ok(Some(text)),
        Some(_) => Err(()),
        None => Ok(None),
    }
}

async fn page_of(state: &AppState, after: Option<&str>) -> anyhow::Result<(Vec<AuditRecord>, Option<String>)> {
    state.accounts.store().audit_page(after, PAGE_SIZE).await
}

fn when(at: &str) -> String {
    at.strip_suffix('Z').map_or_else(
        || at.to_owned(),
        |minute| format!("{} UTC", minute.replacen('T', " ", 1)),
    )
}

fn row(record: &AuditRecord) -> String {
    let action = label(&record.action).map_or_else(|| escape_html(&record.action), str::to_owned);
    let count = if record.count > 1 {
        format!(" ({} informes)", record.count)
    } else {
        String::new()
    };
    let target = if record.target.is_empty() {
        "&mdash;".to_owned()
    } else {
        format!("<code>{}</code>", escape_html(&record.target))
    };
    format!(
        "<tr><td>{}</td><td>{} ({})</td><td>{action}{count}</td><td>{target}</td></tr>",
        escape_html(&when(&record.at)),
        escape_html(&record.user),
        escape_html(&record.role),
    )
}

fn render(records: &[AuditRecord], first: bool, next: Option<&str>) -> String {
    let mut body = String::from(
        "<p><a href=\"/admin\">Volver a los informes</a>. Cada cambio de un mantenedor, el más reciente primero, \
         con la hora en UTC al minuto.</p>",
    );
    if records.is_empty() {
        body.push_str("<p>No hay cambios registrados.</p>");
    } else {
        body.push_str(
            "<table><thead><tr><th>Cuándo</th><th>Quién</th><th>Acción</th><th>Objetivo</th></tr></thead><tbody>",
        );
        for record in records {
            body.push_str(&row(record));
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("<p>");
    if !first {
        body.push_str("<a href=\"/admin/audit\">&larr; Más recientes</a> ");
    }
    if let Some(next) = next {
        let _ = write!(
            body,
            "<a href=\"/admin/audit?after={}\">Anteriores &rarr;</a>",
            escape_html(next)
        );
    }
    body.push_str("</p>");
    body
}

fn bad_cursor_page() -> Response {
    let body = "<p>La página pedida no tiene la forma aceptada.</p><p><a href=\"/admin/audit\">Volver</a></p>";
    (StatusCode::BAD_REQUEST, Html(layout(TITLE, body))).into_response()
}

pub async fn page(
    _: Admin,
    _: CanChange,
    State(state): State<AppState>,
    Query(params): Query<AuditParams>,
) -> Response {
    let Ok(after) = cursor(params) else {
        return bad_cursor_page();
    };
    match page_of(&state, after.as_deref()).await {
        Ok((records, next)) => Html(layout(TITLE, &render(&records, after.is_none(), next.as_deref()))).into_response(),
        Err(error) => internal("audit_page", &error),
    }
}

pub async fn records(_: CanChange, State(state): State<AppState>, Query(params): Query<AuditParams>) -> Response {
    let Ok(after) = cursor(params) else {
        return api_error(StatusCode::BAD_REQUEST, "invalid_cursor");
    };
    match page_of(&state, after.as_deref()).await {
        Ok((records, next)) => axum::Json(json!({ "records": records, "next": next })).into_response(),
        Err(error) => internal("audit_records", &error),
    }
}
