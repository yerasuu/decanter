//! The private log viewer through the whole router: the list and its filters, a log page with hostile content, the
//! caps on lines and width, the download, and the uniform 404. Kept apart from `admin_tests` so both suites grow
//! without touching each other.

use std::error::Error;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use tower::ServiceExt;

use crate::app::{AppState, router};
use crate::config::Config;
use crate::secrets::sha256;
use crate::store::{Change, NewReport, Product, test_stores};

type Test = Result<(), Box<dyn Error>>;
type Answer = (StatusCode, HeaderMap, Vec<u8>);

const ADMIN_KEY: &str = "0123456789abcdef0123456789abcdef-admin";
const ORIGIN: &str = "http://127.0.0.1:8787";
const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");

struct Harness {
    state: AppState,
    root: PathBuf,
    cookie: String,
    csrf: String,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What a test report differs in; the rest is fixed.
struct Spec<'a> {
    product: Product,
    fingerprint: &'a str,
    title: Option<&'a str>,
    kind: &'a str,
    day: &'a str,
}

const PSPORT: Spec<'static> = Spec {
    product: Product::Psport,
    fingerprint: "aaaaaaaaaaaaaaaa",
    title: Some("CUSA03173"),
    kind: "crash",
    day: "2026-10-28",
};

impl Harness {
    async fn new(tweak: impl FnOnce(&mut Config)) -> Result<Self, Box<dyn Error>> {
        let mut config = Config::default();
        config.limits.failures_per_ip = 1000;
        tweak(&mut config);
        let root = std::env::temp_dir().join(format!("decanter-reports-logs-{}", crate::secrets::random_hex()?));
        std::fs::create_dir_all(&root)?;
        let mut harness = Self {
            state: AppState::new(config, ADMIN_KEY, test_stores(&root)),
            root,
            cookie: String::new(),
            csrf: String::new(),
        };
        let (status, headers, _) = harness
            .send(
                Method::POST,
                "/admin/login",
                &[FORM, ("origin", ORIGIN)],
                &format!("key={ADMIN_KEY}"),
            )
            .await?;
        assert_eq!(status, StatusCode::SEE_OTHER);
        harness.cookie = header(&headers, "set-cookie")
            .split(';')
            .next()
            .unwrap_or_default()
            .to_owned();
        let (_, headers, _) = harness.get("/admin/logs").await?;
        harness.csrf = header(&headers, "x-csrf-token");
        Ok(harness)
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
            .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))));
        let response = router(self.state.clone()).oneshot(request).await?;
        let (parts, body) = response.into_parts();
        Ok((parts.status, parts.headers, to_bytes(body, usize::MAX).await?.to_vec()))
    }

    /// A GET with the session cookie.
    async fn get(&self, uri: &str) -> Result<Answer, Box<dyn Error>> {
        self.send(Method::GET, uri, &[("cookie", &self.cookie)], "").await
    }

    async fn page(&self, uri: &str) -> Result<String, Box<dyn Error>> {
        let (status, _, body) = self.get(uri).await?;
        let page = String::from_utf8(body)?;
        assert_eq!(status, StatusCode::OK, "{uri}: {page}");
        Ok(page)
    }

    async fn insert(&self, spec: &Spec<'_>, log: Option<&[u8]>) -> Result<String, Box<dyn Error>> {
        let id = spec.product.new_id()?;
        let new = NewReport {
            id: id.clone(),
            delete_hash: sha256(b"code"),
            fingerprint: spec.fingerprint.to_owned(),
            product: spec.product.name(),
            title_id: spec.title.map(str::to_owned),
            game_name: spec.title.map(|_| "Bloodborne".to_owned()),
            failure_kind: spec.kind.to_owned(),
            template_id: "psport.fault".to_owned(),
            message: "m".to_owned(),
            created_day: spec.day.to_owned(),
            received_day: spec.day.to_owned(),
            report_json: "{}".to_owned(),
        };
        let store = self.state.stores.get(spec.product);
        store.insert(&new, 100_000).await?;
        if let Some(log) = log {
            store.attach(&id, log, b"", 100_000).await?;
        }
        Ok(id)
    }
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn rows(page: &str) -> usize {
    page.matches("<tr><td><a href=\"/admin/logs/").count()
}

