//! The audit trail through the whole router and the `user` subcommand: one record per change with the right
//! account, role and target, the page for admins only, a failing trail that never blocks a change, the retention
//! sweep, no secret anywhere, and the order of the pages.

use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use s3db::{Backend, Cas, DeleteOutcome};
use serde_json::Value;
use tower::ServiceExt;

use crate::accounts::{Accounts, TEST_ADMIN_KEY, TEST_VIEWER_KEY, test_accounts};
use crate::app::{AppState, router};
use crate::audit::PAGE_SIZE;
use crate::config::Config;
use crate::secrets::sha256;
use crate::store::{AccountStore, AuditRecord, NewReport, Product, Role, current_minute, test_stores};

type Test = Result<(), Box<dyn Error>>;
type Answer = (StatusCode, HeaderMap, Vec<u8>);

const ORIGIN: &str = "http://127.0.0.1:8787";
const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");
const PEER: [u8; 4] = [203, 0, 113, 7];

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

impl Login {
    fn headers(&self) -> [(&str, &str); 3] {
        [
            ("cookie", self.cookie.as_str()),
            ("origin", ORIGIN),
            ("x-csrf-token", self.csrf.as_str()),
        ]
    }
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
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
        let root = std::env::temp_dir().join(format!("decanter-reports-audit-{}", crate::secrets::random_hex()?));
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            state: AppState::new(config, accounts(&root), test_stores(&root)),
            root,
        })
    }

    async fn send(
        &self,
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
            .insert(ConnectInfo(SocketAddr::from((PEER, 4000))));
        let response = router(self.state.clone()).oneshot(request).await?;
        let (parts, body) = response.into_parts();
        Ok((parts.status, parts.headers, to_bytes(body, usize::MAX).await?.to_vec()))
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
        let cookie = header(&headers, "set-cookie")
            .split(';')
            .next()
            .unwrap_or_default()
            .to_owned();
        let (status, page_headers, _) = self.send(Method::GET, "/admin", &[("cookie", &cookie)], "").await?;
        assert_eq!(status, StatusCode::OK);
        Ok(Login {
            cookie,
            csrf: header(&page_headers, "x-csrf-token"),
        })
    }

    async fn insert(&self, product: Product, fingerprint: &str) -> Result<String, Box<dyn Error>> {
        let id = product.new_id()?;
        let new = NewReport {
            id: id.clone(),
            delete_hash: sha256(b"code"),
            fingerprint: fingerprint.to_owned(),
            product: product.name(),
            title_id: None,
            game_name: None,
            failure_kind: "t".to_owned(),
            template_id: "t".to_owned(),
            message: "t".to_owned(),
            created_day: "2026-10-27".to_owned(),
            received_day: "2026-10-28".to_owned(),
            report_json: "{}".to_owned(),
        };
        self.state.stores.get(product).insert(&new, 1000).await?;
        Ok(id)
    }

    async fn records(&self) -> Result<Vec<AuditRecord>, Box<dyn Error>> {
        Ok(self.state.accounts.store().audit_page(None, 10_000).await?.0)
    }

    async fn text(&self, uri: &str, headers: &[(&str, &str)]) -> Result<(StatusCode, String), Box<dyn Error>> {
        let (status, _, body) = self.send(Method::GET, uri, headers, "").await?;
        Ok((status, String::from_utf8(body)?))
    }
}

fn record(user: &str, target: &str) -> AuditRecord {
    AuditRecord {
        user: user.to_owned(),
        role: "admin".to_owned(),
        action: "hide".to_owned(),
        target: target.to_owned(),
        count: 1,
        at: String::new(),
    }
}

type Row = (String, String, String, String, u32);

/// `(user, role, action, target, count)` of every record, sorted, since records of one minute list in any order.
fn summary(records: &[AuditRecord]) -> Vec<Row> {
    let mut rows: Vec<Row> = records
        .iter()
        .map(|r| {
            (
                r.user.clone(),
                r.role.clone(),
                r.action.clone(),
                r.target.clone(),
                r.count,
            )
        })
        .collect();
    rows.sort();
    rows
}

