//! The admin session and the admin page through the whole router: login, cookie, CSRF, origin, logout, the uniform
//! 404, the page's escaping, the two roles and what a refresh of the accounts ends. Kept apart from `app`'s tests so
//! the two suites grow without touching each other.

use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use tower::ServiceExt;

use crate::accounts::{Accounts, TEST_ADMIN_KEY, TEST_VIEWER_KEY, refresh_now, test_accounts};
use crate::app::{AppState, router};
use crate::config::Config;
use crate::secrets::sha256;
use crate::store::{NewReport, Product, Role, test_stores};

type Test = Result<(), Box<dyn Error>>;
type Answer = (StatusCode, HeaderMap, Vec<u8>);

const ADMIN_KEY: &str = TEST_ADMIN_KEY;
const VIEWER_KEY: &str = TEST_VIEWER_KEY;
const ORIGIN: &str = "http://127.0.0.1:8787";
const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");

struct Harness {
    state: AppState,
    root: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Login {
    cookie: String,
    csrf: String,
}

impl Harness {
    fn new(tweak: impl FnOnce(&mut Config)) -> Result<Self, Box<dyn Error>> {
        Self::with_accounts(test_accounts, tweak)
    }

    fn with_accounts(
        accounts: impl FnOnce(&std::path::Path) -> Accounts,
        tweak: impl FnOnce(&mut Config),
    ) -> Result<Self, Box<dyn Error>> {
        let mut config = Config::default();
        config.limits.failures_per_ip = 1000;
        tweak(&mut config);
        let root = std::env::temp_dir().join(format!("decanter-reports-admin-{}", crate::secrets::random_hex()?));
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            state: AppState::new(config, accounts(&root), test_stores(&root)),
            root,
        })
    }

    async fn send_from(
        &self,
        ip: [u8; 4],
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<Answer, Box<dyn Error>> {
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body.to_owned()))?;
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((ip, 4000))));
        let response = router(self.state.clone()).oneshot(request).await?;
        let (parts, body) = response.into_parts();
        Ok((parts.status, parts.headers, to_bytes(body, usize::MAX).await?.to_vec()))
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<Answer, Box<dyn Error>> {
        self.send_from([203, 0, 113, 7], method, uri, headers, body).await
    }

    async fn login(&self) -> Result<Login, Box<dyn Error>> {
        self.login_with(ADMIN_KEY).await
    }

    async fn login_with(&self, key: &str) -> Result<Login, Box<dyn Error>> {
        let (status, headers, _) = self
            .send(
                Method::POST,
                "/admin/login",
                &[FORM, ("origin", ORIGIN)],
                &format!("key={key}"),
            )
            .await?;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let set = header(&headers, "set-cookie");
        let cookie = set.split(';').next().unwrap_or_default().to_owned();
        let (status, page_headers, _) = self
            .send(Method::GET, "/admin/reports", &[("cookie", &cookie)], "")
            .await?;
        assert_eq!(status, StatusCode::OK);
        Ok(Login {
            cookie,
            csrf: header(&page_headers, "x-csrf-token"),
        })
    }

    async fn insert(
        &self,
        product: Product,
        fingerprint: &str,
        template: &str,
        title: Option<&str>,
    ) -> Result<String, Box<dyn Error>> {
        let id = product.new_id()?;
        let new = NewReport {
            id: id.clone(),
            delete_hash: sha256(b"code"),
            fingerprint: fingerprint.to_owned(),
            product: product.name(),
            title_id: title.map(str::to_owned),
            game_name: None,
            failure_kind: template.to_owned(),
            template_id: template.to_owned(),
            message: template.to_owned(),
            created_day: "2026-10-27".to_owned(),
            received_day: "2026-10-28".to_owned(),
            report_json: "{}".to_owned(),
        };
        self.state.stores.get(product).insert(&new, 1000).await?;
        Ok(id)
    }

    async fn reads_with_cookie(&self, cookie: &str) -> Result<StatusCode, Box<dyn Error>> {
        Ok(self
            .send(Method::GET, "/admin/reports", &[("cookie", cookie)], "")
            .await?
            .0)
    }

    async fn reads_with_key(&self, key: &str) -> Result<StatusCode, Box<dyn Error>> {
        let auth = format!("Bearer {key}");
        Ok(self
            .send(Method::GET, "/admin/reports", &[("authorization", &auth)], "")
            .await?
            .0)
    }

    async fn page_of(&self, login: &Login, uri: &str) -> Result<String, Box<dyn Error>> {
        let (_, _, body) = self.send(Method::GET, uri, &[("cookie", &login.cookie)], "").await?;
        Ok(String::from_utf8(body)?)
    }

    async fn is_hidden(&self, id: &str) -> Result<bool, Box<dyn Error>> {
        let rows = self
            .state
            .stores
            .admin_list(&crate::store::AdminFilter::default(), 500, 0)
            .await?;
        Ok(rows.iter().any(|row| row.id == id && row.hidden))
    }
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn login_sets_a_host_only_strict_cookie_and_opens_the_admin_page() -> Test {
    let h = Harness::new(|_| {})?;
    let (status, headers, _) = h.send(Method::GET, "/admin/login", &[], "").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert!(header(&headers, "content-security-policy").starts_with("default-src 'none'"));
    let (status, headers, _) = h
        .send(
            Method::POST,
            "/admin/login",
            &[FORM, ("origin", ORIGIN)],
            &format!("key={ADMIN_KEY}"),
        )
        .await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(header(&headers, "location"), "/admin");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    let cookie = header(&headers, "set-cookie");
    let (pair, attributes) = cookie.split_once(';').ok_or("cookie attributes")?;
    let id = pair.strip_prefix("__Host-session=").ok_or("cookie name")?;
    assert!(crate::secrets::is_hex_token(id, 64), "256 random bits");
    for flag in ["Path=/", "Secure", "HttpOnly", "SameSite=Strict", "Max-Age=28800"] {
        assert!(
            attributes.split(';').any(|part| part.trim() == flag),
            "{flag} in {cookie}"
        );
    }
    assert!(!cookie.to_ascii_lowercase().contains("domain"), "{cookie}");
    assert_eq!(h.login().await?.csrf.len(), 64);
    Ok(())
}

