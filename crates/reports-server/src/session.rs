//! Browser sessions of the maintainers: login with an account's key, a `__Host-` cookie, a CSRF token per session,
//! and the gate that lets an `/admin` request through with either a valid Bearer key or a valid session, carrying
//! who made it and with which role.
//!
//! Sessions live in memory only, so a restart logs every maintainer out. Neither a key, a session id nor a CSRF
//! token is ever logged; ids are kept only as their SHA-256.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, COOKIE, LOCATION, ORIGIN, REFERER, REFERRER_POLICY, SET_COOKIE,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use tracing::Instrument as _;

use crate::accounts::Identity;
use crate::app::{AppState, api_error, internal, not_found};
use crate::audit::{self, Action};
use crate::config::{AdminSessions, Config};
use crate::html::layout;
use crate::limits::client_ip;
use crate::secrets::{Hash, is_hex_token, matches, random_hex_256, sha256};
use crate::store::Role;

pub const LOGIN_PATH: &str = "/admin/login";
const COOKIE_NAME: &str = "__Host-session";
pub const CSRF_FIELD: &str = "csrf";
const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");
/// A session id and a CSRF token are 256 random bits each, written as 64 hex characters.
const TOKEN_HEX: usize = 64;
/// Admin forms carry a token and a return path, nothing more; a larger body is not one of them.
const MAX_FORM_BYTES: usize = 4096;

/// Proof that the gate let the request through, and whose request it is. `user` is the account's name; `csrf` is
/// the session's token when a session authenticated it, for the forms a page renders; a Bearer request has none.
#[derive(Clone)]
pub struct Admin {
    pub user: String,
    pub role: Role,
    pub csrf: Option<String>,
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = Response;

    fn from_request_parts(parts: &mut Parts, _: &AppState) -> impl Future<Output = Result<Self, Response>> + Send {
        std::future::ready(parts.extensions.get::<Self>().cloned().ok_or_else(not_found))
    }
}

/// Proof that the account may change reports: the gate inserts it for the `admin` role only, and every handler that
/// hides, restores, resolves or deletes takes it. It is a request extension checked at run time, so a new route whose
/// handler does not take it compiles and lets a viewer through: every mutating route must take it by hand, and
/// `admin_tests` sends a viewer to every mutating `/admin` route of the router to catch one that does not. Built
/// nowhere else, hence the private field.
#[derive(Clone)]
pub struct CanChange(());

const READ_ONLY: &str = "Tu cuenta es de solo lectura: no puede ocultar, resolver ni borrar.";

/// A viewer's change answers 403 (it is a known account, so the uniform 404 would only confuse it): JSON for a
/// tool, a page for a form.
fn role_refused(parts: &Parts) -> Response {
    let is_form = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/x-www-form-urlencoded"));
    if !is_form {
        return forbidden("role");
    }
    let body = format!("<p>{READ_ONLY}</p><p><a href=\"/admin\">Volver</a></p>");
    (StatusCode::FORBIDDEN, Html(layout("Administración", &body))).into_response()
}

impl FromRequestParts<AppState> for CanChange {
    type Rejection = Response;

    fn from_request_parts(parts: &mut Parts, _: &AppState) -> impl Future<Output = Result<Self, Response>> + Send {
        let outcome = match (parts.extensions.get::<Self>(), parts.extensions.get::<Admin>()) {
            (Some(proof), _) => Ok(proof.clone()),
            (None, Some(_)) => Err(role_refused(parts)),
            (None, None) => Err(not_found()),
        };
        std::future::ready(outcome)
    }
}

struct Session {
    identity: Identity,
    csrf: String,
    csrf_hash: Hash,
    created: Instant,
    last_seen: Instant,
}

/// A live session as the gate needs it.
struct Live {
    identity: Identity,
    csrf: String,
    csrf_hash: Hash,
}

