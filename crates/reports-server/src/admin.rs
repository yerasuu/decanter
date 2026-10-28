//! The maintainer's side. Without the key or a session every route here answers 404, as if it did not exist; the
//! check is `session::gate`, and [`Admin`] is the proof it passed.

use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use serde::Deserialize;

use crate::app::{AppState, api_error, internal, not_found, today};
use crate::pages::is_title_id;
use crate::secrets::is_hex_token;
pub use crate::session::Admin;
use crate::store::{AdminFilter, Change, Product};

const MAX_LIST: u32 = 500;

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub fingerprint: Option<String>,
    pub day: Option<String>,
    pub product: Option<String>,
    pub title: Option<String>,
}

fn is_day(text: &str) -> bool {
    text.len() == 10 && NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
}

/// An empty value means "no filter", as on the public form; anything else must have the one accepted shape.
pub fn parse_filter(params: ListParams) -> Option<AdminFilter> {
    let given = |value: Option<String>| value.filter(|text| !text.is_empty());
    let fingerprint = match given(params.fingerprint) {
        Some(text) if is_hex_token(&text, 16) => Some(text),
        Some(_) => return None,
        None => None,
    };
    let day = match given(params.day) {
        Some(text) if is_day(&text) => Some(text),
        Some(_) => return None,
        None => None,
    };
    let product = match given(params.product) {
        None => None,
        Some(text) => Some(Product::from_name(&text)?),
    };
    let title = match given(params.title) {
        Some(text) if is_title_id(&text) => Some(text),
        Some(_) => return None,
        None => None,
    };
    Some(AdminFilter {
        fingerprint,
        day,
        product,
        title,
    })
}

pub async fn list(_: Admin, State(state): State<AppState>, Query(params): Query<ListParams>) -> Response {
    let limit = params.limit.unwrap_or(100).clamp(1, MAX_LIST);
    let offset = params.offset.unwrap_or(0);
    let Some(filter) = parse_filter(params) else {
        return api_error(StatusCode::BAD_REQUEST, "invalid_filter");
    };
    match state.stores.admin_list(&filter, limit, offset).await {
        Ok(rows) => axum::Json(rows).into_response(),
        Err(error) => internal("admin_list", &error),
    }
}

/// Applies `change` to one report; 404 for a malformed or unknown id, like every other admin miss.
async fn change_one(state: &AppState, id: String, change: Change, what: &'static str) -> Response {
    let Some(store) = state.stores.of_id(&id) else {
        return not_found();
    };
    match store.change(&[id], &change).await {
        Ok(changed) if changed.matched == 0 => not_found(),
        Ok(_) => {
            state.index_cache.clear();
            tracing::info!(action = what, "report changed by the maintainer");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => internal(what, &error),
    }
}

/// Applies `change` to every report of the group in both partitions, hidden ones included; 404 when neither holds
/// any.
async fn change_group(state: &AppState, fingerprint: &str, change: Change, what: &'static str) -> Response {
    if !is_hex_token(fingerprint, 16) {
        return not_found();
    }
    match state.stores.change_group(fingerprint, &change).await {
        Ok(changed) if changed.matched == 0 => not_found(),
        Ok(changed) => {
            state.index_cache.clear();
            tracing::info!(
                action = what,
                count = changed.matched,
                "group changed by the maintainer"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => internal(what, &error),
    }
}

pub async fn hide(_: Admin, State(state): State<AppState>, Path(id): Path<String>) -> Response {
    change_one(&state, id, Change::Hide, "hide").await
}

pub async fn restore(_: Admin, State(state): State<AppState>, Path(id): Path<String>) -> Response {
    change_one(&state, id, Change::Restore, "restore").await
}

pub async fn resolve(_: Admin, State(state): State<AppState>, Path(id): Path<String>) -> Response {
    change_one(&state, id, Change::Resolve(today()), "resolve").await
}

pub async fn remove(_: Admin, State(state): State<AppState>, Path(id): Path<String>) -> Response {
    change_one(&state, id, Change::Delete, "delete").await
}

pub async fn hide_group(_: Admin, State(state): State<AppState>, Path(fingerprint): Path<String>) -> Response {
    change_group(&state, &fingerprint, Change::Hide, "hide_group").await
}

pub async fn restore_group(_: Admin, State(state): State<AppState>, Path(fingerprint): Path<String>) -> Response {
    change_group(&state, &fingerprint, Change::Restore, "restore_group").await
}

pub async fn resolve_group(_: Admin, State(state): State<AppState>, Path(fingerprint): Path<String>) -> Response {
    change_group(&state, &fingerprint, Change::Resolve(today()), "resolve_group").await
}

pub async fn remove_group(_: Admin, State(state): State<AppState>, Path(fingerprint): Path<String>) -> Response {
    change_group(&state, &fingerprint, Change::Delete, "delete_group").await
}

/// The one place an attachment leaves the bucket: fixed type, forced download, never parsed.
pub async fn attachment(_: Admin, State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(store) = state.stores.of_id(&id) else {
        return not_found();
    };
    match store.attachment(&id).await {
        Ok(Some(bytes)) => {
            let disposition = format!("attachment; filename=\"report-{id}.bin\"");
            let mut response = bytes.into_response();
            let headers = response.headers_mut();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
            if let Ok(value) = HeaderValue::from_str(&disposition) {
                headers.insert(CONTENT_DISPOSITION, value);
            }
            response
        }
        Ok(None) => not_found(),
        Err(error) => internal("attachment_read", &error),
    }
}