#[tokio::test]
async fn failed_login_is_the_uniform_404_and_counts_as_a_failure() -> Test {
    let h = Harness::new(|config| config.limits.failures_per_ip = 3)?;
    let uniform = h
        .send_from([198, 51, 100, 1], Method::GET, "/admin/nothing", &[], "")
        .await?;
    let wrong = format!("key={}", "x".repeat(40));
    let right = format!("key={ADMIN_KEY}");
    for (headers, body) in [
        (&[FORM, ("origin", ORIGIN)][..], wrong.as_str()),
        (&[FORM, ("origin", "https://evil.example")][..], right.as_str()),
        (&[FORM][..], ""),
    ] {
        let answer = h.send(Method::POST, "/admin/login", headers, body).await?;
        assert_eq!(answer, uniform, "{headers:?} {body}");
    }
    let (status, headers, _) = h.send(Method::GET, "/admin/login", &[], "").await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "three failures spent the budget");
    assert!(headers.get("retry-after").is_some());
    assert_eq!(h.state.sessions.len(), 0);
    Ok(())
}

#[tokio::test]
async fn without_credentials_every_admin_path_is_the_same_404() -> Test {
    let h = Harness::new(|_| {})?;
    let id = format!("d{}", "0".repeat(32));
    let fp = "0".repeat(16);
    let baseline = h.send(Method::GET, "/admin/reports", &[], "").await?;
    assert_eq!(baseline.0, StatusCode::NOT_FOUND);
    assert!(baseline.1.get("allow").is_none());
    let stale = format!("__Host-session={}", "a".repeat(64));
    let credentials: [&[(&str, &str)]; 4] = [
        &[],
        &[("authorization", "Bearer nope")],
        &[("cookie", stale.as_str())],
        &[
            ("cookie", "__Host-session=garbage"),
            ("origin", ORIGIN),
            ("x-csrf-token", "x"),
        ],
    ];
    for headers in credentials {
        for (method, path) in [
            (Method::GET, "/admin".to_owned()),
            (Method::GET, "/admin/".to_owned()),
            (Method::GET, "/admin?product=psport".to_owned()),
            (Method::GET, "/admin/reports".to_owned()),
            (Method::GET, "/admin/unknown/path".to_owned()),
            (Method::PATCH, "/admin/reports".to_owned()),
            (Method::PUT, "/admin/login".to_owned()),
            (Method::DELETE, "/admin/login".to_owned()),
            (Method::POST, "/admin/logout".to_owned()),
            (Method::POST, format!("/admin/reports/{id}/hide")),
            (Method::POST, format!("/admin/reports/{id}/delete")),
            (Method::DELETE, format!("/admin/reports/{id}")),
            (Method::GET, format!("/admin/reports/{id}/attachment")),
            (Method::POST, format!("/admin/groups/{fp}/resolve")),
            (Method::DELETE, format!("/admin/groups/{fp}")),
        ] {
            let answer = h.send(method.clone(), &path, headers, "").await?;
            assert_eq!(answer, baseline, "{method} {path} {headers:?}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn session_actions_need_the_csrf_token_and_the_own_origin() -> Test {
    let h = Harness::new(|_| {})?;
    let id = h.insert(Product::Decanter, &"a".repeat(16), "t", None).await?;
    let session = h.login().await?;
    let path = format!("/admin/reports/{id}/hide");
    let cookie = ("cookie", session.cookie.as_str());
    let wrong_token = "0".repeat(64);
    for (headers, error) in [
        (vec![cookie, ("origin", ORIGIN)], "csrf"),
        (vec![cookie, ("origin", ORIGIN), ("x-csrf-token", &wrong_token)], "csrf"),
        (vec![cookie, ("origin", ORIGIN), ("x-csrf-token", "short")], "csrf"),
        (
            vec![
                cookie,
                ("origin", "https://evil.example"),
                ("x-csrf-token", &session.csrf),
            ],
            "cross_origin",
        ),
        (
            vec![cookie, ("origin", "null"), ("x-csrf-token", &session.csrf)],
            "cross_origin",
        ),
        (vec![cookie, ("x-csrf-token", &session.csrf)], "cross_origin"),
    ] {
        let (status, _, body) = h.send(Method::POST, &path, &headers, "").await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body)?["error"], error);
    }
    let wrong_form = format!("csrf={wrong_token}");
    let (status, ..) = h
        .send(Method::POST, &path, &[cookie, ("origin", ORIGIN), FORM], &wrong_form)
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!h.is_hidden(&id).await?, "no refused request changed anything");

    let referer = format!("{ORIGIN}/admin");
    let (status, ..) = h
        .send(
            Method::POST,
            &path,
            &[cookie, ("referer", &referer), ("x-csrf-token", &session.csrf)],
            "",
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a tool sends the header; Referer stands in for Origin"
    );
    assert!(h.is_hidden(&id).await?);

    let form = format!("csrf={}&return=%2Fadmin%3Fproduct%3Ddecanter", session.csrf);
    let restore = format!("/admin/reports/{id}/restore");
    let (status, headers, _) = h
        .send(Method::POST, &restore, &[cookie, ("origin", ORIGIN), FORM], &form)
        .await?;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "a form goes back to the page it came from"
    );
    assert_eq!(header(&headers, "location"), "/admin?product=decanter");
    assert!(!h.is_hidden(&id).await?);

    let escape = format!("csrf={}&return=https%3A%2F%2Fevil.example", session.csrf);
    let hide = h
        .send(Method::POST, &path, &[cookie, ("origin", ORIGIN), FORM], &escape)
        .await?;
    assert_eq!(
        hide.0,
        StatusCode::NO_CONTENT,
        "a foreign return path is ignored, not followed"
    );
    Ok(())
}