pub struct Sessions {
    idle: Duration,
    max_age: Duration,
    cap: usize,
    per_user: usize,
    by_id_hash: Mutex<HashMap<Hash, Session>>,
}

const NO_ADMINS_EVERY: Duration = Duration::from_hours(1);

/// Repeats in the log, hourly, why `/admin` refuses everyone, since the 404 itself must not say so.
pub async fn warn_without_admins(state: AppState) {
    loop {
        if !state.accounts.has_admin() {
            tracing::warn!(
                "no admin account: nobody can hide, resolve or delete reports, and without any account /admin \
                 refuses everyone; create one with `decanter-reports user add <name> --role admin`"
            );
        }
        tokio::time::sleep(NO_ADMINS_EVERY).await;
    }
}

/// The session to end so a new one fits: within its own user past the per-user cap, else the oldest session of
/// whoever holds the most, so one person's logins never end another's while others hold fewer.
fn eviction(sessions: &HashMap<Hash, Session>, user: &str, per_user: usize, cap: usize) -> Option<Hash> {
    let oldest_of = |id: &str| {
        sessions
            .iter()
            .filter(|(_, session)| session.identity.id == id)
            .min_by_key(|(_, session)| session.created)
            .map(|(key, _)| *key)
    };
    let own = sessions.values().filter(|session| session.identity.id == user).count();
    if own >= per_user {
        return oldest_of(user);
    }
    if sessions.len() < cap {
        return None;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for session in sessions.values() {
        *counts.entry(session.identity.id.as_str()).or_default() += 1;
    }
    let busiest = counts.values().copied().max().unwrap_or_default();
    sessions
        .values()
        .filter(|session| counts.get(session.identity.id.as_str()) == Some(&busiest))
        .min_by_key(|session| session.created)
        .and_then(|session| oldest_of(&session.identity.id))
}

impl Sessions {
    #[must_use]
    pub fn new(settings: &AdminSessions) -> Self {
        Self {
            idle: Duration::from_secs(settings.session_idle_secs.into()),
            max_age: Duration::from_secs(settings.session_max_secs.into()),
            cap: settings.max_sessions as usize,
            per_user: settings.max_sessions_per_user as usize,
            by_id_hash: Mutex::new(HashMap::new()),
        }
    }

    fn is_alive(&self, session: &Session, now: Instant) -> bool {
        now.saturating_duration_since(session.last_seen) < self.idle
            && now.saturating_duration_since(session.created) < self.max_age
    }

    /// Opens a session for `identity` and answers its id. Past `max_sessions_per_user` that user's oldest session
    /// ends; past `max_sessions` the oldest of whoever holds the most.
    ///
    /// # Errors
    ///
    /// The operating system's random source is unavailable.
    pub fn open(&self, identity: &Identity, now: Instant) -> Result<String, getrandom::Error> {
        let id = random_hex_256()?;
        let csrf = random_hex_256()?;
        let session = Session {
            identity: identity.clone(),
            csrf_hash: sha256(csrf.as_bytes()),
            csrf,
            created: now,
            last_seen: now,
        };
        let mut sessions = self.by_id_hash.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.retain(|_, session| self.is_alive(session, now));
        let mut ended = Vec::new();
        while let Some(key) = eviction(&sessions, &identity.id, self.per_user, self.cap) {
            if let Some(gone) = sessions.remove(&key) {
                ended.push(gone.identity);
            }
        }
        sessions.insert(sha256(id.as_bytes()), session);
        drop(sessions);
        for gone in ended {
            tracing::info!(
                user = %gone.name,
                role = %gone.role.name(),
                "admin session ended: max_sessions_per_user or max_sessions reached"
            );
        }
        Ok(id)
    }

    /// The live session with this id, its idle clock restarted; an expired one is removed on the way.
    fn touch(&self, id: &str, now: Instant) -> Option<Live> {
        let mut sessions = self.by_id_hash.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.retain(|_, session| self.is_alive(session, now));
        let live = sessions.get_mut(&sha256(id.as_bytes())).map(|session| {
            session.last_seen = now;
            Live {
                identity: session.identity.clone(),
                csrf: session.csrf.clone(),
                csrf_hash: session.csrf_hash,
            }
        });
        drop(sessions);
        live
    }

    /// Ends the sessions whose account `current` no longer knows as active with the generation they opened with,
    /// and gives the rest their account's present role, so a role change applies without a new login.
    pub fn revalidate(&self, current: impl Fn(&str) -> Option<(Role, u32)>) {
        let mut ended = Vec::new();
        let mut sessions = self.by_id_hash.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.retain(|_, session| match current(&session.identity.id) {
            Some((role, generation)) if generation == session.identity.generation => {
                session.identity.role = role;
                true
            }
            _ => {
                ended.push(session.identity.clone());
                false
            }
        });
        drop(sessions);
        for gone in ended {
            tracing::info!(
                user = %gone.name,
                role = %gone.role.name(),
                "admin session ended: account removed, disabled or given a new key"
            );
        }
    }

    fn close(&self, id: &str) -> bool {
        let removed = self
            .by_id_hash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&sha256(id.as_bytes()));
        removed.is_some()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_id_hash.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    #[cfg(test)]
    pub fn of_user(&self, name: &str) -> usize {
        self.by_id_hash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|session| session.identity.name == name)
            .count()
    }
}

