//! Shared state, the router and the headers every answer carries.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{
    ALLOW, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderName, HeaderValue, REFERRER_POLICY, RETRY_AFTER,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use serde_json::json;

use crate::cache::PageCache;
use crate::config::Config;
use crate::limits::{Limiter, Verdict, client_ip};
use crate::session::{AdminKeys, LOGIN_PATH, Sessions, is_admin_path};
use crate::store::Stores;
use crate::{admin, admin_logs, admin_ui, clean_page, ingest, pages, session};

pub struct Inner {
    pub config: Config,
    pub stores: Stores,
    pub limiter: Limiter,
    pub index_cache: PageCache,
    /// Lets one request rebuild an expired index while the rest wait for its result instead of repeating the scan.
    pub index_gate: tokio::sync::Mutex<()>,
    pub admin_keys: AdminKeys,
    pub sessions: Sessions,
    pub log_searches: admin_logs::Searches,
}

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    #[must_use]
    pub fn new(config: Config, admin_keys: AdminKeys, stores: Stores) -> Self {
        let limiter = Limiter::new(config.limits.clone());
        let index_cache = PageCache::new(config.index_cache_secs);
        let sessions = Sessions::new(&config.admin);
        Self(Arc::new(Inner {
            config,
            stores,
            limiter,
            index_cache,
            index_gate: tokio::sync::Mutex::new(()),
            admin_keys,
            sessions,
            log_searches: admin_logs::Searches::default(),
        }))
    }
}

/// A JSON answer for the API, never carrying a value the client sent.
pub fn api_error(status: StatusCode, error: &'static str) -> Response {
    (status, axum::Json(json!({ "error": error }))).into_response()
}

pub fn not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, "not_found")
}