#[tokio::test]
async fn bearer_needs_no_csrf_and_no_origin() -> Test {
    let h = Harness::new(|_| {})?;
    let fp = "b".repeat(16);
    let id = h.insert(Product::Psport, &fp, "t", Some("CUSA00001")).await?;
    let auth = format!("Bearer {ADMIN_KEY}");
    let bearer = [("authorization", auth.as_str())];
    let (status, ..) = h
        .send(Method::POST, &format!("/admin/reports/{id}/hide"), &bearer, "")
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, headers, body) = h.send(Method::GET, "/admin/reports", &bearer, "").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("x-csrf-token").is_none());
    assert!(String::from_utf8(body)?.contains(&id));
    let (status, ..) = h
        .send(Method::POST, &format!("/admin/groups/{fp}/delete"), &bearer, "")
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(h.state.stores.count().await?, 0);
    Ok(())
}

#[tokio::test]
async fn logout_needs_csrf_and_ends_the_session() -> Test {
    let h = Harness::new(|_| {})?;
    let session = h.login().await?;
    let cookie = ("cookie", session.cookie.as_str());
    let (refused, ..) = h
        .send(Method::POST, "/admin/logout", &[cookie, ("origin", ORIGIN)], "")
        .await?;
    assert_eq!(refused, StatusCode::FORBIDDEN);
    assert_eq!(
        h.send(Method::GET, "/admin/reports", &[cookie], "").await?.0,
        StatusCode::OK
    );
    let form = format!("csrf={}", session.csrf);
    let (status, headers, _) = h
        .send(
            Method::POST,
            "/admin/logout",
            &[cookie, ("origin", ORIGIN), FORM],
            &form,
        )
        .await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(header(&headers, "location"), "/admin/login");
    assert!(header(&headers, "set-cookie").contains("Max-Age=0"));
    assert!(headers.get("x-csrf-token").is_none());
    let uniform = h.send(Method::GET, "/admin/reports", &[], "").await?;
    assert_eq!(h.send(Method::GET, "/admin/reports", &[cookie], "").await?, uniform);
    assert_eq!(h.state.sessions.len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_login_past_max_sessions_ends_the_oldest() -> Test {
    let h = Harness::new(|config| config.admin.max_sessions = 2)?;
    let first = h.login().await?;
    let second = h.login().await?;
    let third = h.login().await?;
    assert_eq!(h.state.sessions.len(), 2);
    for (login, expected) in [
        (first, StatusCode::NOT_FOUND),
        (second, StatusCode::OK),
        (third, StatusCode::OK),
    ] {
        let (status, ..) = h
            .send(Method::GET, "/admin/reports", &[("cookie", &login.cookie)], "")
            .await?;
        assert_eq!(status, expected);
    }
    Ok(())
}

#[tokio::test]
async fn admin_page_escapes_hostile_values_and_carries_the_token_in_every_form() -> Test {
    let h = Harness::new(|_| {})?;
    let hostile = "<script>alert(1)</script>\"'><img src=x onerror=alert(2)>";
    let fp = "c".repeat(16);
    h.insert(Product::Psport, &fp, hostile, Some("<b>CUSA\"x")).await?;
    let session = h.login().await?;
    let (status, headers, body) = h
        .send(
            Method::GET,
            &format!("/admin?fingerprint={fp}"),
            &[("cookie", &session.cookie)],
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert_eq!(header(&headers, "referrer-policy"), "same-origin");
    let csp = header(&headers, "content-security-policy");
    for directive in [
        "default-src 'none'",
        "form-action 'self'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directive), "{csp}");
    }
    let page = String::from_utf8(body)?;
    assert!(
        !page.contains("<script") && !page.contains("<img") && !page.contains("<b>CUSA"),
        "{page}"
    );
    assert!(page.contains("&lt;script&gt;"));
    let forms = page.matches("<form method=\"post\"").count();
    let tokens = page
        .matches(&format!("name=\"csrf\" value=\"{}\"", session.csrf))
        .count();
    assert!(forms >= 8, "row and group actions plus logout: {forms}");
    assert_eq!(forms, tokens);
    for path in [
        "/hide\"",
        "/resolve\"",
        "/delete\"",
        &format!("/admin/groups/{fp}/restore\""),
    ] {
        assert!(page.contains(path), "{path}");
    }
    let (bad, ..) = h
        .send(
            Method::GET,
            "/admin?day=%3Cscript%3E",
            &[("cookie", &session.cookie)],
            "",
        )
        .await?;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    let auth = format!("Bearer {ADMIN_KEY}");
    let (status, headers, body) = h.send(Method::GET, "/admin", &[("authorization", &auth)], "").await?;
    assert_eq!(status, StatusCode::OK, "the key opens the page too");
    assert!(headers.get("x-csrf-token").is_none());
    assert!(String::from_utf8(body)?.contains(&fp));
    Ok(())
}

#[tokio::test]
async fn without_admin_accounts_every_key_gets_the_uniform_404() -> Test {
    let h = Harness::with_accounts(
        |root| Accounts::new(crate::store::test_accounts(&root.join("accounts"))),
        |_| {},
    )?;
    let uniform = h.send(Method::GET, "/admin/nothing", &[], "").await?;
    let bearer = format!("Bearer {ADMIN_KEY}");
    let answer = h
        .send(Method::GET, "/admin/reports", &[("authorization", &bearer)], "")
        .await?;
    assert_eq!(answer, uniform);
    let answer = h
        .send(
            Method::POST,
            "/admin/login",
            &[FORM, ("origin", ORIGIN)],
            &format!("key={ADMIN_KEY}"),
        )
        .await?;
    assert_eq!(answer, uniform);
    assert_eq!(h.state.sessions.len(), 0);
    Ok(())
}

fn error_of(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[tokio::test]
async fn a_viewer_reads_everything_and_changes_nothing() -> Test {
    let h = Harness::new(|_| {})?;
    let fp = "d".repeat(16);
    let id = h.insert(Product::Decanter, &fp, "t", None).await?;
    h.state
        .stores
        .get(Product::Decanter)
        .attach(&id, b"raw log", b"", 10)
        .await?;
    let session = h.login_with(VIEWER_KEY).await?;
    let cookie = ("cookie", session.cookie.as_str());
    let bearer = format!("Bearer {VIEWER_KEY}");
    for credentials in [vec![cookie], vec![("authorization", bearer.as_str())]] {
        for path in [
            "/admin".to_owned(),
            "/admin/reports".to_owned(),
            format!("/admin/reports/{id}/attachment"),
            "/admin/logs".to_owned(),
            format!("/admin/logs/{id}"),
            format!("/admin/logs/search?q=raw&fingerprint={fp}"),
            format!("/admin/logs/diff?a={id}&b={id}"),
        ] {
            let (status, ..) = h.send(Method::GET, &path, &credentials, "").await?;
            assert_eq!(status, StatusCode::OK, "{path} {credentials:?}");
        }
    }
    let changes = [
        (Method::POST, format!("/admin/reports/{id}/hide")),
        (Method::POST, format!("/admin/reports/{id}/restore")),
        (Method::POST, format!("/admin/reports/{id}/resolve")),
        (Method::POST, format!("/admin/reports/{id}/delete")),
        (Method::DELETE, format!("/admin/reports/{id}")),
        (Method::POST, format!("/admin/groups/{fp}/hide")),
        (Method::POST, format!("/admin/groups/{fp}/restore")),
        (Method::POST, format!("/admin/groups/{fp}/resolve")),
        (Method::POST, format!("/admin/groups/{fp}/delete")),
        (Method::DELETE, format!("/admin/groups/{fp}")),
    ];
    for (method, path) in &changes {
        let (status, _, body) = h
            .send(
                method.clone(),
                path,
                &[cookie, ("origin", ORIGIN), ("x-csrf-token", &session.csrf)],
                "",
            )
            .await?;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, "role".to_owned()),
            "{path}"
        );
        let (status, _, body) = h
            .send(method.clone(), path, &[("authorization", bearer.as_str())], "")
            .await?;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, "role".to_owned()),
            "{path}"
        );
    }
    let form = format!("csrf={}&return=%2Fadmin", session.csrf);
    let (status, _, page) = h
        .send(
            Method::POST,
            &format!("/admin/reports/{id}/hide"),
            &[cookie, ("origin", ORIGIN), FORM],
            &form,
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "a form gets the page, not a redirect");
    assert!(String::from_utf8(page)?.contains("Tu cuenta es de solo lectura: no puede ocultar, resolver ni borrar."));
    assert!(!h.is_hidden(&id).await?);
    let rows = h
        .state
        .stores
        .admin_list(&crate::store::AdminFilter::default(), 10, 0)
        .await?;
    assert!(rows.len() == 1 && rows.iter().all(|row| row.resolved_at.is_none() && row.has_attachment));
    Ok(())
}