#[must_use]
pub fn is_admin_path(path: &str) -> bool {
    path == "/admin" || path.starts_with("/admin/")
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value.trim())
        .filter(|value| is_hex_token(value, TOKEN_HEX))
}

fn bearer_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, key)| key.trim())
}

/// `scheme://authority` of an absolute http(s) URL, lowercased; `None` for anything else, `null` included.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

/// Whether the URL's host is this machine: `localhost`, `127.0.0.0/8` or `[::1]`.
#[must_use]
pub fn is_loopback_origin(url: &str) -> bool {
    let Some(origin) = origin_of(url) else {
        return false;
    };
    let authority = origin.split_once("://").map_or("", |(_, authority)| authority);
    let host = authority.strip_prefix('[').map_or_else(
        || authority.split(':').next().unwrap_or_default(),
        |bracketed| bracketed.split(']').next().unwrap_or_default(),
    );
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// A state-changing request made with the session cookie must come from a page of this server: its `Origin` (or,
/// without one, its `Referer`) must be `public_url`'s origin. Loopback origins are accepted only while `public_url`
/// is empty or itself loopback, which is a development setup.
fn is_same_origin(config: &Config, headers: &HeaderMap) -> bool {
    let header = |name| headers.get(name).and_then(|value: &HeaderValue| value.to_str().ok());
    let Some(claimed) = header(ORIGIN).or_else(|| header(REFERER)).and_then(origin_of) else {
        return false;
    };
    if origin_of(&config.public_url).is_some_and(|own| own == claimed) {
        return true;
    }
    let development = config.public_url.is_empty() || is_loopback_origin(&config.public_url);
    development && is_loopback_origin(&claimed)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let high = bytes.get(i + 1).copied().and_then(hex_value)?;
                let low = bytes.get(i + 2).copied().and_then(hex_value)?;
                out.push(high << 4 | low);
                i += 2;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The first value of `name` in an `application/x-www-form-urlencoded` body.
fn form_value(body: &[u8], name: &str) -> Option<String> {
    std::str::from_utf8(body)
        .ok()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .and_then(|(_, value)| percent_decode(value))
}

/// Where a form action sends the browser back to: the admin page with its filters, nothing else.
fn safe_return(path: &str) -> bool {
    let query_ok = |query: &str| {
        query
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'=' | b'&' | b'-' | b'_'))
    };
    path == "/admin" || path.strip_prefix("/admin?").is_some_and(query_ok)
}

