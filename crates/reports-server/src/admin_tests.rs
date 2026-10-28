//! The admin session and the admin page through the whole router: login, cookie, CSRF, origin, logout, the uniform
//! 404 and the page's escaping. Kept apart from `app`'s tests so the two suites grow without touching each other.

use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use tower::ServiceExt;

use crate::app::{AppState, router};
use crate::config::Config;
use crate::secrets::sha256;
use crate::session::AdminKeys;
use crate::store::{NewReport, Product, test_stores};

type Test = Result<(), Box<dyn Error>>;
type Answer = (StatusCode, HeaderMap, Vec<u8>);

const ADMIN_KEY: &str = "0123456789abcdef0123456789abcdef-admin";
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
        Self::with_keys(AdminKeys::single(ADMIN_KEY), tweak)
    }

    fn with_keys(keys: AdminKeys, tweak: impl FnOnce(&mut Config)) -> Result<Self, Box<dyn Error>> {
        let mut config = Config::default();
        config.limits.failures_per_ip = 1000;
        tweak(&mut config);
        let root = std::env::temp_dir().join(format!("decanter-reports-admin-{}", crate::secrets::random_hex()?));
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            state: AppState::new(config, keys, test_stores(&root)),
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
        let (status, headers, _) = self
            .send(
                Method::POST,
                "/admin/login",
                &[FORM, ("origin", ORIGIN)],
                &format!("key={ADMIN_KEY}"),
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
    let h = Harness::with_keys(AdminKeys::default(), |_| {})?;
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