#[tokio::test]
async fn the_page_names_the_session_and_offers_actions_to_admins_only() -> Test {
    let h = Harness::new(|_| {})?;
    let fp = "e".repeat(16);
    h.insert(Product::Psport, &fp, "t", Some("CUSA00001")).await?;
    let uri = format!("/admin?fingerprint={fp}");
    let viewer = h.page_of(&h.login_with(VIEWER_KEY).await?, &uri).await?;
    assert!(viewer.contains("Sesión de <strong>ana</strong> (lectura)"), "{viewer}");
    assert_eq!(viewer.matches("<form method=\"post\"").count(), 1, "logout only");
    for action in ["/hide\"", "/restore\"", "/resolve\"", "/delete\""] {
        assert!(!viewer.contains(action), "{action}");
    }
    let admin = h.page_of(&h.login().await?, &uri).await?;
    assert!(admin.contains("Sesión de <strong>owner</strong> (administración)"));
    assert!(admin.matches("<form method=\"post\"").count() >= 8);
    let (_, _, login_form) = h.send(Method::GET, "/admin/login", &[], "").await?;
    assert!(String::from_utf8(login_form)?.contains("Clave de acceso"));
    Ok(())
}

#[tokio::test]
async fn remove_and_reset_end_sessions_and_keys_at_the_refresh() -> Test {
    let h = Harness::with_accounts(
        |root| Accounts::new(crate::store::test_accounts(&root.join("accounts"))),
        |_| {},
    )?;
    let store = h.state.accounts.store();
    let owner = store
        .add("owner", Role::Admin, "2026-10-28", false)
        .await?
        .key()
        .ok_or("owner")?;
    let ana = store
        .add("ana", Role::Admin, "2026-10-28", false)
        .await?
        .key()
        .ok_or("ana")?;
    let bo = store
        .add("bo", Role::Viewer, "2026-10-28", false)
        .await?
        .key()
        .ok_or("bo")?;
    refresh_now(&h.state).await?;
    let (owner_session, ana_session, bo_session) = (
        h.login_with(&owner).await?,
        h.login_with(&ana).await?,
        h.login_with(&bo).await?,
    );
    assert!(matches!(store.remove("bo").await?, crate::store::AccountChange::Done));
    let crate::store::AccountChange::Key(new_ana) = store.reset("ana").await? else {
        return Err("reset gave no key".into());
    };
    assert!(matches!(
        store.set_role("owner", Role::Viewer).await?,
        crate::store::AccountChange::Done
    ));
    assert_eq!(
        h.reads_with_key(&bo).await?,
        StatusCode::OK,
        "nothing changes before the refresh"
    );
    assert_eq!(h.reads_with_cookie(&ana_session.cookie).await?, StatusCode::OK);

    refresh_now(&h.state).await?;
    let uniform = h.send(Method::GET, "/admin/reports", &[], "").await?.0;
    for (what, status) in [
        ("bo's session", h.reads_with_cookie(&bo_session.cookie).await?),
        ("bo's key", h.reads_with_key(&bo).await?),
        ("ana's session", h.reads_with_cookie(&ana_session.cookie).await?),
        ("ana's old key", h.reads_with_key(&ana).await?),
    ] {
        assert_eq!(status, uniform, "{what}");
    }
    assert_eq!(h.reads_with_key(&new_ana).await?, StatusCode::OK);
    assert_eq!(
        h.reads_with_cookie(&owner_session.cookie).await?,
        StatusCode::OK,
        "a role change keeps the session"
    );
    let id = h.insert(Product::Decanter, &"f".repeat(16), "t", None).await?;
    let (status, ..) = h
        .send(
            Method::POST,
            &format!("/admin/reports/{id}/hide"),
            &[
                ("cookie", &owner_session.cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &owner_session.csrf),
            ],
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "the session follows the new role");
    assert_eq!(h.state.sessions.len(), 1);
    Ok(())
}