fn forbidden(error: &'static str) -> Response {
    api_error(StatusCode::FORBIDDEN, error)
}

fn record_failure(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>, now: Instant) {
    if let Some(peer) = peer {
        state
            .limiter
            .record_failure(client_ip(peer, headers, &state.config), now);
    }
}

fn no_store(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // The forms of these pages post back to this server, and `no-referrer` would make the browser send
    // `Origin: null` with them, which the origin check refuses.
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
}

/// Hands the handlers who the request is from: [`Admin`] always, [`CanChange`] for the `admin` role only. Every
/// log line of the request carries the account's name and role through the span it runs in.
async fn admitted(request: Request, next: Next, identity: &Identity, csrf: Option<String>) -> Response {
    let mut request = request;
    let span = tracing::info_span!("admin", user = %identity.name, role = %identity.role.name());
    if identity.role == Role::Admin {
        request.extensions_mut().insert(CanChange(()));
    }
    request.extensions_mut().insert(Admin {
        user: identity.name.clone(),
        role: identity.role,
        csrf,
    });
    next.run(request).instrument(span).await
}

/// Lets an `/admin` request through with a valid Bearer key (unchanged, no CSRF) or a valid session cookie; with a
/// session every method but GET and HEAD also needs a same-origin `Origin` and the session's CSRF token, from the
/// `X-CSRF-Token` header or the `csrf` form field. With neither credential, every path under `/admin` except the
/// login form answers the one uniform 404 and counts as an authentication failure, unless what failed is only a
/// well-formed session cookie: that is a browser left open past its session, and a session id cannot be guessed, so
/// counting it would only lock the maintainer out.
pub async fn gate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_login = path == LOGIN_PATH && matches!(*request.method(), Method::GET | Method::POST);
    if !is_admin_path(path) || is_login {
        return next.run(request).await;
    }
    let now = Instant::now();
    if let Some(key) = bearer_key(request.headers())
        && let Some(identity) = state.accounts.verify(key).await
    {
        return admitted(request, next, &identity, None).await;
    }
    let cookie = session_cookie(request.headers());
    let live = cookie.and_then(|id| state.sessions.touch(id, now));
    let Some(live) = live else {
        if cookie.is_none() || bearer_key(request.headers()).is_some() {
            let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|info| info.0);
            record_failure(&state, request.headers(), peer, now);
        }
        return not_found();
    };
    let mut return_to = None;
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        if !is_same_origin(&state.config, request.headers()) {
            return forbidden("cross_origin");
        }
        let (parts, body) = request.into_parts();
        let Ok(body) = to_bytes(body, MAX_FORM_BYTES).await else {
            return api_error(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
        };
        let is_form = parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/x-www-form-urlencoded"));
        let field = |name| if is_form { form_value(&body, name) } else { None };
        let candidate = parts
            .headers
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| field(CSRF_FIELD))
            .unwrap_or_default();
        if !matches(&live.csrf_hash, candidate.as_bytes()) {
            return forbidden("csrf");
        }
        return_to = field("return").filter(|path| safe_return(path));
        request = Request::from_parts(parts, Body::from(body));
    }
    let mut response = admitted(request, next, &live.identity, Some(live.csrf.clone())).await;
    if let Some(path) = return_to.filter(|_| response.status() == StatusCode::NO_CONTENT)
        && let Ok(location) = HeaderValue::from_str(&path)
    {
        response = (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response();
    }
    if !response.headers().contains_key(SET_COOKIE)
        && let Ok(value) = HeaderValue::from_str(&live.csrf)
    {
        response.headers_mut().insert(CSRF_HEADER, value);
    }
    no_store(&mut response);
    response
}

fn cookie(value: &str, max_age: u64) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}"
    ))
    .ok()
}