pub fn internal(what: &'static str, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(what, %error, "request failed");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

pub fn today() -> String {
    chrono::Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

pub fn denied(retry_after: std::time::Duration) -> Response {
    let seconds = retry_after.as_secs().max(1);
    let mut response = api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

/// Counts the request against the limits, answering 429 with `Retry-After` when it is over.
///
/// # Errors
///
/// The response to send instead of handling the request.
pub fn rate_limit(state: &AppState, peer: SocketAddr, headers: &HeaderMap) -> Result<(), Box<Response>> {
    let ip = client_ip(peer, headers, &state.config);
    match state.limiter.check(ip, Instant::now()) {
        Verdict::Allowed => Ok(()),
        Verdict::Denied { retry_after } => Err(Box::new(denied(retry_after))),
    }
}

/// Page reads are counted per client apart from uploads. The health check and the admin routes (which answer 404
/// without the key and must stay indistinguishable from unknown paths) are outside it, except the login form and its
/// post, which are public and counted like a page. An address over its authentication-failure budget gets 429 on
/// every `/admin` path, known or not, so the 429 reveals no route.
async fn read_limit(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let admin = is_admin_path(path);
    if admin {
        let ip = client_ip(peer, request.headers(), &state.config);
        if let Verdict::Denied { retry_after } = state.limiter.check_failures(ip, Instant::now()) {
            return denied(retry_after);
        }
    }
    let read = matches!(*request.method(), Method::GET | Method::HEAD) && path != "/healthz" && !admin;
    let login = path == LOGIN_PATH && matches!(*request.method(), Method::GET | Method::POST);
    if read || login {
        let ip = client_ip(peer, request.headers(), &state.config);
        if let Verdict::Denied { retry_after } = state.limiter.check_read(ip, Instant::now()) {
            return denied(retry_after);
        }
    }
    next.run(request).await
}

/// Extractor rejections (a duplicated query key, a bad path) come as plain text from the framework; every error
/// leaves as the same small JSON so no framework wording or parsed input is exposed.
async fn json_errors(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let plain = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/plain"));
    if plain && response.status().is_client_error() {
        return api_error(response.status(), "bad_request");
    }
    response
}

const PAGE_CSP: &str =
    "default-src 'none'; style-src 'self' 'unsafe-inline'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let is_html = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers
        .entry(REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("no-referrer"));
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers
        .entry(CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(if is_html { "no-cache" } else { "no-store" }));
    response
}

/// A wrong method on a known path answers 404 like an unknown path, so the admin routes stay unadvertised.
async fn fallback() -> Response {
    not_found()
}

/// The `Allow` header of a method-not-allowed answer is added by the method router after its layers ran, so it can
/// only be removed from outside the whole router; left in, it names the methods of a route that answered 404.
async fn without_allow(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().remove(ALLOW);
    response
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .fallback_service(routes(state))
        .layer(middleware::from_fn(without_allow))
}

fn routes(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(pages::index))
        .route("/r/{id}", get(pages::report))
        .route("/r/{id}/log", get(clean_page::log))
        .route("/g/{fingerprint}", get(pages::group))
        .route("/v1/reports", post(ingest::create))
        .route("/v1/reports/{id}", delete(ingest::remove))
        .route("/v1/reports/{id}/attachment", put(ingest::attach))
        .route(LOGIN_PATH, get(session::login_form).post(session::login))
        .route("/admin/logout", post(session::logout))
        .route("/admin", get(admin_ui::page))
        .route("/admin/logs", get(admin_logs::list))
        .route("/admin/logs/search", get(admin_logs::search))
        .route("/admin/logs/diff", get(admin_logs::diff))
        .route("/admin/logs/{id}", get(admin_logs::view))
        .route("/admin/reports", get(admin::list))
        .route("/admin/reports/{id}", delete(admin::remove))
        .route("/admin/reports/{id}/delete", post(admin::remove))
        .route("/admin/reports/{id}/hide", post(admin::hide))
        .route("/admin/reports/{id}/restore", post(admin::restore))
        .route("/admin/reports/{id}/resolve", post(admin::resolve))
        .route("/admin/reports/{id}/attachment", get(admin::attachment))
        .route("/admin/groups/{fingerprint}", delete(admin::remove_group))
        .route("/admin/groups/{fingerprint}/delete", post(admin::remove_group))
        .route("/admin/groups/{fingerprint}/hide", post(admin::hide_group))
        .route("/admin/groups/{fingerprint}/restore", post(admin::restore_group))
        .route("/admin/groups/{fingerprint}/resolve", post(admin::resolve_group))
        .fallback(fallback)
        .method_not_allowed_fallback(fallback)
        .layer(middleware::from_fn_with_state(state.clone(), session::gate))
        .layer(middleware::from_fn_with_state(state.clone(), read_limit))
        .layer(middleware::from_fn(json_errors))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    use axum::body::{Body, to_bytes};
    use axum::extract::ConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{AppState, router};
    use crate::config::Config;
    use crate::secrets::sha256;
    use crate::session::AdminKeys;
    use crate::store::{NewReport, Product, test_stores};

    type Test = Result<(), Box<dyn Error>>;

    const JSON: &[(&str, &str)] = &[("content-type", "application/json")];

    const ADMIN_KEY: &str = "0123456789abcdef0123456789abcdef-admin";
    const DECANTER: &str = r#"{"schema":2,"product":"decanter","date":"2026-10-21","decanter":{"version":"0.3.2","commit":"d61f1ff91d29"},"system":{"os":"linux"},"action":"build","failure":{"kind":"error","message":{"template":"decanter.job.failed","slots":[{"type":"choice","value":"build"},{"type":"number","value":1}]},"count":1}}"#;
    const PRIVATE_PATH: &str = r#"{"schema":2,"product":"decanter","date":"2026-10-21","decanter":{"version":"0.3.2"},"system":{"os":"linux","os_version":"/home/deck/games"},"action":"build","failure":{"kind":"error","count":1}}"#;

    struct Harness {
        state: AppState,
        root: PathBuf,
    }

    impl Harness {
        fn new(tweak: impl FnOnce(&mut Config)) -> Result<Self, Box<dyn Error>> {
            let mut config = Config::default();
            tweak(&mut config);
            let root = std::env::temp_dir().join(format!("decanter-reports-test-{}", crate::secrets::random_hex()?));
            std::fs::create_dir_all(&root)?;
            Ok(Self {
                state: AppState::new(config, AdminKeys::single(ADMIN_KEY), test_stores(&root)),
                root,
            })
        }

        async fn send(
            &self,
            method: Method,
            uri: &str,
            headers: &[(&str, &str)],
            body: Vec<u8>,
        ) -> Result<(StatusCode, axum::http::HeaderMap, Vec<u8>), Box<dyn Error>> {
            self.send_from([203, 0, 113, 7], method, uri, headers, body).await
        }

        async fn send_from(
            &self,
            ip: [u8; 4],
            method: Method,
            uri: &str,
            headers: &[(&str, &str)],
            body: Vec<u8>,
        ) -> Result<(StatusCode, axum::http::HeaderMap, Vec<u8>), Box<dyn Error>> {
            let mut request = Request::builder().method(method).uri(uri);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            let mut request = request.body(Body::from(body))?;
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from((ip, 4000))));
            let response = router(self.state.clone()).oneshot(request).await?;
            let (parts, body) = response.into_parts();
            Ok((parts.status, parts.headers, to_bytes(body, usize::MAX).await?.to_vec()))
        }

        async fn post(&self, body: &str) -> Result<(StatusCode, Value), Box<dyn Error>> {
            let (status, _, bytes) = self
                .send(Method::POST, "/v1/reports", JSON, body.as_bytes().to_vec())
                .await?;
            Ok((status, serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
        }

        async fn stored(&self) -> Result<u32, Box<dyn Error>> {
            Ok(self.state.stores.count().await?)
        }

        async fn blobs(&self) -> Result<usize, Box<dyn Error>> {
            Ok(self.state.stores.blob_count().await?)
        }

        async fn count_in(&self, product: Product) -> Result<u32, Box<dyn Error>> {
            Ok(self.state.stores.get(product).count().await?)
        }

        /// Stores a report as if validated, with the received day the test needs.
        async fn insert_direct(
            &self,
            product: Product,
            fingerprint: &str,
            day: &str,
        ) -> Result<String, Box<dyn Error>> {
            let id = product.new_id()?;
            let new = NewReport {
                id: id.clone(),
                delete_hash: sha256(b"code"),
                fingerprint: fingerprint.to_owned(),
                product: product.name(),
                title_id: None,
                game_name: None,
                failure_kind: "error".to_owned(),
                template_id: "t".to_owned(),
                message: "t".to_owned(),
                created_day: day.to_owned(),
                received_day: day.to_owned(),
                report_json: "{}".to_owned(),
            };
            self.state.stores.get(product).insert(&new, 1000).await?;
            Ok(id)
        }
    }

    impl Harness {
        async fn get(&self, uri: &str) -> Result<(StatusCode, String), Box<dyn Error>> {
            let (status, _, body) = self.send(Method::GET, uri, &[], vec![]).await?;
            Ok((status, String::from_utf8(body)?))
        }
    }

    fn psport(title: &str, name: &str) -> String {
        format!(
            r#"{{"schema":2,"product":"psport","date":"2026-10-21","decanter":{{"version":"0.3.2","commit":"d61f1ff91d29"}},"psport":{{"version":"0.1.0","commit":"aaaaaaaaaaaa"}},"system":{{"os":"linux"}},"game":{{"title_id":"{title}","name":"{name}"}},"failure":{{"kind":"error","count":1}}}}"#
        )
    }

    impl Harness {
        async fn admin(&self, method: Method, uri: &str) -> Result<(StatusCode, Vec<u8>), Box<dyn Error>> {
            let auth = bearer();
            let (status, _, body) = self
                .send(method, uri, &[("authorization", auth.as_str())], vec![])
                .await?;
            Ok((status, body))
        }

        async fn admin_rows(&self, query: &str) -> Result<Vec<Value>, Box<dyn Error>> {
            let (status, body) = self.admin(Method::GET, &format!("/admin/reports{query}")).await?;
            assert_eq!(status, StatusCode::OK, "{query}");
            Ok(serde_json::from_slice(&body)?)
        }
    }

    fn other_commit(n: u8) -> String {
        DECANTER.replace("d61f1ff91d29", &format!("{n:012x}"))
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn bearer() -> String {
        format!("Bearer {ADMIN_KEY}")
    }

    #[tokio::test]
    async fn valid_report_is_published_and_listed() -> Test {
        let h = Harness::new(|_| {})?;
        let (status, answer) = h.post(DECANTER).await?;
        assert_eq!(status, StatusCode::CREATED);
        let id = answer["id"].as_str().ok_or("id")?;
        assert_eq!(Product::of_id(id), Some(Product::Decanter), "{id}");
        assert_eq!(answer["delete_code"].as_str().map(str::len), Some(32));
        let (status, headers, page) = h.send(Method::GET, &format!("/r/{id}"), &[], vec![]).await?;
        assert_eq!(status, StatusCode::OK);
        let page = String::from_utf8(page)?;
        assert!(page.contains("decanter.job.failed"));
        assert_eq!(
            headers.get("x-content-type-options").and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert!(
            headers
                .get("content-security-policy")
                .is_some_and(|v| v.as_bytes().starts_with(b"default-src 'none'"))
        );
        let (_, _, list) = h
            .send(Method::GET, "/?product=decanter&resolved=0", &[], vec![])
            .await?;
        assert!(String::from_utf8(list)?.contains(answer["fingerprint"].as_str().ok_or("fp")?));
        Ok(())
    }

    #[tokio::test]
    async fn rejected_report_is_422_stores_nothing_and_echoes_no_value() -> Test {
        let h = Harness::new(|_| {})?;
        let (status, _, body) = h
            .send(Method::POST, "/v1/reports", JSON, PRIVATE_PATH.as_bytes().to_vec())
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let text = String::from_utf8(body)?;
        assert!(text.contains("system.os_version"));
        assert!(!text.contains("/home/deck"));
        assert_eq!(h.stored().await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn oversized_body_is_413_by_header_and_by_length() -> Test {
        let h = Harness::new(|_| {})?;
        let (declared, ..) = h
            .send(
                Method::POST,
                "/v1/reports",
                &[("content-length", "65537"), JSON[0]],
                b"{}".to_vec(),
            )
            .await?;
        assert_eq!(declared, StatusCode::PAYLOAD_TOO_LARGE);
        let (actual, ..) = h.send(Method::POST, "/v1/reports", JSON, vec![b' '; 70_000]).await?;
        assert_eq!(actual, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(h.stored().await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn delete_answers_the_same_for_unknown_id_and_wrong_code() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?.to_owned();
        let code = answer["delete_code"].as_str().ok_or("code")?.to_owned();
        let known = format!("/v1/reports/{id}");
        let (s2, _, b2) = h
            .send(Method::DELETE, &known, &[("x-delete-code", "wrong")], vec![])
            .await?;
        let (s3, _, b3) = h.send(Method::DELETE, &known, &[], vec![]).await?;
        assert_eq!((s2, s3), (StatusCode::NOT_FOUND, StatusCode::NOT_FOUND));
        assert_eq!(b2, b3);
        let other_partition = id.replacen('d', "p", 1);
        for unknown in [
            "0".repeat(32),
            format!("d{}", "0".repeat(32)),
            other_partition,
            id.get(1..).unwrap_or_default().to_owned(),
        ] {
            let (status, _, body) = h
                .send(
                    Method::DELETE,
                    &format!("/v1/reports/{unknown}"),
                    &[("x-delete-code", &code)],
                    vec![],
                )
                .await?;
            assert_eq!((status, &body), (StatusCode::NOT_FOUND, &b2), "{unknown}");
        }
        let (ok, ..) = h
            .send(Method::DELETE, &known, &[("x-delete-code", &code)], vec![])
            .await?;
        assert_eq!(ok, StatusCode::NO_CONTENT);
        let (gone, ..) = h.send(Method::GET, &format!("/r/{id}"), &[], vec![]).await?;
        assert_eq!(gone, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn rate_limit_answers_429_with_retry_after() -> Test {
        let h = Harness::new(|config| config.limits.per_ip_hour = 2)?;
        for _ in 0..2 {
            assert_eq!(h.post(DECANTER).await?.0, StatusCode::CREATED);
        }
        let (status, headers, _) = h
            .send(Method::POST, "/v1/reports", JSON, DECANTER.as_bytes().to_vec())
            .await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(headers.get("retry-after").is_some());
        assert_eq!(h.stored().await?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn admin_routes_are_404_without_the_key() -> Test {
        let h = Harness::new(|config| config.limits.failures_per_ip = 1000)?;
        let id = "0".repeat(32);
        for (method, path) in [
            (Method::GET, "/admin/reports".to_owned()),
            (Method::POST, format!("/admin/reports/{id}/hide")),
            (Method::POST, format!("/admin/reports/{id}/resolve")),
            (Method::DELETE, format!("/admin/reports/{id}")),
            (Method::GET, format!("/admin/reports/{id}/attachment")),
            (Method::POST, format!("/admin/groups/{}/hide", "0".repeat(16))),
            (Method::POST, format!("/admin/groups/{}/restore", "0".repeat(16))),
            (Method::POST, format!("/admin/groups/{}/resolve", "0".repeat(16))),
            (Method::DELETE, format!("/admin/groups/{}", "0".repeat(16))),
        ] {
            let (none, ..) = h.send(method.clone(), &path, &[], vec![]).await?;
            let (wrong, ..) = h
                .send(method, &path, &[("authorization", "Bearer nope")], vec![])
                .await?;
            assert_eq!((none, wrong), (StatusCode::NOT_FOUND, StatusCode::NOT_FOUND), "{path}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn maintainer_hides_restores_and_resolves() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?.to_owned();
        let auth = bearer();
        let key = [("authorization", auth.as_str())];
        let (hide, ..) = h
            .send(Method::POST, &format!("/admin/reports/{id}/hide"), &key, vec![])
            .await?;
        assert_eq!(hide, StatusCode::NO_CONTENT);
        let (page, ..) = h.send(Method::GET, &format!("/r/{id}"), &[], vec![]).await?;
        assert_eq!(page, StatusCode::NOT_FOUND);
        let (_, _, list) = h.send(Method::GET, "/admin/reports", &key, vec![]).await?;
        let rows: Value = serde_json::from_slice(&list)?;
        assert_eq!(rows[0]["hidden"], json!(true));
        h.send(Method::POST, &format!("/admin/reports/{id}/restore"), &key, vec![])
            .await?;
        let (resolve, ..) = h
            .send(Method::POST, &format!("/admin/reports/{id}/resolve"), &key, vec![])
            .await?;
        assert_eq!(resolve, StatusCode::NO_CONTENT);
        let (status, _, page) = h.send(Method::GET, &format!("/r/{id}"), &[], vec![]).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8(page)?.contains("Resuelto el"));
        Ok(())
    }

    #[tokio::test]
    async fn attachment_is_private_and_goes_away_on_resolve() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?.to_owned();
        let code = answer["delete_code"].as_str().ok_or("code")?.to_owned();
        let path = format!("/v1/reports/{id}/attachment");
        let (denied, ..) = h
            .send(Method::PUT, &path, &[("x-delete-code", "wrong")], b"log".to_vec())
            .await?;
        assert_eq!(denied, StatusCode::NOT_FOUND);
        let (stored, ..) = h
            .send(Method::PUT, &path, &[("x-delete-code", &code)], b"raw log".to_vec())
            .await?;
        assert_eq!(stored, StatusCode::NO_CONTENT);
        let (second, ..) = h
            .send(Method::PUT, &path, &[("x-delete-code", &code)], b"again".to_vec())
            .await?;
        assert_eq!(second, StatusCode::CONFLICT);
        let (too_big, ..) = h
            .send(
                Method::PUT,
                &path,
                &[("x-delete-code", &code), ("content-length", "5242881")],
                vec![],
            )
            .await?;
        assert_eq!(too_big, StatusCode::PAYLOAD_TOO_LARGE);
        let auth = bearer();
        let key = [("authorization", auth.as_str())];
        let admin_path = format!("/admin/reports/{id}/attachment");
        let (status, headers, body) = h.send(Method::GET, &admin_path, &key, vec![]).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"raw log");
        assert_eq!(
            headers.get("content-type").and_then(|v| v.to_str().ok()),
            Some("application/octet-stream")
        );
        assert!(
            headers
                .get("content-disposition")
                .is_some_and(|v| v.as_bytes().starts_with(b"attachment"))
        );
        assert_eq!(h.blobs().await?, 1);
        h.send(Method::POST, &format!("/admin/reports/{id}/resolve"), &key, vec![])
            .await?;
        assert_eq!(h.blobs().await?, 0);
        let (gone, ..) = h.send(Method::GET, &admin_path, &key, vec![]).await?;
        assert_eq!(gone, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn pages_escape_even_a_report_that_skipped_validation() -> Test {
        let h = Harness::new(|_| {})?;
        let hostile = json!({"product": "decanter", "system": {"os_version": "<img src=x onerror=alert(1)>\"'"},
            "game": {"name": "<script>alert(1)</script>"}, "failure": {"message": {"template": "<b>t</b>"}}});
        let id = format!("p{}", "a".repeat(32));
        let new = NewReport {
            id: id.clone(),
            delete_hash: sha256(b"x"),
            fingerprint: "b".repeat(16),
            product: "psport",
            title_id: Some("CUSA00000".to_owned()),
            game_name: Some("<script>alert(1)</script>".to_owned()),
            failure_kind: "error".to_owned(),
            template_id: "t".to_owned(),
            message: crate::html::message_text(&hostile),
            created_day: "2026-10-21".to_owned(),
            received_day: "2026-10-25".to_owned(),
            report_json: hostile.to_string(),
        };
        h.state.stores.get(Product::Psport).insert(&new, 10).await?;
        for path in [format!("/r/{id}"), "/".to_owned(), format!("/g/{}", "b".repeat(16))] {
            let (status, _, body) = h.send(Method::GET, &path, &[], vec![]).await?;
            let page = String::from_utf8(body)?;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(
                !page.contains("<img") && !page.contains("<script>alert") && !page.contains("<b>t"),
                "{path}"
            );
        }
        let (bad, ..) = h.send(Method::GET, "/?product=%3Cscript%3E", &[], vec![]).await?;
        assert_eq!(bad, StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    async fn index_is_served_from_cache_until_it_is_cleared() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, first) = h.post(DECANTER).await?;
        let first_fp = first["fingerprint"].as_str().ok_or("fp")?.to_owned();
        assert!(h.get("/").await?.1.contains(&first_fp));
        let (_, second) = h.post(&other_commit(7)).await?;
        let second_fp = second["fingerprint"].as_str().ok_or("fp")?.to_owned();
        assert!(
            !h.get("/").await?.1.contains(&second_fp),
            "a hit must not see the new group"
        );
        let id = first["id"].as_str().ok_or("id")?;
        let auth = bearer();
        h.send(
            Method::POST,
            &format!("/admin/reports/{id}/hide"),
            &[("authorization", auth.as_str())],
            vec![],
        )
        .await?;
        let page = h.get("/").await?.1;
        assert!(
            page.contains(&second_fp) && !page.contains(&first_fp),
            "hiding drops the cache"
        );
        Ok(())
    }

    #[tokio::test]
    async fn index_cache_off_always_reads_the_table() -> Test {
        let h = Harness::new(|config| config.index_cache_secs = 0)?;
        h.get("/").await?;
        let (_, answer) = h.post(DECANTER).await?;
        assert!(
            h.get("/")
                .await?
                .1
                .contains(answer["fingerprint"].as_str().ok_or("fp")?)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cached_key_set_stays_bounded_under_varied_filters() -> Test {
        let h = Harness::new(|config| config.limits.get_per_ip_minute = 100_000)?;
        for i in 0..400 {
            let (status, _) = h.get(&format!("/?title=CUSA{i:05}&page={}", 1 + i % 7)).await?;
            assert_eq!(status, StatusCode::OK);
        }
        assert!(h.state.index_cache.len() <= 128);
        assert_eq!(h.get("/?title=bad").await?.0, StatusCode::BAD_REQUEST);
        assert!(h.state.index_cache.len() <= 128);
        Ok(())
    }

    #[tokio::test]
    async fn upload_does_not_queue_behind_parallel_reads() -> Test {
        let h = Harness::new(|config| {
            config.index_cache_secs = 0;
            config.limits.per_ip_hour = 100;
            config.limits.per_ip_day = 100;
        })?;
        for n in 0..20 {
            h.post(&other_commit(n)).await?;
        }
        let reads: Vec<_> = (0..16)
            .map(|_| {
                let stores = h.state.stores.clone();
                tokio::spawn(async move {
                    for _ in 0..10 {
                        stores.groups(&crate::store::Filter::default(), 26, 0).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .collect();
        let started = std::time::Instant::now();
        assert_eq!(h.post(&other_commit(99)).await?.0, StatusCode::CREATED);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1000),
            "{:?}",
            started.elapsed()
        );
        for read in reads {
            read.await??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn page_reads_are_limited_per_client_but_not_health_or_admin() -> Test {
        let h = Harness::new(|config| config.limits.get_per_ip_minute = 3)?;
        for _ in 0..3 {
            assert_eq!(h.get("/").await?.0, StatusCode::OK);
        }
        let (status, headers, _) = h.send(Method::GET, "/", &[], vec![]).await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(headers.get("retry-after").is_some());
        assert_eq!(h.get("/healthz").await?.0, StatusCode::OK);
        assert_eq!(h.get("/admin/reports").await?.0, StatusCode::NOT_FOUND);
        assert_eq!(h.post(DECANTER).await?.0, StatusCode::CREATED);
        Ok(())
    }

    #[tokio::test]
    async fn rejected_uploads_do_not_spend_the_global_cap() -> Test {
        let h = Harness::new(|config| config.limits.global_hour = 2)?;
        for _ in 0..5 {
            assert_eq!(h.post(PRIVATE_PATH).await?.0, StatusCode::UNPROCESSABLE_ENTITY);
        }
        assert_eq!(h.post(DECANTER).await?.0, StatusCode::CREATED);
        assert_eq!(h.post(&other_commit(1)).await?.0, StatusCode::CREATED);
        assert_eq!(h.post(&other_commit(2)).await?.0, StatusCode::TOO_MANY_REQUESTS);
        Ok(())
    }

    #[tokio::test]
    async fn rejected_uploads_still_count_for_the_address() -> Test {
        let h = Harness::new(|config| config.limits.per_ip_hour = 3)?;
        for _ in 0..3 {
            assert_eq!(h.post(PRIVATE_PATH).await?.0, StatusCode::UNPROCESSABLE_ENTITY);
        }
        assert_eq!(h.post(DECANTER).await?.0, StatusCode::TOO_MANY_REQUESTS);
        Ok(())
    }

    #[tokio::test]
    async fn attachment_spends_the_global_cap_only_with_the_right_code() -> Test {
        let h = Harness::new(|config| config.limits.global_hour = 2)?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?.to_owned();
        let code = answer["delete_code"].as_str().ok_or("code")?.to_owned();
        let path = format!("/v1/reports/{id}/attachment");
        for _ in 0..5 {
            let (status, ..) = h
                .send(Method::PUT, &path, &[("x-delete-code", "wrong")], b"log".to_vec())
                .await?;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        let (stored, ..) = h
            .send(Method::PUT, &path, &[("x-delete-code", &code)], b"log".to_vec())
            .await?;
        assert_eq!(stored, StatusCode::NO_CONTENT);
        assert_eq!(h.post(&other_commit(1)).await?.0, StatusCode::TOO_MANY_REQUESTS);
        Ok(())
    }

    #[tokio::test]
    async fn group_shows_the_most_frequent_game_not_the_latest() -> Test {
        let h = Harness::new(|config| config.index_cache_secs = 0)?;
        for (title, name) in [
            ("CUSA00001", "Game One"),
            ("CUSA00001", "Game One"),
            ("CUSA00002", "Game Two"),
            ("CUSA00003", "Game Three"),
            ("CUSA00004", "Game Four"),
        ] {
            assert_eq!(h.post(&psport(title, name)).await?.0, StatusCode::CREATED);
        }
        let page = h.get("/").await?.1;
        assert!(page.contains("CUSA00001</code> Game One (+3 más)"), "{page}");
        assert!(!page.contains("Game Four"));
        Ok(())
    }

    #[tokio::test]
    async fn maintainer_acts_on_a_whole_group() -> Test {
        let h = Harness::new(|_| {})?;
        let mut ids = Vec::new();
        for title in ["CUSA00001", "CUSA00002"] {
            let (_, answer) = h.post(&psport(title, "G")).await?;
            ids.push((
                answer["id"].as_str().ok_or("id")?.to_owned(),
                answer["delete_code"].as_str().ok_or("code")?.to_owned(),
                answer["fingerprint"].as_str().ok_or("fp")?.to_owned(),
            ));
        }
        let (_, _, fp) = ids[0].clone();
        let (_, other) = h.post(DECANTER).await?;
        let other_id = other["id"].as_str().ok_or("id")?.to_owned();
        let (id, code, _) = ids[0].clone();
        h.send(
            Method::PUT,
            &format!("/v1/reports/{id}/attachment"),
            &[("x-delete-code", &code)],
            b"log".to_vec(),
        )
        .await?;
        assert_eq!(
            h.admin(Method::POST, &format!("/admin/groups/{fp}/hide")).await?.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(h.get(&format!("/r/{}", ids[1].0)).await?.0, StatusCode::NOT_FOUND);
        assert_eq!(h.get(&format!("/r/{other_id}")).await?.0, StatusCode::OK);
        assert_eq!(
            h.admin(Method::POST, &format!("/admin/groups/{fp}/restore")).await?.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(h.get(&format!("/r/{}", ids[1].0)).await?.0, StatusCode::OK);
        assert_eq!(h.blobs().await?, 1);
        assert_eq!(
            h.admin(Method::POST, &format!("/admin/groups/{fp}/resolve")).await?.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(h.blobs().await?, 0);
        let resolved = h.admin_rows(&format!("?fingerprint={fp}")).await?;
        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|row| !row["resolved_at"].is_null()));
        assert!(
            h.admin_rows(&format!("?fingerprint={}", "0".repeat(16)))
                .await?
                .is_empty()
        );
        assert_eq!(
            h.admin(Method::DELETE, &format!("/admin/groups/{fp}")).await?.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(h.stored().await?, 1);
        assert_eq!(
            h.admin(Method::DELETE, &format!("/admin/groups/{fp}")).await?.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            h.admin(Method::POST, "/admin/groups/not-a-fingerprint/hide").await?.0,
            StatusCode::NOT_FOUND
        );
        Ok(())
    }

    #[tokio::test]
    async fn group_delete_unlinks_attachments() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?;
        let code = answer["delete_code"].as_str().ok_or("code")?;
        let fp = answer["fingerprint"].as_str().ok_or("fp")?;
        h.send(
            Method::PUT,
            &format!("/v1/reports/{id}/attachment"),
            &[("x-delete-code", code)],
            b"log".to_vec(),
        )
        .await?;
        assert_eq!(h.blobs().await?, 1);
        h.admin(Method::DELETE, &format!("/admin/groups/{fp}")).await?;
        assert_eq!(h.blobs().await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn admin_list_filters_by_group_day_product_and_title() -> Test {
        let h = Harness::new(|_| {})?;
        h.post(DECANTER).await?;
        h.post(&psport("CUSA00001", "G")).await?;
        h.post(&psport("CUSA00002", "G")).await?;
        let today = super::today();
        assert_eq!(h.admin_rows("").await?.len(), 3);
        assert_eq!(h.admin_rows("?product=psport").await?.len(), 2);
        assert_eq!(h.admin_rows("?title=CUSA00002").await?.len(), 1);
        assert_eq!(h.admin_rows(&format!("?day={today}&product=decanter")).await?.len(), 1);
        assert_eq!(h.admin_rows("?day=2001-01-01").await?.len(), 0);
        assert_eq!(h.admin_rows("?title=&product=").await?.len(), 3);
        for bad in [
            "fingerprint=xyz",
            "day=2026-13-40",
            "day=today",
            "product=other",
            "title=cusa1",
            "title=%27",
        ] {
            let (status, _) = h.admin(Method::GET, &format!("/admin/reports?{bad}")).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn full_table_evicts_the_oldest_unresolved_and_unlinks_their_files() -> Test {
        let h = Harness::new(|config| config.limits.max_reports = 3)?;
        let mut first = None;
        for n in 0..3 {
            let (status, answer) = h.post(&other_commit(n)).await?;
            assert_eq!(status, StatusCode::CREATED);
            if n == 0 {
                first = Some(answer);
            }
        }
        let first = first.ok_or("first")?;
        let id = first["id"].as_str().ok_or("id")?;
        let code = first["delete_code"].as_str().ok_or("code")?;
        h.send(
            Method::PUT,
            &format!("/v1/reports/{id}/attachment"),
            &[("x-delete-code", code)],
            b"log".to_vec(),
        )
        .await?;
        assert_eq!(h.post(&other_commit(9)).await?.0, StatusCode::CREATED);
        assert_eq!(h.stored().await?, 3);
        assert_eq!(h.get(&format!("/r/{id}")).await?.0, StatusCode::NOT_FOUND);
        assert_eq!(h.blobs().await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn full_table_of_resolved_reports_still_answers_503() -> Test {
        let h = Harness::new(|config| config.limits.max_reports = 2)?;
        for n in 0..2 {
            let (_, answer) = h.post(&other_commit(n)).await?;
            let id = answer["id"].as_str().ok_or("id")?;
            h.admin(Method::POST, &format!("/admin/reports/{id}/resolve")).await?;
        }
        assert_eq!(h.post(&other_commit(9)).await?.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(h.stored().await?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn full_attachment_store_evicts_the_oldest_attachment() -> Test {
        let h = Harness::new(|config| config.limits.max_attachments = 1)?;
        let mut owners = Vec::new();
        for n in 0..2 {
            let (_, answer) = h.post(&other_commit(n)).await?;
            owners.push((
                answer["id"].as_str().ok_or("id")?.to_owned(),
                answer["delete_code"].as_str().ok_or("code")?.to_owned(),
            ));
        }
        for (id, code) in &owners {
            let (status, ..) = h
                .send(
                    Method::PUT,
                    &format!("/v1/reports/{id}/attachment"),
                    &[("x-delete-code", code)],
                    b"log".to_vec(),
                )
                .await?;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        assert_eq!(h.blobs().await?, 1);
        let (old, _) = h
            .admin(Method::GET, &format!("/admin/reports/{}/attachment", owners[0].0))
            .await?;
        let (new, _) = h
            .admin(Method::GET, &format!("/admin/reports/{}/attachment", owners[1].0))
            .await?;
        assert_eq!((old, new), (StatusCode::NOT_FOUND, StatusCode::OK));
        assert_eq!(h.get(&format!("/r/{}", owners[0].0)).await?.0, StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn sweep_expires_resolved_reports_only_when_configured() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, resolved) = h.post(DECANTER).await?;
        h.post(&other_commit(1)).await?;
        h.admin(
            Method::POST,
            &format!("/admin/reports/{}/resolve", resolved["id"].as_str().ok_or("id")?),
        )
        .await?;
        let store = h.state.stores.get(Product::Decanter);
        let kept = store.sweep("2026-10-28", "0000-01-01", None).await?;
        assert_eq!((kept.unresolved, kept.resolved), (0, 0));
        let swept = store.sweep("2026-10-28", "0000-01-01", Some("9999-12-31")).await?;
        assert_eq!((swept.unresolved, swept.resolved), (0, 1));
        assert_eq!(h.stored().await?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn reports_of_each_product_land_in_their_own_partition() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, decanter) = h.post(DECANTER).await?;
        let (_, psport) = h.post(&psport("CUSA00001", "G")).await?;
        let decanter = decanter["id"].as_str().ok_or("id")?;
        let psport = psport["id"].as_str().ok_or("id")?;
        assert!(decanter.starts_with('d') && psport.starts_with('p'));
        for product in Product::ALL {
            let file = h.root.join(product.name()).join("s3db.redb");
            assert!(file.is_file(), "{}", file.display());
        }
        let (in_decanter, in_psport) = (
            h.state.stores.get(Product::Decanter),
            h.state.stores.get(Product::Psport),
        );
        assert!(in_decanter.find_public(decanter).await?.is_some());
        assert!(in_psport.find_public(decanter).await?.is_none());
        assert!(in_psport.find_public(psport).await?.is_some());
        assert!(in_decanter.find_public(psport).await?.is_none());
        assert_eq!(
            (h.count_in(Product::Decanter).await?, h.count_in(Product::Psport).await?),
            (1, 1)
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_id_never_resolves_in_the_other_partition() -> Test {
        let h = Harness::new(|_| {})?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?.to_owned();
        let code = answer["delete_code"].as_str().ok_or("code")?.to_owned();
        let crossed = id.replacen('d', "p", 1);
        let (missing, unknown_page) = h.get(&format!("/r/{}", "0".repeat(33))).await?;
        let (status, crossed_page) = h.get(&format!("/r/{crossed}")).await?;
        assert_eq!((missing, status), (StatusCode::NOT_FOUND, StatusCode::NOT_FOUND));
        assert_eq!(unknown_page, crossed_page);
        let (attach, ..) = h
            .send(
                Method::PUT,
                &format!("/v1/reports/{crossed}/attachment"),
                &[("x-delete-code", &code)],
                b"log".to_vec(),
            )
            .await?;
        assert_eq!(attach, StatusCode::NOT_FOUND);
        for (method, path) in [
            (Method::POST, format!("/admin/reports/{crossed}/hide")),
            (Method::DELETE, format!("/admin/reports/{crossed}")),
            (Method::GET, format!("/admin/reports/{crossed}/attachment")),
        ] {
            assert_eq!(h.admin(method, &path).await?.0, StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(h.get(&format!("/r/{id}")).await?.0, StatusCode::OK);
        assert_eq!(h.blobs().await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn index_merges_both_partitions_in_index_order_and_filters_by_product() -> Test {
        let h = Harness::new(|config| {
            config.index_cache_secs = 0;
            config.page_size = 1;
        })?;
        let oldest = "1".repeat(16);
        let middle = "2".repeat(16);
        let newest = "3".repeat(16);
        h.insert_direct(Product::Decanter, &oldest, "2026-10-20").await?;
        h.insert_direct(Product::Psport, &middle, "2026-10-22").await?;
        h.insert_direct(Product::Decanter, &newest, "2026-10-24").await?;
        for (page, fp) in [(1, &newest), (2, &middle), (3, &oldest)] {
            let body = h.get(&format!("/?page={page}")).await?.1;
            assert!(body.contains(fp.as_str()), "page {page}: {body}");
        }
        assert!(!h.get("/?page=4").await?.1.contains("/g/"));
        let psport = h.get("/?product=psport").await?.1;
        assert!(psport.contains(&middle) && !psport.contains(&newest));
        let decanter = h.get("/?product=decanter&page=2").await?.1;
        assert!(decanter.contains(&oldest) && !decanter.contains(&middle));
        Ok(())
    }

    #[tokio::test]
    async fn group_actions_and_lists_span_both_partitions() -> Test {
        let h = Harness::new(|config| config.index_cache_secs = 0)?;
        let fp = "c".repeat(16);
        let decanter = h.insert_direct(Product::Decanter, &fp, "2026-10-25").await?;
        let psport = h.insert_direct(Product::Psport, &fp, "2026-10-26").await?;
        h.insert_direct(Product::Psport, &"d".repeat(16), "2026-10-27").await?;
        let group = h.get(&format!("/g/{fp}")).await?.1;
        assert!(
            group.find(&psport) < group.find(&decanter) && group.contains(&decanter),
            "{group}"
        );
        assert_eq!(h.get("/").await?.1.matches(&format!("/g/{fp}")).count(), 2);
        assert_eq!(h.admin_rows(&format!("?fingerprint={fp}")).await?.len(), 2);
        let all = h.admin_rows("").await?;
        assert_eq!(all.len(), 3);
        assert_eq!(
            all[2]["id"],
            json!(decanter),
            "newest received day first across partitions"
        );
        assert_eq!(h.admin_rows("?product=decanter").await?.len(), 1);
        assert_eq!(h.admin_rows("?limit=1&offset=1").await?[0]["id"], json!(psport));
        assert_eq!(
            h.admin(Method::POST, &format!("/admin/groups/{fp}/hide")).await?.0,
            StatusCode::NO_CONTENT
        );
        for id in [&decanter, &psport] {
            assert_eq!(h.get(&format!("/r/{id}")).await?.0, StatusCode::NOT_FOUND);
        }
        assert_eq!(h.get(&format!("/g/{fp}")).await?.0, StatusCode::NOT_FOUND);
        assert_eq!(
            h.admin(Method::DELETE, &format!("/admin/groups/{fp}")).await?.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            (h.count_in(Product::Decanter).await?, h.count_in(Product::Psport).await?),
            (0, 1)
        );
        Ok(())
    }

    #[tokio::test]
    async fn caps_evict_within_the_partition_that_is_full() -> Test {
        let h = Harness::new(|config| config.limits.max_reports = 2)?;
        let mut first = None;
        for n in 0..2 {
            let (_, answer) = h.post(&other_commit(n)).await?;
            first.get_or_insert(answer);
            h.post(&psport(&format!("CUSA0000{n}"), "G")).await?;
        }
        assert_eq!(h.stored().await?, 4, "each partition holds up to max_reports");
        assert_eq!(h.post(&other_commit(9)).await?.0, StatusCode::CREATED);
        let first = first.ok_or("first")?;
        assert_eq!(
            h.get(&format!("/r/{}", first["id"].as_str().ok_or("id")?)).await?.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            (h.count_in(Product::Decanter).await?, h.count_in(Product::Psport).await?),
            (2, 2)
        );
        Ok(())
    }

    #[tokio::test]
    async fn daily_sweep_runs_on_every_partition() -> Test {
        let h = Harness::new(|_| {})?;
        for product in Product::ALL {
            h.insert_direct(product, &"e".repeat(16), "2001-01-01").await?;
        }
        let (_, fresh) = h.post(&psport("CUSA00001", "G")).await?;
        crate::sweep::run_once(&h.state).await;
        assert_eq!(
            (h.count_in(Product::Decanter).await?, h.count_in(Product::Psport).await?),
            (0, 1)
        );
        assert_eq!(
            h.get(&format!("/r/{}", fresh["id"].as_str().ok_or("id")?)).await?.0,
            StatusCode::OK
        );
        Ok(())
    }

    #[tokio::test]
    async fn allow_header_is_never_sent() -> Test {
        let h = Harness::new(|_| {})?;
        for (method, uri) in [
            (Method::PATCH, "/admin/reports"),
            (Method::POST, "/healthz"),
            (Method::PUT, "/"),
            (Method::GET, "/v1/reports"),
        ] {
            let (status, headers, _) = h.send(method.clone(), uri, &[], vec![]).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            assert!(headers.get("allow").is_none(), "{method} {uri}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn extractor_errors_are_json_with_a_fixed_message() -> Test {
        let h = Harness::new(|_| {})?;
        let auth = bearer();
        for (uri, headers) in [
            ("/?page=1&page=2", vec![]),
            ("/?product=decanter&product=psport", vec![]),
            ("/g/0123456789abcdef?page=1&page=2", vec![]),
            ("/admin/reports?limit=x", vec![("authorization", auth.as_str())]),
            ("/admin/reports?limit=1&limit=2", vec![("authorization", auth.as_str())]),
        ] {
            let (status, answer, body) = h.send(Method::GET, uri, &headers, vec![]).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            let content_type = answer.get("content-type").and_then(|v| v.to_str().ok());
            assert_eq!(content_type, Some("application/json"), "{uri}");
            assert_eq!(
                serde_json::from_slice::<Value>(&body)?,
                json!({"error": "bad_request"}),
                "{uri}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn upload_requires_a_json_content_type() -> Test {
        let h = Harness::new(|_| {})?;
        let body = DECANTER.as_bytes().to_vec();
        for headers in [
            &[][..],
            &[("content-type", "text/plain")],
            &[("content-type", "application/jsonx")],
        ] {
            let (status, ..) = h.send(Method::POST, "/v1/reports", headers, body.clone()).await?;
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{headers:?}");
        }
        assert_eq!(h.stored().await?, 0);
        for value in ["application/json", "Application/JSON; charset=utf-8"] {
            let (status, ..) = h
                .send(Method::POST, "/v1/reports", &[("content-type", value)], body.clone())
                .await?;
            assert_eq!(status, StatusCode::CREATED, "{value}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn wrong_admin_keys_end_in_429_for_that_address_only() -> Test {
        let h = Harness::new(|config| config.limits.failures_per_ip = 3)?;
        let wrong = [("authorization", "Bearer nope")];
        for _ in 0..3 {
            let (status, ..) = h.send(Method::GET, "/admin/reports", &wrong, vec![]).await?;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        let good = bearer();
        let right = [("authorization", good.as_str())];
        for (uri, headers) in [("/admin/reports", &right[..]), ("/admin/nothing", &[][..])] {
            let (status, answer, _) = h.send(Method::GET, uri, headers, vec![]).await?;
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{uri}");
            assert!(answer.get("retry-after").is_some());
        }
        let (public, ..) = h.send(Method::GET, "/healthz", &[], vec![]).await?;
        assert_eq!(public, StatusCode::OK);
        let (other, ..) = h
            .send_from([198, 51, 100, 9], Method::GET, "/admin/reports", &right, vec![])
            .await?;
        assert_eq!(other, StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn admin_successes_do_not_count_as_failures() -> Test {
        let h = Harness::new(|config| config.limits.failures_per_ip = 2)?;
        for _ in 0..6 {
            assert_eq!(h.admin(Method::GET, "/admin/reports").await?.0, StatusCode::OK);
        }
        Ok(())
    }

    #[tokio::test]
    async fn wrong_delete_codes_end_in_429_and_a_right_one_does_not_count() -> Test {
        let h = Harness::new(|config| config.limits.failures_per_ip = 3)?;
        let (_, answer) = h.post(DECANTER).await?;
        let id = answer["id"].as_str().ok_or("id")?.to_owned();
        let code = answer["delete_code"].as_str().ok_or("code")?.to_owned();
        let target = format!("/v1/reports/{id}");
        for attempt in ["a", "b", "c"] {
            let (status, ..) = h
                .send(Method::DELETE, &target, &[("x-delete-code", attempt)], vec![])
                .await?;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        let (status, headers, _) = h
            .send(Method::DELETE, &target, &[("x-delete-code", &code)], vec![])
            .await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(headers.get("retry-after").is_some());
        let (status, ..) = h
            .send_from(
                [198, 51, 100, 9],
                Method::DELETE,
                &target,
                &[("x-delete-code", &code)],
                vec![],
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        Ok(())
    }
}