#[tokio::test]
async fn an_account_added_after_the_refresh_works_at_once() -> Test {
    let h = Harness::with_accounts(
        |root| Accounts::new(crate::store::test_accounts(&root.join("accounts"))),
        |_| {},
    )?;
    let key = h
        .state
        .accounts
        .store()
        .add("late", Role::Viewer, "2026-10-28", false)
        .await?
        .key()
        .ok_or("late")?;
    let auth = format!("Bearer {key}");
    let (status, ..) = h
        .send(Method::GET, "/admin/reports", &[("authorization", &auth)], "")
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.state.accounts.current(key.get(4..12).unwrap_or_default()),
        Some((Role::Viewer, 0))
    );
    Ok(())
}

#[tokio::test]
async fn a_login_past_the_per_user_cap_ends_only_that_users_oldest() -> Test {
    let h = Harness::new(|config| config.admin.max_sessions_per_user = 2)?;
    let viewer = h.login_with(VIEWER_KEY).await?;
    let first = h.login().await?;
    let second = h.login().await?;
    let third = h.login().await?;
    assert_eq!(
        (h.state.sessions.of_user("owner"), h.state.sessions.of_user("ana")),
        (2, 1)
    );
    for (login, expected) in [
        (viewer, StatusCode::OK),
        (first, StatusCode::NOT_FOUND),
        (second, StatusCode::OK),
        (third, StatusCode::OK),
    ] {
        let (status, ..) = h
            .send(Method::GET, "/admin/reports", &[("cookie", &login.cookie)], "")
            .await?;
        assert_eq!(status, expected);
    }
    Ok(())
}