pub async fn login_form() -> Response {
    let body = format!(
        "<form method=\"post\" action=\"{LOGIN_PATH}\"><label>Clave de acceso\
         <input type=\"password\" name=\"key\" autocomplete=\"current-password\" required></label>\
         <button type=\"submit\">Entrar</button></form>"
    );
    let mut response = Html(layout("Administración", &body)).into_response();
    no_store(&mut response);
    response
}

/// A wrong key, a missing one, a disabled or removed account and a cross-site post all answer the uniform 404 and
/// count as a failure.
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let now = Instant::now();
    let (parts, body) = request.into_parts();
    let key = if is_same_origin(&state.config, &parts.headers) {
        to_bytes(body, MAX_FORM_BYTES)
            .await
            .ok()
            .and_then(|body| form_value(&body, "key"))
    } else {
        None
    };
    let identity = match key {
        Some(key) => state.accounts.verify(key.trim()).await,
        None => None,
    };
    let Some(identity) = identity else {
        record_failure(&state, &parts.headers, Some(peer), now);
        return not_found();
    };
    let id = match state.sessions.open(&identity, now) {
        Ok(id) => id,
        Err(error) => return internal("session_open", &error),
    };
    let max_age = u64::from(state.config.admin.session_max_secs);
    let mut response = (StatusCode::SEE_OTHER, [(LOCATION, HeaderValue::from_static("/admin"))]).into_response();
    if let Some(value) = cookie(&id, max_age) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
    no_store(&mut response);
    state.accounts.record_login(&identity.id).await;
    tracing::info!(user = %identity.name, role = %identity.role.name(), "admin session opened");
    audit::record_session(&state, (&identity.name, identity.role), Action::Login).await;
    response
}