fn row(user: &str, role: &str, action: &str, target: &str, count: u32) -> Row {
    (
        user.to_owned(),
        role.to_owned(),
        action.to_owned(),
        target.to_owned(),
        count,
    )
}

#[tokio::test]
async fn every_admin_change_writes_one_record_by_its_account() -> Test {
    let h = Harness::new(|_| {})?;
    let (fp_one, fp_group) = ("a".repeat(16), "b".repeat(16));
    let id = h.insert(Product::Decanter, &fp_one).await?;
    h.insert(Product::Decanter, &fp_group).await?;
    h.insert(Product::Psport, &fp_group).await?;

    let session = h.login_with(TEST_ADMIN_KEY).await?;
    for action in ["hide", "restore", "resolve", "delete"] {
        let (status, ..) = h
            .send(
                Method::POST,
                &format!("/admin/reports/{id}/{action}"),
                &session.headers(),
                "",
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{action}");
    }
    let bearer = format!("Bearer {TEST_ADMIN_KEY}");
    for action in ["hide", "restore", "resolve", "delete"] {
        let (status, ..) = h
            .send(
                Method::POST,
                &format!("/admin/groups/{fp_group}/{action}"),
                &[("authorization", &bearer)],
                "",
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{action}");
    }
    let missing = "0".repeat(32);
    let (status, ..) = h
        .send(
            Method::POST,
            &format!("/admin/reports/{missing}/hide"),
            &session.headers(),
            "",
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a miss changes nothing and records nothing"
    );

    let viewer = h.login_with(TEST_VIEWER_KEY).await?;
    let (status, ..) = h
        .send(
            Method::POST,
            &format!("/admin/groups/{fp_one}/hide"),
            &viewer.headers(),
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "a refused change records nothing");
    for login in [&session, &viewer] {
        let (status, ..) = h.send(Method::POST, "/admin/logout", &login.headers(), "").await?;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }
    let (status, ..) = h
        .send(Method::POST, "/admin/logout", &[("authorization", &bearer)], "")
        .await?;
    assert_eq!(status, StatusCode::SEE_OTHER, "a Bearer logout closes no session");

    let mut expected = vec![
        row("ana", "viewer", "login", "", 1),
        row("ana", "viewer", "logout", "", 1),
        row("owner", "admin", "login", "", 1),
        row("owner", "admin", "logout", "", 1),
    ];
    for action in ["hide", "restore", "resolve", "delete"] {
        expected.push(row("owner", "admin", action, &id, 1));
        expected.push(row("owner", "admin", &format!("{action}_group"), &fp_group, 2));
    }
    expected.sort();
    let records = h.records().await?;
    assert_eq!(summary(&records), expected);
    let minute = |at: &str| {
        at.len() == 17
            && at.ends_with('Z')
            && at.as_bytes().get(10) == Some(&b'T')
            && at.as_bytes().get(13) == Some(&b':')
    };
    assert!(
        records.iter().all(|r| minute(&r.at)),
        "UTC to the minute, nothing finer"
    );
    Ok(())
}

#[tokio::test]
async fn cli_changes_are_recorded_as_cli() -> Test {
    let dir = std::env::temp_dir().join(format!("decanter-reports-audit-cli-{}", crate::secrets::random_hex()?));
    let store = crate::store::test_accounts(&dir);
    let mut keys = Vec::new();
    for line in [
        "add owner --role admin",
        "add ana --role viewer",
        "list",
        "role ana --role admin",
        "reset ana",
        "remove ana",
        "remove nobody",
        "role owner --role viewer",
    ] {
        let words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        let command = crate::accounts::cli::parse(&words)?;
        let (mut out, mut notes) = (Vec::new(), Vec::new());
        let _ = crate::accounts::cli::run(&store, &command, &mut out, &mut notes).await;
        keys.extend(
            String::from_utf8(out)?
                .lines()
                .filter(|l| l.starts_with("drk_"))
                .map(str::to_owned),
        );
        assert!(
            String::from_utf8(notes)?.lines().all(|l| !l.starts_with("warning")),
            "{line}"
        );
    }
    let mut expected = vec![
        row("cli", "cli", "user_add_admin", "owner", 1),
        row("cli", "cli", "user_add_viewer", "ana", 1),
        row("cli", "cli", "user_role_admin", "ana", 1),
        row("cli", "cli", "user_reset", "ana", 1),
        row("cli", "cli", "user_remove", "ana", 1),
    ];
    expected.sort();
    let (records, _) = store.audit_page(None, 100).await?;
    assert_eq!(
        summary(&records),
        expected,
        "a refused change and `list` record nothing"
    );
    let text = serde_json::to_string(&serde_json::json!(records))?;
    assert_eq!(keys.len(), 3);
    for key in &keys {
        let hash = crate::secrets::hex(&sha256(key.as_bytes()));
        assert!(!text.contains(key.as_str()) && !text.contains(&hash));
    }
    drop(store);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[tokio::test]
async fn only_admins_read_the_audit() -> Test {
    let h = Harness::new(|_| {})?;
    let viewer = h.login_with(TEST_VIEWER_KEY).await?;
    let admin = h.login_with(TEST_ADMIN_KEY).await?;
    let viewer_bearer = format!("Bearer {TEST_VIEWER_KEY}");
    let admin_bearer = format!("Bearer {TEST_ADMIN_KEY}");
    let wrong = format!("Bearer drk_00000a01_{}", "9".repeat(64));
    for path in ["/admin/audit", "/admin/audit/records"] {
        for credentials in [
            vec![("cookie", viewer.cookie.as_str())],
            vec![("authorization", viewer_bearer.as_str())],
        ] {
            let (status, body) = h.text(path, &credentials).await?;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path} {credentials:?}");
            assert!(body.contains("\"role\""), "the 403 of every admin-only route: {body}");
        }
        for credentials in [vec![], vec![("authorization", wrong.as_str())]] {
            let (status, body) = h.text(path, &credentials).await?;
            assert_eq!(
                (status, body.as_str()),
                (StatusCode::NOT_FOUND, "{\"error\":\"not_found\"}"),
                "{path}"
            );
        }
        for credentials in [
            vec![("cookie", admin.cookie.as_str())],
            vec![("authorization", admin_bearer.as_str())],
        ] {
            assert_eq!(h.text(path, &credentials).await?.0, StatusCode::OK, "{path}");
        }
        let bad = format!("{path}?after=..%2Fusers");
        assert_eq!(
            h.text(&bad, &[("cookie", &admin.cookie)]).await?.0,
            StatusCode::BAD_REQUEST
        );
    }
    let link = "href=\"/admin/audit\"";
    assert!(h.text("/admin", &[("cookie", &admin.cookie)]).await?.1.contains(link));
    assert!(!h.text("/admin", &[("cookie", &viewer.cookie)]).await?.1.contains(link));
    let (_, page) = h.text("/admin/audit", &[("cookie", &admin.cookie)]).await?;
    for column in [
        "Registro de cambios",
        "Cuándo",
        "Quién",
        "Acción",
        "Objetivo",
        "iniciar sesión",
    ] {
        assert!(page.contains(column), "{column}");
    }
    Ok(())
}

/// A bucket that refuses everything, under an account map seeded without it.
#[derive(Debug)]
struct Broken;

#[async_trait::async_trait]
impl Backend for Broken {
    async fn get(&self, _: &str) -> anyhow::Result<(Vec<u8>, Option<String>)> {
        anyhow::bail!("bucket unavailable")
    }

    async fn put_cas(&self, _: &str, _: Vec<u8>, _: Option<&str>) -> anyhow::Result<Cas> {
        anyhow::bail!("bucket unavailable")
    }

    async fn delete(&self, _: &str) -> anyhow::Result<DeleteOutcome> {
        anyhow::bail!("bucket unavailable")
    }

    async fn delete_many(&self, _: &[String]) -> anyhow::Result<DeleteOutcome> {
        anyhow::bail!("bucket unavailable")
    }

    async fn list_prefix(&self, _: &str) -> anyhow::Result<Vec<(String, u64)>> {
        anyhow::bail!("bucket unavailable")
    }

    async fn get_range(&self, _: &str, _: u64, _: u64) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("bucket unavailable")
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn a_failing_audit_write_never_blocks_the_change() -> Test {
    let h = Harness::with_accounts(
        |_| {
            let store = AccountStore::with_cipher(Arc::new(Broken), Arc::new(s3db::Cipher::disabled()));
            let accounts = Accounts::new(store);
            accounts.seed(TEST_ADMIN_KEY, "owner", Role::Admin);
            accounts
        },
        |_| {},
    )?;
    let fp = "c".repeat(16);
    let id = h.insert(Product::Decanter, &fp).await?;
    let session = h.login_with(TEST_ADMIN_KEY).await?;
    let (status, ..) = h
        .send(
            Method::POST,
            &format!("/admin/reports/{id}/hide"),
            &session.headers(),
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let bearer = format!("Bearer {TEST_ADMIN_KEY}");
    let (status, ..) = h
        .send(
            Method::DELETE,
            &format!("/admin/groups/{fp}"),
            &[("authorization", &bearer)],
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(h.state.stores.count().await?, 0, "the change landed and stays");
    let (status, ..) = h.send(Method::POST, "/admin/logout", &session.headers(), "").await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (status, ..) = h
        .send(Method::GET, "/admin/audit", &[("authorization", &bearer)], "")
        .await?;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the page says it cannot read the trail"
    );
    Ok(())
}

#[tokio::test]
async fn retention_deletes_only_records_past_it() -> Test {
    const DAY: u64 = 24 * 60;
    let now = current_minute();
    for (days, kept) in [(180, vec!["fresh", "recent"]), (0, vec!["fresh", "old", "recent"])] {
        let h = Harness::new(|config| config.audit_retention_days = days)?;
        let store = h.state.accounts.store();
        store.append_audit(&record("owner", "old"), now - 181 * DAY).await?;
        store.append_audit(&record("owner", "recent"), now - 179 * DAY).await?;
        store.append_audit(&record("owner", "fresh"), now).await?;
        crate::sweep::run_once(&h.state).await;
        let mut targets: Vec<String> = h.records().await?.into_iter().map(|r| r.target).collect();
        targets.sort();
        assert_eq!(targets, kept, "audit_retention_days = {days}");
    }
    Ok(())
}

#[tokio::test]
async fn no_secret_in_any_record_or_on_the_page() -> Test {
    let h = Harness::new(|_| {})?;
    let id = h.insert(Product::Decanter, &"d".repeat(16)).await?;
    h.state
        .stores
        .get(Product::Decanter)
        .attach(&id, b"attachment secret content", b"", 10)
        .await?;
    let session = h.login_with(TEST_ADMIN_KEY).await?;
    h.send(
        Method::POST,
        &format!("/admin/reports/{id}/resolve"),
        &session.headers(),
        "",
    )
    .await?;
    let words: Vec<String> = ["add", "bo", "--role", "viewer"].map(str::to_owned).to_vec();
    let (mut out, mut notes) = (Vec::new(), Vec::new());
    crate::accounts::cli::run(
        h.state.accounts.store(),
        &crate::accounts::cli::parse(&words)?,
        &mut out,
        &mut notes,
    )
    .await?;
    let cli_key = String::from_utf8(out)?.trim_end().to_owned();
    let (_, page) = h.text("/admin/audit", &[("cookie", &session.cookie)]).await?;
    let (_, json) = h.text("/admin/audit/records", &[("cookie", &session.cookie)]).await?;
    h.send(Method::POST, "/admin/logout", &session.headers(), "").await?;
    let all = h.records().await?;
    assert_eq!(all.len(), 4, "login, resolve, user add, logout");
    let records = serde_json::to_string(&serde_json::json!(all))?;

    let cookie_id = session.cookie.split('=').nth(1).unwrap_or_default();
    let mut secrets = vec![
        TEST_ADMIN_KEY.to_owned(),
        TEST_VIEWER_KEY.to_owned(),
        cookie_id.to_owned(),
        session.csrf.clone(),
        crate::secrets::hex(&sha256(TEST_ADMIN_KEY.as_bytes())),
        cli_key.clone(),
        crate::secrets::hex(&sha256(cli_key.as_bytes())),
        "203.0.113.7".to_owned(),
        "attachment secret content".to_owned(),
    ];
    secrets.extend(
        h.state
            .accounts
            .store()
            .users()
            .await?
            .into_iter()
            .map(|user| user.key_hash),
    );
    for secret in &secrets {
        assert!(!secret.is_empty(), "every secret checked is real");
        for (what, text) in [("records", &records), ("page", &page), ("json", &json)] {
            assert!(!text.contains(secret.as_str()), "{secret} in the {what}");
        }
    }
    Ok(())
}

fn next_cursor(page: &str) -> Option<String> {
    let start = page.find("/admin/audit?after=")? + "/admin/audit?after=".len();
    page.get(start..)?.split('"').next().map(str::to_owned)
}

#[tokio::test]
async fn pages_run_newest_first_with_a_cursor_and_escape_every_value() -> Test {
    let h = Harness::new(|_| {})?;
    let store = h.state.accounts.store();
    let base = 29_000_000;
    let total = PAGE_SIZE * 2 + 5;
    for i in 0..total {
        store
            .append_audit(&record("owner", &format!("t{i:03}")), base + u64::try_from(i)?)
            .await?;
    }
    let hostile = AuditRecord {
        role: "<i>viewer</i>".to_owned(),
        action: "<svg onload=x>".to_owned(),
        ..record("<b>x</b>", "<script>alert(1)</script>")
    };
    store.append_audit(&hostile, base - 1).await?;
    let bearer = format!("Bearer {TEST_ADMIN_KEY}");
    let auth = [("authorization", bearer.as_str())];

    let mut seen = Vec::new();
    let mut uri = "/admin/audit".to_owned();
    let mut pages = 0;
    loop {
        let (status, page) = h.text(&uri, &auth).await?;
        assert_eq!(status, StatusCode::OK);
        pages += 1;
        for part in page.split("<code>t").skip(1) {
            seen.push(format!("t{}", part.get(..3).unwrap_or_default()));
        }
        let Some(cursor) = next_cursor(&page) else {
            assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !page.contains("<script>"));
            assert!(page.contains("&lt;b&gt;x&lt;/b&gt; (&lt;i&gt;viewer&lt;/i&gt;)"));
            assert!(page.contains("&lt;svg onload=x&gt;") && !page.contains("<svg"));
            break;
        };
        uri = format!("/admin/audit?after={cursor}");
    }
    let expected: Vec<String> = (0..total).rev().map(|i| format!("t{i:03}")).collect();
    assert_eq!(pages, 3);
    assert_eq!(seen, expected, "newest first, each once, across the pages");

    let mut json_targets = Vec::new();
    let mut uri = "/admin/audit/records".to_owned();
    loop {
        let (status, text) = h.text(&uri, &auth).await?;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_str(&text)?;
        let records = value["records"].as_array().cloned().unwrap_or_default();
        assert!(records.len() <= PAGE_SIZE);
        json_targets.extend(records.iter().filter_map(|r| r["target"].as_str().map(str::to_owned)));
        match value["next"].as_str() {
            Some(next) => uri = format!("/admin/audit/records?after={next}"),
            None => break,
        }
    }
    assert_eq!(json_targets.len(), total + 1);
    assert_eq!(json_targets.get(..total), Some(expected.as_slice()));
    Ok(())
}