#[tokio::test]
async fn unknown_id_and_wrong_secret_are_the_same_404_and_both_count() -> Test {
    let h = Harness::new(|config| config.limits.failures_per_ip = 4)?;
    let uniform = h
        .send_from([198, 51, 100, 2], Method::GET, "/admin/nothing", &[], "")
        .await?;
    let secret = ADMIN_KEY.get(13..).unwrap_or_default();
    let unknown_id = format!("drk_0000ffff_{secret}");
    let wrong_secret = format!("{}{}", ADMIN_KEY.get(..13).unwrap_or_default(), "9".repeat(64));
    for key in [&unknown_id, &wrong_secret] {
        let auth = format!("Bearer {key}");
        let answer = h
            .send(Method::GET, "/admin/reports", &[("authorization", &auth)], "")
            .await?;
        assert_eq!(answer, uniform, "{key}");
        let answer = h
            .send(
                Method::POST,
                "/admin/login",
                &[FORM, ("origin", ORIGIN)],
                &format!("key={key}"),
            )
            .await?;
        assert_eq!(answer, uniform, "{key}");
    }
    let (status, ..) = h.send(Method::GET, "/admin/login", &[], "").await?;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "all four misses spent the budget"
    );
    Ok(())
}

thread_local! {
    static CAPTURED: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// Writes into the calling thread's capture, if it started one, and nowhere otherwise.
struct ThreadCapture;

impl std::io::Write for ThreadCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        CAPTURED.with_borrow_mut(|captured| {
            if let Some(captured) = captured {
                captured.extend_from_slice(bytes);
            }
        });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Captures what this thread logs (a `#[tokio::test]` runs on one thread). The subscriber is global, because a
/// thread-scoped one loses events whose callsites other test threads already cached as uninteresting.
fn capture_logs() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(|| ThreadCapture)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    CAPTURED.with_borrow_mut(|captured| *captured = Some(Vec::new()));
}

