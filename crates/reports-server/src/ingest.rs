//! What a player's client does: upload a report, delete it with its code, attach a private log.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use decanter_report::schema::Report;
use decanter_report::validate::MAX_BYTES;
use decanter_report::{MAX_CLEAN_LINES, clean_log, fingerprint, validate};
use serde_json::{Value, json};

use crate::app::{AppState, api_error, denied, internal, not_found, rate_limit, today};
use crate::html::message_text;
use crate::limits::{Verdict, client_ip};
use crate::secrets::{self, random_hex, sha256};
use crate::store::{Attach, Change, Insert, NewReport, Product};

pub const MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
const DELETE_CODE_HEADER: &str = "x-delete-code";
const REPORT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACHMENT_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Refuses on the declared length before reading anything, and caps what is read when none is declared.
async fn read_capped(
    body: Body,
    headers: &HeaderMap,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Box<Response>> {
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > limit) {
        return Err(Box::new(api_error(StatusCode::PAYLOAD_TOO_LARGE, "too_large")));
    }
    match tokio::time::timeout(timeout, to_bytes(body, limit)).await {
        Err(_) => Err(Box::new(api_error(StatusCode::REQUEST_TIMEOUT, "timeout"))),
        Ok(Err(_)) => Err(Box::new(api_error(StatusCode::PAYLOAD_TOO_LARGE, "too_large"))),
        Ok(Ok(bytes)) => Ok(bytes.to_vec()),
    }
}

struct Meta {
    product: Product,
    title_id: Option<String>,
    game_name: Option<String>,
    failure_kind: String,
    template_id: String,
    created_day: String,
}

fn meta(report: &Report) -> Meta {
    let (product, title_id, game_name, failure, date) = match report {
        Report::Decanter(r) => (Product::Decanter, None, None, &r.failure, &r.date),
        Report::Psport(r) => (
            Product::Psport,
            Some(r.game.title_id.clone()),
            r.game.name.clone(),
            &r.failure,
            &r.date,
        ),
    };
    let kind = serde_json::to_value(failure.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let template = failure
        .message
        .as_ref()
        .map(|message| message.template.clone())
        .unwrap_or_default();
    Meta {
        product,
        title_id,
        game_name,
        failure_kind: kind,
        template_id: template,
        created_day: date.clone(),
    }
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/json"))
}

pub async fn create(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    if let Err(response) = rate_limit(&state, peer, request.headers()) {
        return *response;
    }
    if !is_json(request.headers()) {
        return api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let (parts, body) = request.into_parts();
    let bytes = match read_capped(body, &parts.headers, MAX_BYTES, REPORT_READ_TIMEOUT).await {
        Ok(bytes) => bytes,
        Err(response) => return *response,
    };
    let report = match validate(&bytes) {
        Ok(report) => report,
        Err(rejections) => {
            tracing::info!(count = rejections.len(), "report rejected");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                axum::Json(json!({ "rejections": rejections })),
            )
                .into_response();
        }
    };
    state.limiter.spend_global(Instant::now());
    let Ok(report_json) = String::from_utf8(bytes) else {
        return api_error(StatusCode::UNPROCESSABLE_ENTITY, "not_utf8");
    };
    let Meta {
        product,
        title_id,
        game_name,
        failure_kind,
        template_id,
        created_day,
    } = meta(&report);
    let (Ok(id), Ok(delete_code)) = (product.new_id(), random_hex()) else {
        return internal("random", &"entropy source unavailable");
    };
    let fingerprint = fingerprint(&report);
    let message = serde_json::from_str::<Value>(&report_json)
        .map(|value| message_text(&value))
        .unwrap_or_default();
    let stored = NewReport {
        id: id.clone(),
        delete_hash: sha256(delete_code.as_bytes()),
        fingerprint: fingerprint.clone(),
        product: product.name(),
        title_id,
        game_name,
        failure_kind,
        template_id,
        message,
        created_day,
        received_day: today(),
        report_json,
    };
    let store = state.stores.get(product);
    let product = product.name();
    match store.insert(&stored, state.config.limits.max_reports).await {
        Ok(Insert::Stored { evicted }) => {
            if evicted > 0 {
                state.index_cache.clear();
                tracing::warn!(product, evicted, "report store full: oldest unresolved reports evicted");
            }
        }
        Ok(Insert::Full) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "storage_full"),
        Err(error) => return internal("insert", &error),
    }
    tracing::info!(product, "report stored");
    let url = format!("{}/r/{id}", state.config.public_url);
    let body = json!({ "id": id, "delete_code": delete_code, "url": url, "fingerprint": fingerprint });
    (StatusCode::CREATED, axum::Json(body)).into_response()
}