/// Reached only through the gate, so a session request here already passed the origin and CSRF checks. A Bearer
/// request closes nothing and records nothing.
pub async fn logout(admin: Admin, State(state): State<AppState>, headers: HeaderMap) -> Response {
    if session_cookie(&headers).is_some_and(|id| state.sessions.close(id)) {
        tracing::info!("admin session closed");
        audit::record_session(&state, (&admin.user, admin.role), Action::Logout).await;
    }
    let mut response = (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static(LOGIN_PATH))],
    )
        .into_response();
    if let Some(value) = cookie("", 0) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(idle: u32, max: u32, cap: u32) -> Sessions {
        Sessions::new(&AdminSessions {
            session_idle_secs: idle,
            session_max_secs: max,
            max_sessions: cap,
            max_sessions_per_user: cap,
        })
    }

    fn who(name: &str) -> Identity {
        Identity {
            id: crate::secrets::hex(name.as_bytes()),
            name: name.to_owned(),
            role: Role::Admin,
            generation: 0,
        }
    }

    fn open(store: &Sessions, now: Instant) -> Result<String, getrandom::Error> {
        store.open(&who("owner"), now)
    }

    #[test]
    fn ids_and_tokens_are_256_bits_and_differ() -> Result<(), getrandom::Error> {
        let store = sessions(60, 600, 4);
        let now = Instant::now();
        let (a, b) = (open(&store, now)?, open(&store, now)?);
        assert!(is_hex_token(&a, TOKEN_HEX) && is_hex_token(&b, TOKEN_HEX));
        assert_ne!(a, b);
        let (csrf_a, csrf_b) = (
            store.touch(&a, now).map(|l| l.csrf),
            store.touch(&b, now).map(|l| l.csrf),
        );
        assert!(csrf_a.as_deref().is_some_and(|t| is_hex_token(t, TOKEN_HEX)));
        assert_ne!(csrf_a, csrf_b);
        assert_ne!(csrf_a.as_deref(), Some(a.as_str()));
        Ok(())
    }

    #[test]
    fn idle_session_ends_and_activity_keeps_it() -> Result<(), getrandom::Error> {
        let store = sessions(60, 600, 4);
        let t0 = Instant::now();
        let id = open(&store, t0)?;
        assert!(store.touch(&id, t0 + Duration::from_secs(59)).is_some());
        assert!(
            store.touch(&id, t0 + Duration::from_secs(118)).is_some(),
            "the touch restarted the idle clock"
        );
        assert!(store.touch(&id, t0 + Duration::from_secs(178)).is_none());
        assert_eq!(store.len(), 0, "the expired session is gone, not just refused");
        Ok(())
    }

    #[test]
    fn active_session_still_ends_at_its_absolute_lifetime() -> Result<(), getrandom::Error> {
        let store = sessions(60, 150, 4);
        let t0 = Instant::now();
        let id = open(&store, t0)?;
        for step in 1..=2 {
            assert!(store.touch(&id, t0 + Duration::from_secs(50 * step)).is_some());
        }
        assert!(store.touch(&id, t0 + Duration::from_secs(150)).is_none());
        Ok(())
    }

    #[test]
    fn past_the_cap_the_oldest_session_ends() -> Result<(), getrandom::Error> {
        let store = sessions(60, 600, 2);
        let t0 = Instant::now();
        let first = open(&store, t0)?;
        let second = open(&store, t0 + Duration::from_secs(1))?;
        assert!(store.touch(&first, t0 + Duration::from_secs(2)).is_some());
        let third = open(&store, t0 + Duration::from_secs(3))?;
        assert_eq!(store.len(), 2);
        let now = t0 + Duration::from_secs(4);
        assert!(
            store.touch(&first, now).is_none(),
            "oldest by creation, even if used last"
        );
        assert!(store.touch(&second, now).is_some() && store.touch(&third, now).is_some());
        Ok(())
    }

    #[test]
    fn expired_sessions_are_swept_before_evicting_a_live_one() -> Result<(), getrandom::Error> {
        let store = sessions(10, 600, 2);
        let t0 = Instant::now();
        open(&store, t0)?;
        let live = open(&store, t0 + Duration::from_secs(8))?;
        open(&store, t0 + Duration::from_secs(12))?;
        assert!(store.touch(&live, t0 + Duration::from_secs(13)).is_some());
        Ok(())
    }

    #[test]
    fn closed_session_is_gone() -> Result<(), getrandom::Error> {
        let store = sessions(60, 600, 4);
        let now = Instant::now();
        let id = open(&store, now)?;
        assert!(store.close(&id));
        assert!(!store.close(&id));
        assert!(store.touch(&id, now).is_none());
        Ok(())
    }

    #[test]
    fn per_user_cap_ends_only_that_users_oldest_and_the_global_cap_the_busiest() -> Result<(), getrandom::Error> {
        let store = Sessions::new(&AdminSessions {
            session_idle_secs: 60,
            session_max_secs: 600,
            max_sessions: 4,
            max_sessions_per_user: 2,
        });
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let ana = store.open(&who("ana"), at(0))?;
        let first = store.open(&who("owner"), at(1))?;
        let second = store.open(&who("owner"), at(2))?;
        let third = store.open(&who("owner"), at(3))?;
        assert!(store.touch(&first, at(4)).is_none(), "owner's oldest ended");
        assert!(store.touch(&ana, at(4)).is_some(), "ana's session stays");
        assert!(store.touch(&second, at(4)).is_some() && store.touch(&third, at(4)).is_some());
        let bo = store.open(&who("bo"), at(5))?;
        store.open(&who("cy"), at(6))?;
        assert_eq!(store.len(), 4);
        assert!(
            store.touch(&second, at(7)).is_none(),
            "the busiest user gives up its oldest"
        );
        for kept in [&ana, &third, &bo] {
            assert!(store.touch(kept, at(7)).is_some());
        }
        Ok(())
    }

    #[test]
    fn revalidate_ends_gone_and_rekeyed_accounts_and_follows_roles() -> Result<(), getrandom::Error> {
        let store = sessions(60, 600, 8);
        let now = Instant::now();
        let kept = store.open(&who("owner"), now)?;
        let rekeyed = store.open(&who("ana"), now)?;
        let removed = store.open(&who("bo"), now)?;
        let owner_id = who("owner").id;
        let ana_id = who("ana").id;
        store.revalidate(|id| {
            if id == owner_id {
                Some((Role::Viewer, 0))
            } else if id == ana_id {
                Some((Role::Admin, 1))
            } else {
                None
            }
        });
        assert!(store.touch(&rekeyed, now).is_none() && store.touch(&removed, now).is_none());
        assert_eq!(
            store.touch(&kept, now).map(|live| live.identity.role),
            Some(Role::Viewer)
        );
        Ok(())
    }

    fn headers(pairs: &[(HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            if let Ok(value) = HeaderValue::from_str(value) {
                map.append(name.clone(), value);
            }
        }
        map
    }

    #[test]
    fn origin_must_be_the_public_url_or_loopback_in_development() {
        let production = Config {
            public_url: "https://reports.example.org".to_owned(),
            ..Config::default()
        };
        let own = headers(&[(ORIGIN, "https://Reports.Example.org")]);
        assert!(is_same_origin(&production, &own));
        for refused in [
            headers(&[(ORIGIN, "https://evil.example")]),
            headers(&[(ORIGIN, "http://reports.example.org")]),
            headers(&[(ORIGIN, "https://reports.example.org.evil.example")]),
            headers(&[(ORIGIN, "null")]),
            headers(&[(ORIGIN, "http://127.0.0.1:8787")]),
            headers(&[]),
        ] {
            assert!(!is_same_origin(&production, &refused), "{refused:?}");
        }
        let referer = headers(&[(REFERER, "https://reports.example.org/admin?page=2")]);
        assert!(is_same_origin(&production, &referer));
        let both = headers(&[
            (ORIGIN, "https://evil.example"),
            (REFERER, "https://reports.example.org/admin"),
        ]);
        assert!(!is_same_origin(&production, &both), "Origin wins over Referer");
        let development = Config::default();
        for local in ["http://127.0.0.1:8787", "http://localhost:8787", "http://[::1]:8787"] {
            assert!(is_same_origin(&development, &headers(&[(ORIGIN, local)])), "{local}");
        }
        assert!(!is_same_origin(&development, &headers(&[(ORIGIN, "http://192.0.2.1")])));
    }

    #[test]
    fn cookie_is_read_by_exact_name_and_shape() {
        let id = "a".repeat(TOKEN_HEX);
        let jar = headers(&[(COOKIE, &format!("other=1; {COOKIE_NAME}={id}"))]);
        assert_eq!(session_cookie(&jar), Some(id.as_str()));
        for bad in [
            format!("session={id}"),
            format!("{COOKIE_NAME}={}", "A".repeat(TOKEN_HEX)),
            format!("{COOKIE_NAME}={}", "a".repeat(32)),
        ] {
            assert_eq!(session_cookie(&headers(&[(COOKIE, &bad)])), None, "{bad}");
        }
    }

    #[test]
    fn form_values_are_decoded_and_return_paths_stay_on_the_admin_page() {
        let body = b"csrf=ab12&return=%2Fadmin%3Fproduct%3Dpsport%26page%3D2&key=a+b%21";
        assert_eq!(form_value(body, "csrf").as_deref(), Some("ab12"));
        assert_eq!(form_value(body, "key").as_deref(), Some("a b!"));
        assert_eq!(form_value(body, "missing"), None);
        assert_eq!(form_value(b"key=%zz", "key"), None);
        let back = form_value(body, "return").unwrap_or_default();
        assert!(safe_return(&back));
        for bad in [
            "//evil.example",
            "/admin/../x",
            "https://evil.example/admin",
            "/admin?x=<",
            "/adminx",
        ] {
            assert!(!safe_return(bad), "{bad}");
        }
    }
}