fn captured_logs() -> String {
    CAPTURED.with_borrow(|captured| String::from_utf8_lossy(captured.as_deref().unwrap_or_default()).into_owned())
}

#[tokio::test]
async fn admin_log_lines_name_the_account_and_hold_no_secret() -> Test {
    capture_logs();
    let h = Harness::new(|_| {})?;
    let id = h.insert(Product::Decanter, &"a".repeat(16), "t", None).await?;
    let session = h.login().await?;
    h.send(
        Method::POST,
        &format!("/admin/reports/{id}/hide"),
        &[
            ("cookie", &session.cookie),
            ("origin", ORIGIN),
            ("x-csrf-token", &session.csrf),
        ],
        "",
    )
    .await?;
    let auth = format!("Bearer {VIEWER_KEY}");
    h.send(Method::GET, "/admin/logs", &[("authorization", &auth)], "")
        .await?;
    let text = captured_logs();
    let opened = text
        .lines()
        .find(|line| line.contains("admin session opened"))
        .unwrap_or_default();
    assert!(opened.contains("user=owner") && opened.contains("role=admin"), "{text}");
    let changed = text
        .lines()
        .find(|line| line.contains("report changed"))
        .unwrap_or_default();
    assert!(
        changed.contains("user=owner") && changed.contains("role=admin"),
        "{text}"
    );
    let cookie_id = session.cookie.split('=').nth(1).unwrap_or_default();
    let hash = crate::secrets::hex(&sha256(ADMIN_KEY.as_bytes()));
    for secret in [
        ADMIN_KEY,
        VIEWER_KEY,
        cookie_id,
        session.csrf.as_str(),
        hash.as_str(),
        "203.0.113.7",
    ] {
        assert!(!secret.is_empty() && !text.contains(secret), "{secret} in {text}");
    }
    Ok(())
}