#[tokio::test]
async fn list_shows_visible_attached_reports_with_filters_and_pages() -> Test {
    let h = Harness::new(|_| {}).await?;
    let newest = h.insert(&PSPORT, Some(b"psport log")).await?;
    let decanter = h
        .insert(
            &Spec {
                product: Product::Decanter,
                title: None,
                kind: "error",
                day: "2026-10-27",
                fingerprint: "bbbbbbbbbbbbbbbb",
            },
            Some(b"decanter log"),
        )
        .await?;
    let other_title = h
        .insert(
            &Spec {
                title: Some("CUSA00001"),
                kind: "stop",
                day: "2026-10-26",
                ..PSPORT
            },
            Some(b"x"),
        )
        .await?;
    let unattached = h.insert(&PSPORT, None).await?;
    let hidden = h.insert(&PSPORT, Some(b"hidden")).await?;
    h.state
        .stores
        .get(Product::Psport)
        .change(std::slice::from_ref(&hidden), &Change::Hide)
        .await?;

    let all = h.page("/admin/logs").await?;
    assert_eq!(rows(&all), 3, "{all}");
    assert!(all.find(&newest) < all.find(&decanter) && all.find(&decanter) < all.find(&other_title));
    assert!(!all.contains(&unattached) && !all.contains(&hidden));
    assert!(all.contains("10 B") && all.contains("Bloodborne") && all.contains("CUSA03173"));
    assert!(all.contains(&format!("/admin/reports/{newest}/attachment")) && all.contains(&format!("/r/{newest}")));
    for (query, expected) in [
        ("?product=decanter", 1),
        ("?title=CUSA03173", 1),
        ("?title=CUSA00001&kind=stop", 1),
        ("?kind=crash", 1),
        ("?day=2026-10-27", 1),
        ("?fingerprint=aaaaaaaaaaaaaaaa", 2),
        ("?product=psport&title=&day=&kind=&fingerprint=", 2),
        ("?day=2001-01-01", 0),
    ] {
        assert_eq!(
            rows(&h.page(&format!("/admin/logs{query}")).await?),
            expected,
            "{query}"
        );
    }
    for bad in [
        "kind=Crash",
        "kind=%3Cb%3E",
        "title=cusa1",
        "day=today",
        "fingerprint=xyz",
        "product=other",
        "page=0",
        "page=x",
    ] {
        let (status, ..) = h.get(&format!("/admin/logs?{bad}")).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    Ok(())
}

#[tokio::test]
async fn list_pages_by_fifty() -> Test {
    let h = Harness::new(|_| {}).await?;
    for _ in 0..51 {
        h.insert(&PSPORT, Some(b"log")).await?;
    }
    let first = h.page("/admin/logs?title=CUSA03173").await?;
    assert_eq!(rows(&first), 50);
    assert!(
        first.contains("href=\"/admin/logs?title=CUSA03173&amp;page=2\""),
        "{first}"
    );
    let second = h.page("/admin/logs?title=CUSA03173&page=2").await?;
    assert_eq!(rows(&second), 1);
    assert!(!second.contains("Siguiente"));
    Ok(())
}

#[tokio::test]
async fn viewer_escapes_hostile_content_and_highlights_the_classes() -> Test {
    let h = Harness::new(|_| {}).await?;
    let log = "boot\n<script>alert(1)</script><img src=x onerror=alert(2)>\n\
               \u{1b}[31mGPU [Render.Vulkan] <Error> red\u{1b}[0m\n\
               Runtime: Guest fault at +0x1234\nHost fault in thread 3\nmprotect failed\nVirtualProtect failed: 87\n\
               Calls: summary stub 1 sceNpTrophyRegisterContext\nimport sceNetInit is a stub\n\
               GPU <Warning> missing extension\nNUL\0here and \u{202e}reversed\u{2066}\nplain\r\n";
    let id = h.insert(&PSPORT, Some(log.as_bytes())).await?;
    let (status, headers, body) = h.get(&format!("/admin/logs/{id}")).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert!(header(&headers, "content-security-policy").starts_with("default-src 'none'"));
    let page = String::from_utf8(body)?;
    assert!(!page.contains("<script>") && !page.contains("<img"), "{page}");
    assert!(!page.contains('\u{1b}') && !page.contains('\0') && !page.contains('\u{202e}'));
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(page.contains("&lt;U+202E&gt;") && page.contains("\u{2400}") && page.contains("\u{241b}"));
    for (line, class, label) in [
        (3, "error", "ERROR"),
        (4, "fault", "FALLO"),
        (5, "fault", "FALLO"),
        (6, "fault", "FALLO"),
        (7, "fault", "FALLO"),
        (8, "calls", "LLAMADAS"),
        (9, "failed", "FALLIDA"),
        (10, "warn", "AVISO"),
    ] {
        let row = format!(
            "<tr id=\"L{line}\" class=\"{class}\"><td class=\"n\"><a href=\"#L{line}\">{line}</a></td><td class=\"m\">{label}</td>"
        );
        assert!(page.contains(&row), "{row}");
        assert!(
            page.contains(&format!("?page=1#L{line}\">{line}</a> [{label}]")),
            "jump to {line}"
        );
    }
    assert!(page.contains("<tr id=\"L1\" class=\"\">"));
    assert!(page.contains("12 líneas"), "{page}");
    assert!(page.contains("FALLO 4; LLAMADAS 1; ERROR 1; FALLIDA 1; AVISO 1;"));
    assert!(page.contains(&format!("href=\"/admin/reports/{id}/attachment\"")));
    Ok(())
}

#[tokio::test]
async fn long_lines_are_cut_and_long_logs_are_paged() -> Test {
    let h = Harness::new(|config| config.logs.page_lines = 100).await?;
    let mut log = format!("{}\n", "x".repeat(3000));
    for n in 2..=250 {
        let _ = writeln!(log, "line {n}");
    }
    let id = h.insert(&PSPORT, Some(log.as_bytes())).await?;
    let first = h.page(&format!("/admin/logs/{id}")).await?;
    assert!(first.contains("línea cortada: 1000 bytes más"), "{first}");
    assert!(!first.contains(&"x".repeat(2001)));
    assert!(first.contains("250 líneas en 3 páginas de 100"));
    assert!(first.contains(">line 100<") && !first.contains(">line 101<"));
    assert!(first.contains(&format!("/admin/logs/{id}?page=2\">Siguiente")));
    let third = h.page(&format!("/admin/logs/{id}?page=3")).await?;
    assert!(third.contains("id=\"L201\"") && third.contains(">line 250<") && !third.contains("id=\"L200\""));
    let (past, ..) = h.get(&format!("/admin/logs/{id}?page=4")).await?;
    assert_eq!(past, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn binary_attachment_shows_a_size_note() -> Test {
    let h = Harness::new(|_| {}).await?;
    let id = h.insert(&PSPORT, Some(&[0_u8; 3000])).await?;
    let page = h.page(&format!("/admin/logs/{id}")).await?;
    assert!(page.contains("parece binario (2.9 KiB)"), "{page}");
    assert!(!page.contains("class=\"log\""));
    Ok(())
}

fn render_time(log: &[u8]) -> Duration {
    let row = crate::store::LogRow::for_test("p0", "psport");
    let started = Instant::now();
    let page = crate::admin_logs::render_log(&row, log, 1, 2000);
    let elapsed = started.elapsed();
    assert!(
        page.is_some_and(|page| page.len() < 2 * 1024 * 1024),
        "a page stays small"
    );
    elapsed
}

/// A 5 MiB line and 5 MiB of one-character lines render in bounded time, and a log four times larger costs about
/// four times more, not sixteen.
#[tokio::test]
async fn hostile_sizes_render_in_linear_time() -> Test {
    let mib = 1024 * 1024;
    for shape in ["one line", "short lines"] {
        let make = |size: usize| match shape {
            "one line" => "<".repeat(size).into_bytes(),
            _ => "E\n".repeat(size / 2).into_bytes(),
        };
        let (small, large) = (make(mib + mib / 4), make(5 * mib));
        render_time(&small);
        let t_small = render_time(&small).max(Duration::from_millis(5));
        let t_large = render_time(&large);
        assert!(t_large < Duration::from_secs(10), "{shape}: {t_large:?}");
        assert!(t_large < t_small * 12, "{shape}: {t_small:?} then {t_large:?}");
    }
    let h = Harness::new(|_| {}).await?;
    let id = h.insert(&PSPORT, Some("a\n".repeat(5 * mib / 2).as_bytes())).await?;
    let started = Instant::now();
    let page = h.page(&format!("/admin/logs/{id}?page=1311")).await?;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(page.contains("id=\"L2621440\""), "the last page holds the last line");
    Ok(())
}

#[tokio::test]
async fn download_is_a_forced_unsniffed_unstored_attachment() -> Test {
    let h = Harness::new(|_| {}).await?;
    let id = h.insert(&PSPORT, Some(b"<html>raw</html>")).await?;
    let (status, headers, body) = h.get(&format!("/admin/reports/{id}/attachment")).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"<html>raw</html>");
    assert_eq!(header(&headers, "content-type"), "application/octet-stream");
    assert!(header(&headers, "content-disposition").starts_with("attachment;"));
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    Ok(())
}

#[tokio::test]
async fn unknown_hidden_and_unattached_ids_are_the_uniform_404() -> Test {
    let h = Harness::new(|_| {}).await?;
    let unattached = h.insert(&PSPORT, None).await?;
    let hidden = h.insert(&PSPORT, Some(b"log")).await?;
    h.state
        .stores
        .get(Product::Psport)
        .change(std::slice::from_ref(&hidden), &Change::Hide)
        .await?;
    let shown = h.insert(&PSPORT, Some(b"log")).await?;
    let (baseline_status, _, baseline) = h.get(&format!("/admin/logs/p{}", "0".repeat(32))).await?;
    assert_eq!(baseline_status, StatusCode::NOT_FOUND);
    for id in [
        unattached,
        hidden,
        shown.replacen('p', "d", 1),
        "nope".to_owned(),
        format!("p{}", "A".repeat(32)),
    ] {
        let (status, _, body) = h.get(&format!("/admin/logs/{id}")).await?;
        assert_eq!((status, &body), (StatusCode::NOT_FOUND, &baseline), "{id}");
    }
    Ok(())
}

#[tokio::test]
async fn every_log_route_is_404_without_a_session() -> Test {
    let h = Harness::new(|_| {}).await?;
    let id = h.insert(&PSPORT, Some(b"log")).await?;
    let baseline = h.send(Method::GET, "/admin/reports", &[], "").await?;
    assert_eq!(baseline.0, StatusCode::NOT_FOUND);
    let stale = format!("__Host-session={}", "a".repeat(64));
    for headers in [
        &[][..],
        &[("cookie", stale.as_str())][..],
        &[("authorization", "Bearer nope")][..],
    ] {
        for path in [
            "/admin/logs".to_owned(),
            "/admin/logs?title=CUSA03173".to_owned(),
            format!("/admin/logs/{id}"),
            format!("/admin/logs/{id}?page=2"),
            "/admin/logs/search?q=x&title=CUSA03173".to_owned(),
            format!("/admin/logs/diff?a={id}&b={id}"),
        ] {
            let answer = h.send(Method::GET, &path, headers, "").await?;
            assert_eq!(answer, baseline, "{path} {headers:?}");
        }
    }
    let auth = format!("Bearer {ADMIN_KEY}");
    let (status, headers, _) = h
        .send(
            Method::GET,
            &format!("/admin/logs/{id}"),
            &[("authorization", &auth)],
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "the key opens the viewer too");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    let admin = h.page("/admin").await?;
    assert!(admin.contains("href=\"/admin/logs\""));
    Ok(())
}

fn run_log(started: &str, extra: &str) -> String {
    format!(
        "# started {started}\nboot\nLinked modules: libc.prx@0x56e0000\nframe 1\nframe 2\nframe 3\nframe 4\nframe 5\n\
         frame 6\nframe 7\n{extra}Guest fault at +0x1234 <img src=x>\n# ended {started}\n# exit 139\n"
    )
}

#[tokio::test]
async fn diff_shows_changed_added_and_removed_lines_with_both_numbers() -> Test {
    let h = Harness::new(|_| {}).await?;
    let a = h
        .insert(&PSPORT, Some(run_log("2026-10-27T10:00:00Z", "only in a\n").as_bytes()))
        .await?;
    let b_log = run_log("2026-10-28T11:30:00Z", "").replace("frame 4", "frame four") + "trailer\n";
    let b = h.insert(&PSPORT, Some(b_log.as_bytes())).await?;
    let raw = h.page(&format!("/admin/logs/diff?a={a}&b={b}")).await?;
    assert!(!raw.contains("<img") && raw.contains("&lt;img src=x&gt;"));
    assert!(
        raw.contains("Iguales 10 &middot; cambiadas 3 &middot; quitadas de A 1 &middot; añadidas en B 1"),
        "{raw}"
    );
    let changed = format!(
        "<tr class=\"chg\"><td class=\"n\"><a href=\"/admin/logs/{a}?page=1#L7\">7</a></td><td>frame 4</td>\
         <td class=\"m\">~</td><td class=\"n\"><a href=\"/admin/logs/{b}?page=1#L7\">7</a></td><td>frame four</td></tr>"
    );
    assert!(raw.contains(&changed), "{changed}");
    assert!(
        raw.contains("<tr class=\"del\"><td class=\"n\"><a href=\"/admin/logs/") && raw.contains(">only in a</td>")
    );
    assert!(raw.contains("<td class=\"m\">+</td>") && raw.contains(">trailer</td>"));
    assert!(raw.contains("<td class=\"m\">&minus;</td>"));
    assert!(
        !raw.contains("líneas iguales"),
        "runs of up to twice the context are shown whole"
    );

    let normalized = h.page(&format!("/admin/logs/diff?a={a}&b={b}&normalize=1")).await?;
    assert!(normalized.contains("Iguales 12 &middot; cambiadas 1 &middot; quitadas de A 1 &middot; añadidas en B 1"));
    assert!(normalized.contains("ignorados") && normalized.contains("name=\"normalize\" value=\"1\" checked"));
    assert!(
        normalized.contains("&hellip; 3 líneas iguales</td></tr><tr class=\"\"><td class=\"n\"><a href=\"/admin/logs/"),
        "the leading run shows only the context before the first change: {normalized}"
    );

    let same = h.page(&format!("/admin/logs/diff?a={a}&b={a}")).await?;
    assert!(same.contains("Los dos logs son iguales (14 líneas)") && !same.contains("class=\"chg\""));
    Ok(())
}

#[tokio::test]
async fn diff_falls_back_when_too_long_and_needs_two_readable_logs() -> Test {
    let h = Harness::new(|config| config.logs.diff_max_lines = 100).await?;
    let mut long_a = String::new();
    for n in 0..80 {
        let _ = writeln!(long_a, "line {n}");
    }
    let long_b = long_a.replace("line 40\n", "line forty\n");
    let a = h.insert(&PSPORT, Some(long_a.as_bytes())).await?;
    let b = h.insert(&PSPORT, Some(long_b.as_bytes())).await?;
    let page = h.page(&format!("/admin/logs/diff?a={a}&b={b}")).await?;
    assert!(
        page.contains("Demasiado distintos para un diff (los dos logs juntos pasan"),
        "{page}"
    );
    assert!(page.contains("cambiadas 1") && page.contains(">line forty</td>"));

    let form = h.page("/admin/logs/diff").await?;
    assert!(form.contains("name=\"a\"") && form.contains("checked"));
    let hidden = h.insert(&PSPORT, Some(b"log")).await?;
    h.state
        .stores
        .get(Product::Psport)
        .change(std::slice::from_ref(&hidden), &Change::Hide)
        .await?;
    let unattached = h.insert(&PSPORT, None).await?;
    let (_, _, baseline) = h.get(&format!("/admin/logs/p{}", "0".repeat(32))).await?;
    for other in [hidden, unattached, "x".to_owned(), format!("d{}", "0".repeat(32))] {
        for uri in [
            format!("/admin/logs/diff?a={a}&b={other}"),
            format!("/admin/logs/diff?a={other}&b={a}"),
        ] {
            let (status, _, body) = h.get(&uri).await?;
            assert_eq!((status, &body), (StatusCode::NOT_FOUND, &baseline), "{uri}");
        }
    }
    Ok(())
}

fn fault_log(faults: usize, padding: usize) -> String {
    let mut log = String::from("boot\nmounting\n");
    for n in 0..faults {
        let _ = writeln!(
            log,
            "Runtime: Guest fault #{n} at +0x1234 <b>x</b>\nafter {n}\nquiet\nquiet\nquiet"
        );
    }
    log.push_str(&"p".repeat(padding));
    log
}

#[tokio::test]
async fn search_needs_a_scope_and_a_short_query() -> Test {
    let h = Harness::new(|_| {}).await?;
    h.insert(&PSPORT, Some(fault_log(1, 0).as_bytes())).await?;
    let form = h.page("/admin/logs/search").await?;
    assert!(form.contains("name=\"q\"") && !form.contains("Se buscó"));
    let (status, _, body) = h.get("/admin/logs/search?q=Guest+fault").await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8(body)?.contains("una búsqueda en todos los logs del bucket"));
    let (status, ..) = h.get("/admin/logs/search?q=Guest&product=psport&kind=crash").await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "product and kind are no scope");
    for bad in [
        format!("q={}&title=CUSA03173", "a".repeat(201)),
        "q=a%01b&title=CUSA03173".to_owned(),
        "q=x&title=bad".to_owned(),
    ] {
        let (status, ..) = h.get(&format!("/admin/logs/search?{bad}")).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let page = h
        .page(&format!("/admin/logs/search?q={}&title=CUSA03173", "a".repeat(200)))
        .await?;
    assert!(page.contains("0 coincidencias"));
    Ok(())
}

#[tokio::test]
async fn search_shows_matches_with_context_and_links_to_the_line() -> Test {
    let h = Harness::new(|config| config.logs.page_lines = 100).await?;
    let older = h.insert(&PSPORT, Some(fault_log(1, 0).as_bytes())).await?;
    let newer = h.insert(&PSPORT, Some(fault_log(30, 0).as_bytes())).await?;
    let elsewhere = h
        .insert(
            &Spec {
                title: Some("CUSA00001"),
                ..PSPORT
            },
            Some(fault_log(1, 0).as_bytes()),
        )
        .await?;
    let page = h.page("/admin/logs/search?q=guest+FAULT&title=CUSA03173").await?;
    assert!(page.contains("en 2 de 2 logs"), "{page}");
    assert!(page.contains("31 coincidencias en 2 logs"));
    assert!(page.find(&newer) < page.find(&older) && !page.contains(&elsewhere));
    assert!(!page.contains("<b>x</b>") && page.contains("&lt;b&gt;x&lt;/b&gt;"));
    assert!(page.contains(&format!(
        "/admin/logs/{older}?page=1#L3\">3</a></td><td class=\"m\">&raquo;"
    )));
    assert!(page.contains(&format!(
        "/admin/logs/{older}?page=1#L2\">2</a></td><td class=\"m\"></td><td>mounting"
    )));
    assert!(
        page.contains(&format!("/admin/logs/{newer}?page=2#L148\">148</a>")),
        "line 148 is on page 2"
    );
    assert!(!page.contains("La búsqueda paró"));
    let by_group = h
        .page("/admin/logs/search?q=guest&fingerprint=aaaaaaaaaaaaaaaa&day=2026-10-28")
        .await?;
    assert!(by_group.contains("en 3 de 3 logs"), "a fingerprint spans titles");
    Ok(())
}

#[tokio::test]
async fn search_caps_are_applied_and_said() -> Test {
    let h = Harness::new(|config| {
        config.logs.search_max_logs = 2;
        config.logs.search_max_mib = 1;
        config.logs.search_max_matches = 5;
    })
    .await?;
    for _ in 0..3 {
        h.insert(&PSPORT, Some(fault_log(1, 0).as_bytes())).await?;
    }
    let logs = h.page("/admin/logs/search?q=guest&title=CUSA03173").await?;
    assert!(
        logs.contains("paró en el tope de 2 logs por búsqueda: buscados 2 de 3 logs"),
        "{logs}"
    );

    let big = Spec {
        title: Some("CUSA00002"),
        ..PSPORT
    };
    for _ in 0..2 {
        h.insert(&big, Some(fault_log(1, 700 * 1024).as_bytes())).await?;
    }
    let bytes = h.page("/admin/logs/search?q=guest&title=CUSA00002").await?;
    assert!(
        bytes.contains("tope de 1 MiB leídos por búsqueda: buscados 1 de 2 logs"),
        "{bytes}"
    );

    let many = Spec {
        title: Some("CUSA00003"),
        ..PSPORT
    };
    h.insert(&many, Some(fault_log(9, 0).as_bytes())).await?;
    let matches = h.page("/admin/logs/search?q=guest&title=CUSA00003").await?;
    assert!(matches.contains("5 coincidencias en 1 logs") && matches.contains("tope de 5 coincidencias"));
    assert!(matches.contains("Guest fault #4") && !matches.contains("Guest fault #5"));
    Ok(())
}

#[tokio::test]
async fn one_search_at_a_time_per_session() -> Test {
    let h = Harness::new(|_| {}).await?;
    h.insert(&PSPORT, Some(fault_log(1, 0).as_bytes())).await?;
    let uri = "/admin/logs/search?q=guest&title=CUSA03173";
    let running = h.state.log_searches.try_begin(sha256(h.csrf.as_bytes()));
    assert!(running.is_some());
    let (status, headers, _) = h.get(uri).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&headers, "retry-after"), "5");
    let auth = format!("Bearer {ADMIN_KEY}");
    let (other, ..) = h.send(Method::GET, uri, &[("authorization", &auth)], "").await?;
    assert_eq!(other, StatusCode::OK, "another maintainer is not held up");
    drop(running);
    assert!(h.page(uri).await?.contains("1 coincidencias"));
    assert!(
        h.state.log_searches.try_begin(sha256(h.csrf.as_bytes())).is_some(),
        "a finished search frees its slot"
    );
    Ok(())
}