/// Whether `code` is the delete code of report `id`. An unknown id is compared against a zero hash so the
/// answer and its timing are the same as for a wrong code. Every mismatch counts against the address's failure
/// budget and an address over it is answered 429 before anything is looked up.
async fn code_matches(
    state: &AppState,
    peer: SocketAddr,
    id: &str,
    headers: &HeaderMap,
) -> Result<bool, Box<Response>> {
    let ip = client_ip(peer, headers, &state.config);
    if let Verdict::Denied { retry_after } = state.limiter.check_failures(ip, Instant::now()) {
        return Err(Box::new(denied(retry_after)));
    }
    let matched = code_matches_hash(state, id, headers).await?;
    if !matched {
        state.limiter.record_failure(ip, Instant::now());
    }
    Ok(matched)
}

async fn code_matches_hash(state: &AppState, id: &str, headers: &HeaderMap) -> Result<bool, Box<Response>> {
    let code = headers
        .get(DELETE_CODE_HEADER)
        .map(axum::http::HeaderValue::as_bytes)
        .unwrap_or_default();
    // A malformed id names no partition; an id of the other product is simply unknown in this one.
    let Some(store) = state.stores.of_id(id) else {
        let _ = secrets::matches(&[0; 32], code);
        return Ok(false);
    };
    match store.delete_hash(id).await {
        Ok(Some(hash)) => Ok(secrets::matches_hex(&hash, code)),
        Ok(None) => {
            let _ = secrets::matches(&[0; 32], code);
            Ok(false)
        }
        Err(error) => Err(Box::new(internal("delete_hash", &error))),
    }
}

pub async fn remove(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match code_matches(&state, peer, &id, &headers).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(response) => return *response,
    }
    let Some(store) = state.stores.of_id(&id) else {
        return not_found();
    };
    match store.change(&[id], &Change::Delete).await {
        Ok(changed) if changed.matched == 0 => not_found(),
        Ok(_) => {
            state.index_cache.clear();
            tracing::info!("report deleted by its owner");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => internal("delete", &error),
    }
}

/// The stored form of the attachment's public clean log (decision #1060), built from the allow-list alone. A NUL
/// byte means the attachment is not a text log, and matching lossily decoded binary against the templates could
/// only produce noise, so it gives an empty one.
fn public_log(bytes: &[u8], product: Product) -> Result<Vec<u8>, serde_json::Error> {
    let text = if bytes.contains(&0) {
        String::new()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    serde_json::to_vec(&clean_log(&text, product.schema(), MAX_CLEAN_LINES))
}

pub async fn attach(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    if let Err(response) = rate_limit(&state, peer, request.headers()) {
        return *response;
    }
    match code_matches(&state, peer, &id, request.headers()).await {
        Ok(true) => state.limiter.spend_global(Instant::now()),
        Ok(false) => return not_found(),
        Err(response) => return *response,
    }
    let (parts, body) = request.into_parts();
    let bytes = match read_capped(body, &parts.headers, MAX_ATTACHMENT_BYTES, ATTACHMENT_READ_TIMEOUT).await {
        Ok(bytes) if bytes.is_empty() => return api_error(StatusCode::BAD_REQUEST, "empty"),
        Ok(bytes) => bytes,
        Err(response) => return *response,
    };
    let Some(product) = Product::of_id(&id) else {
        return not_found();
    };
    let (bytes, clean) = match tokio::task::spawn_blocking(move || {
        let clean = public_log(&bytes, product);
        (bytes, clean)
    })
    .await
    {
        Ok((bytes, Ok(clean))) => (bytes, clean),
        Ok((_, Err(error))) => return internal("clean_log", &error),
        Err(error) => return internal("clean_log", &error),
    };
    match state
        .stores
        .get(product)
        .attach(&id, &bytes, &clean, state.config.limits.max_attachments)
        .await
    {
        Ok(Attach::Stored { evicted }) => {
            if evicted > 0 {
                tracing::warn!(
                    product = product.name(),
                    evicted,
                    "attachment store full: oldest attachments evicted"
                );
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Attach::NotFound) => not_found(),
        Ok(Attach::Exists | Attach::Resolved) => api_error(StatusCode::CONFLICT, "attachment_not_accepted"),
        Ok(Attach::Full) => api_error(StatusCode::SERVICE_UNAVAILABLE, "storage_full"),
        Err(error) => internal("attach", &error),
    }
}