/// Every `(method, path)` of `app::routes` under `/admin` that is not a GET, read from the router's source, since an
/// axum router cannot list its routes. Login and logout are the two any account may post.
fn mutating_admin_routes() -> Vec<(Method, String)> {
    let source = include_str!("app.rs");
    let body = source
        .split_once("fn routes(")
        .and_then(|(_, rest)| rest.split_once(".fallback(fallback)"))
        .map_or("", |(body, _)| body);
    let mut routes = Vec::new();
    for line in body.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix(".route(\"") else {
            continue;
        };
        let Some((path, handlers)) = rest.split_once('"') else {
            continue;
        };
        if !path.starts_with("/admin") || path == "/admin/logout" {
            continue;
        }
        for (call, method) in [
            ("post(", Method::POST),
            ("delete(", Method::DELETE),
            ("put(", Method::PUT),
            ("patch(", Method::PATCH),
        ] {
            if handlers.contains(call) {
                routes.push((method, path.to_owned()));
            }
        }
    }
    routes
}

#[tokio::test]
async fn a_viewer_gets_403_on_every_mutating_admin_route_of_the_router() -> Test {
    let h = Harness::new(|_| {})?;
    let fp = "9".repeat(16);
    let id = h.insert(Product::Decanter, &fp, "t", None).await?;
    let routes = mutating_admin_routes();
    assert!(routes.len() >= 10, "the route table was read: {routes:?}");
    let session = h.login_with(VIEWER_KEY).await?;
    let bearer = format!("Bearer {VIEWER_KEY}");
    for (method, template) in &routes {
        let path = template
            .split('/')
            .map(
                |segment| match segment.strip_prefix('{').and_then(|name| name.strip_suffix('}')) {
                    Some("fingerprint") => fp.as_str(),
                    Some(_) => id.as_str(),
                    None => segment,
                },
            )
            .collect::<Vec<_>>()
            .join("/");
        for credentials in [
            vec![
                ("cookie", session.cookie.as_str()),
                ("origin", ORIGIN),
                ("x-csrf-token", session.csrf.as_str()),
            ],
            vec![("authorization", bearer.as_str())],
        ] {
            let (status, _, body) = h.send(method.clone(), &path, &credentials, "").await?;
            assert_eq!(
                (status, error_of(&body)),
                (StatusCode::FORBIDDEN, "role".to_owned()),
                "{method} {template} takes CanChange"
            );
        }
    }
    assert!(!h.is_hidden(&id).await?);
    assert_eq!(h.state.stores.count().await?, 1);
    Ok(())
}

#[tokio::test]
async fn an_expired_session_cookie_is_the_uniform_404_and_no_failure() -> Test {
    let h = Harness::new(|config| config.limits.failures_per_ip = 2)?;
    let uniform = h
        .send_from([198, 51, 100, 3], Method::GET, "/admin/nothing", &[], "")
        .await?;
    let stale = format!("__Host-session={}", "a".repeat(64));
    for _ in 0..5 {
        let answer = h.send(Method::GET, "/admin/reports", &[("cookie", &stale)], "").await?;
        assert_eq!(answer, uniform);
    }
    let (status, ..) = h.send(Method::GET, "/admin/login", &[], "").await?;
    assert_eq!(status, StatusCode::OK, "a stale cookie spends no failure");
    for _ in 0..2 {
        let answer = h
            .send(
                Method::GET,
                "/admin/reports",
                &[("cookie", &stale), ("authorization", "Bearer nope")],
                "",
            )
            .await?;
        assert_eq!(answer, uniform);
    }
    let (status, ..) = h.send(Method::GET, "/admin/login", &[], "").await?;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a bad Bearer beside it still counts"
    );
    Ok(())
}

#[tokio::test]
async fn repeated_logins_leave_one_login_record() -> Test {
    let h = Harness::new(|_| {})?;
    let sessions = [h.login().await?, h.login().await?, h.login().await?];
    for session in &sessions {
        let form = format!("csrf={}", session.csrf);
        h.send(
            Method::POST,
            "/admin/logout",
            &[("cookie", &session.cookie), ("origin", ORIGIN), FORM],
            &form,
        )
        .await?;
    }
    let (records, _) = h.state.accounts.store().audit_page(None, 100).await?;
    let count = |action: &str| records.iter().filter(|record| record.action == action).count();
    assert_eq!((count("login"), count("logout")), (1, 3));
    assert_eq!(h.state.session_records.take_skipped(), 2);
    Ok(())
}
