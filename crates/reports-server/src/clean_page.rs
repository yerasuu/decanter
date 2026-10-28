//! The public clean log of a report (decision #1060): built from the attachment at upload with the report crate's
//! allow-list, stored beside the report, re-validated by the same crate every time it is read and refused when it
//! does not pass. The raw attachment never reaches these pages.

use std::fmt::Write as _;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use decanter_report::{CleanLog, message_text, validate_clean_log};

use crate::app::AppState;
use crate::html::escape_html;
use crate::pages::{html_page, not_found_page};
use crate::store::{Product, Report, Store};

const TARGET: &str = "decanter_reports::clean_page";

pub enum Loaded {
    None,
    /// Stored but refused by the validator: shown as unavailable, never rendered.
    Invalid,
    Valid(CleanLog),
}

/// The report's clean log, validated.
///
/// # Errors
///
/// The bucket fails.
pub async fn load(store: &Store, report: &Report) -> anyhow::Result<Loaded> {
    let Some(bytes) = store.clean_log(report).await? else {
        return Ok(Loaded::None);
    };
    let Some(product) = Product::from_name(&report.product) else {
        return Ok(Loaded::Invalid);
    };
    Ok(match validate_clean_log(&bytes, product.schema()) {
        Ok(log) => Loaded::Valid(log),
        Err(rejections) => {
            tracing::error!(target: TARGET, count = rejections.len(), "stored clean log refused by the validator");
            Loaded::Invalid
        }
    })
}

fn counts(log: &CleanLog) -> String {
    let mut text = format!(
        "{} líneas examinadas: {} mostradas y {} omitidas; {} de nivel de error.",
        log.total,
        log.lines.len(),
        log.omitted,
        log.errors
    );
    if log.capped {
        text.push_str(" Se alcanzó el tope de lectura: el resto del log no se examinó.");
    }
    text
}

/// The paragraph `/r/<id>` shows about the log, with the link to it; empty when the report has none.
pub fn summary(id: &str, loaded: &Loaded) -> String {
    match loaded {
        Loaded::None => String::new(),
        Loaded::Invalid => "<h2>Log</h2><p>La versión pública del log no está disponible.</p>".to_owned(),
        Loaded::Valid(log) => format!(
            "<h2>Log</h2><p>{} <a href=\"/r/{}/log\">Ver la versión pública del log</a>.</p>",
            escape_html(&counts(log)),
            escape_html(id)
        ),
    }
}

fn render(id: &str, log: &CleanLog) -> String {
    let mut body = format!(
        "<p><a href=\"/r/{id}\">&larr; Volver al informe</a></p>\
         <p>Esta es la versión pública del log que se adjuntó al informe. Solo muestra las líneas que coinciden con \
         una plantilla conocida del vocabulario de informes, rehechas a partir de la plantilla y de sus valores \
         comprobados, nunca copiadas del log; <code>&hellip;</code> marca texto de la línea que no se publica. Las \
         demás líneas se omiten y solo se cuentan. El log original es privado.</p><p>{}</p>",
        escape_html(&counts(log)),
        id = escape_html(id),
    );
    if log.lines.is_empty() {
        body.push_str("<p>Ninguna línea del log coincide con una plantilla conocida.</p>");
        return body;
    }
    body.push_str("<table><thead><tr><th>Línea</th><th>Texto</th><th>Plantilla</th></tr></thead><tbody>");
    for line in &log.lines {
        let Some(text) = message_text(&line.message, log.product) else {
            continue;
        };
        let _ = write!(
            body,
            "<tr><td>{}</td><td><code>{}</code></td><td><code>{}</code></td></tr>",
            line.line,
            escape_html(&text),
            escape_html(&line.message.template)
        );
    }
    body.push_str("</tbody></table>");
    body
}

pub async fn log(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(store) = state.stores.of_id(&id) else {
        return not_found_page();
    };
    let report = match store.find_public(&id).await {
        Ok(Some(report)) => report,
        Ok(None) => return not_found_page(),
        Err(error) => return crate::app::internal("find_public", &error),
    };
    let title = format!("Log del informe {}", id.get(..8).unwrap_or_default());
    match load(store, &report).await {
        Ok(Loaded::None) => not_found_page(),
        Ok(Loaded::Invalid) => html_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            &title,
            "<p>La versión pública del log no está disponible.</p>",
        ),
        Ok(Loaded::Valid(log)) => html_page(StatusCode::OK, &title, &render(&id, &log)),
        Err(error) => crate::app::internal("clean_log", &error),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    use axum::body::{Body, to_bytes};
    use axum::extract::ConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::app::{AppState, router};
    use crate::config::Config;
    use crate::session::AdminKeys;
    use crate::store::{Product, test_stores};

    type Test = Result<(), Box<dyn Error>>;

    const ADMIN_KEY: &str = "0123456789abcdef0123456789abcdef-admin";
    const PSPORT: &str = r#"{"schema":2,"product":"psport","date":"2026-10-21","decanter":{"version":"0.3.2"},"psport":{"version":"0.1.0"},"system":{"os":"linux"},"game":{"title_id":"CUSA03173","name":"Bloodborne"},"failure":{"kind":"error","count":1}}"#;
    const DECANTER: &str = r#"{"schema":2,"product":"decanter","date":"2026-10-21","decanter":{"version":"0.3.2"},"system":{"os":"linux"},"action":"build","failure":{"kind":"error","count":1}}"#;

    /// Strings of the person and the machine seeded into the logs below; none may reach a public page.
    const PRIVATE: &[&str] = &[
        "yerasuu",
        "/home/deck",
        "C:\\Users",
        "John Smith",
        "DELL",
        "S2522HG",
        "8BitDo",
        "Wireless",
        "192.168",
        "fe80::",
        "3cb00b65",
        "7395438211234567890",
        "76561198012345678",
        "ghp_16C7",
        "eyJhbGci",
        "gitquarry",
        "DESKTOP-7H3K2LQ",
        "@gmail",
        "10:24:31",
        "3c:7c:3f",
        "Bloodborne\u{2122}",
    ];

    const PSPORT_LOG: &str = "\
Settings: /home/deck/data/Decanter/steamapps/psport/bbport/psport.ini
Display 1: Dell Inc. DELL S2522HG 1920x1080 (primary) <- the game's (PS_DISPLAY)
Pad 1: 8BitDo Ultimate 2 Wireless Controller
Window: Bloodborne\u{2122} (app id decanter.game.ps_CUSA03173)
Net: 192.168.1.37 fe80::1c2b:3cff:fe4d:5e6f host gitquarry DESKTOP-7H3K2LQ mac 3c:7c:3f:1a:2b:4d
NP: account 7395438211234567890 steam 76561198012345678 yerasuu@gmail.com ghp_16C7e42F292c6912E7710c838347Ae178B4a
[2026-10-28 10:24:31.640] eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl
GPU [Lib.Font] <Error> font.cpp:7236 sceFontWritingGetRenderMetrics: INVALID_WRITING
GPU [Lib.Pad] <Error> pad.cpp:120 scePadOpen: FAILED rc=0x80920001 code=3 /home/deck/x yerasuu DELL S2522HG
GPU [Lib.Pad] <Error> pad.cpp:120 yerasuu: (STUBBED) called
GPU [Lib.Pad] <Error> pad.cpp:120 DESKTOP: (STUBBED) called
GPU [Render.Vulkan] <Error> vk_platform.cpp:56 DebugUtilsCallback: VUID-vkCmdDraw-yerasuu-01234: x
GPU [Render.Vulkan] <Error> vk_platform.cpp:56 DebugUtilsCallback: DELL S2522HG
Host fault (signal 11) in C:\\Users\\John Smith\\yerasuu.exe+0x1234 (main), address 0x7ffd1234
Host fault (signal 11) in fe80::1c2b:3cff:fe4d:5e6f+0x1234 (main), address 0x0
STOP: called from 3cb00b65-386e-4b6f-8433-65edfeae0b30.prx+0x10; game code up the stack:
ERROR: cannot read /home/deck/data/Decanter/steamapps/psport/profiles/3cb00b65-386e-4b6f-8433-65edfeae0b30
";

    const DECANTER_LOG: &str = "\
2026-10-28T10:24:31.123456Z  INFO decanter: decanter started build=\"0.3.4 (143aa63)\" home=/home/deck user=yerasuu
2026-10-28T10:24:31.123456Z  WARN decanter::core: failed to open db err=database is locked path=/home/deck/x
2026-10-28T10:24:31.123456Z  WARN decanter::state::ps::jobs: job failed job=\"game\" target=\"yerasuu\" exit=\"code 139\" log=/home/deck/data/x.log
2026-10-28T10:24:31.123456Z  WARN decanter_gog::library::enrich: GOG order history unavailable account_id=\"7395438211234567890\" error=GOG session is not valid: yerasuu@gmail.com
2026-10-28T10:24:31.123456Z ERROR decanter::app: could not resolve library error=C:\\Users\\John Smith (os error 2)
2026-10-28T10:24:31.123456Z  WARN decanter::core: gitquarry DESKTOP-7H3K2LQ 192.168.1.37 DELL S2522HG
";

    struct Harness {
        state: AppState,
        root: PathBuf,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Harness {
        fn new() -> Result<Self, Box<dyn Error>> {
            let root = std::env::temp_dir().join(format!("decanter-reports-clean-{}", crate::secrets::random_hex()?));
            std::fs::create_dir_all(&root)?;
            Ok(Self {
                state: AppState::new(Config::default(), AdminKeys::single(ADMIN_KEY), test_stores(&root)),
                root,
            })
        }

        async fn send(
            &self,
            method: Method,
            uri: &str,
            headers: &[(&str, &str)],
            body: Vec<u8>,
        ) -> Result<(StatusCode, Vec<u8>), Box<dyn Error>> {
            let mut request = Request::builder().method(method).uri(uri);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            let mut request = request.body(Body::from(body))?;
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 4000))));
            let response = router(self.state.clone()).oneshot(request).await?;
            let status = response.status();
            Ok((status, to_bytes(response.into_body(), usize::MAX).await?.to_vec()))
        }

        async fn page(&self, uri: &str) -> Result<(StatusCode, String), Box<dyn Error>> {
            let (status, body) = self.send(Method::GET, uri, &[], vec![]).await?;
            Ok((status, String::from_utf8(body)?))
        }

        async fn admin(&self, method: Method, uri: &str) -> Result<(StatusCode, Vec<u8>), Box<dyn Error>> {
            let auth = format!("Bearer {ADMIN_KEY}");
            self.send(method, uri, &[("authorization", auth.as_str())], vec![])
                .await
        }

        async fn post(&self, report: &str) -> Result<Value, Box<dyn Error>> {
            let (status, body) = self
                .send(
                    Method::POST,
                    "/v1/reports",
                    &[("content-type", "application/json")],
                    report.as_bytes().to_vec(),
                )
                .await?;
            assert_eq!(status, StatusCode::CREATED);
            Ok(serde_json::from_slice(&body)?)
        }

        /// Posts `report`, attaches `log` with its code and returns the id.
        async fn report_with_log(&self, report: &str, log: &[u8]) -> Result<String, Box<dyn Error>> {
            let answer = self.post(report).await?;
            let id = answer["id"].as_str().ok_or("id")?.to_owned();
            let code = answer["delete_code"].as_str().ok_or("code")?;
            let (status, _) = self
                .send(
                    Method::PUT,
                    &format!("/v1/reports/{id}/attachment"),
                    &[("x-delete-code", code)],
                    log.to_vec(),
                )
                .await?;
            assert_eq!(status, StatusCode::NO_CONTENT);
            Ok(id)
        }

        async fn stored_clean_log(&self, id: &str) -> Result<String, Box<dyn Error>> {
            let store = self.state.stores.of_id(id).ok_or("partition")?;
            let report = store.find_public(id).await?.ok_or("report")?;
            Ok(String::from_utf8(store.clean_log(&report).await?.ok_or("clean log")?)?)
        }
    }

    fn assert_clean(what: &str, text: &str) {
        let lower = text.to_lowercase();
        for private in PRIVATE {
            assert!(!lower.contains(&private.to_lowercase()), "{what} holds {private:?}");
        }
    }

    #[tokio::test]
    async fn seeded_logs_publish_only_allow_listed_lines() -> Test {
        let h = Harness::new()?;
        for (report, log, kept) in [
            (
                PSPORT,
                PSPORT_LOG,
                "GPU [Lib.Pad] &lt;Error&gt; \u{2026}:\u{2026} scePadOpen: FAILED rc=0x80920001 code=3 \u{2026}",
            ),
            (DECANTER, DECANTER_LOG, "failed to open db: database is locked"),
        ] {
            let id = h.report_with_log(report, log.as_bytes()).await?;
            let (status, page) = h.page(&format!("/r/{id}/log")).await?;
            assert_eq!(status, StatusCode::OK);
            assert!(page.contains(kept), "{page}");
            assert!(page.contains("omitidas"));
            assert_clean("the clean log page", &page);
            let (status, summary) = h.page(&format!("/r/{id}")).await?;
            assert_eq!(status, StatusCode::OK);
            assert!(summary.contains(&format!("href=\"/r/{id}/log\"")));
            assert_clean("the report page", &summary);
            assert_clean("the stored clean log", &h.stored_clean_log(&id).await?);
            let path = format!("/admin/reports/{id}/attachment");
            let (status, _) = h.send(Method::GET, &path, &[], vec![]).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "the raw log stays private");
            let (status, raw) = h.admin(Method::GET, &path).await?;
            assert_eq!((status, raw.as_slice()), (StatusCode::OK, log.as_bytes()));
        }
        Ok(())
    }

    #[tokio::test]
    async fn hidden_report_hides_its_log_resolve_keeps_it_delete_removes_it() -> Test {
        let h = Harness::new()?;
        let id = h.report_with_log(PSPORT, PSPORT_LOG.as_bytes()).await?;
        let log = format!("/r/{id}/log");
        assert_eq!(h.state.stores.clean_blob_count().await?, 1);
        h.admin(Method::POST, &format!("/admin/reports/{id}/hide")).await?;
        assert_eq!(h.page(&log).await?.0, StatusCode::NOT_FOUND);
        h.admin(Method::POST, &format!("/admin/reports/{id}/restore")).await?;
        h.admin(Method::POST, &format!("/admin/reports/{id}/resolve")).await?;
        assert_eq!(h.page(&log).await?.0, StatusCode::OK, "resolve keeps the clean log");
        assert_eq!(h.state.stores.blob_count().await?, 0, "resolve drops the raw log");
        h.admin(Method::DELETE, &format!("/admin/reports/{id}")).await?;
        assert_eq!(h.page(&log).await?.0, StatusCode::NOT_FOUND);
        assert_eq!(h.state.stores.clean_blob_count().await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn report_without_a_log_has_no_log_page() -> Test {
        let h = Harness::new()?;
        let id = h.post(PSPORT).await?["id"].as_str().ok_or("id")?.to_owned();
        assert_eq!(h.page(&format!("/r/{id}/log")).await?.0, StatusCode::NOT_FOUND);
        assert!(!h.page(&format!("/r/{id}")).await?.1.contains("/log\""));
        let unknown = Product::Psport.new_id()?;
        assert_eq!(h.page(&format!("/r/{unknown}/log")).await?.0, StatusCode::NOT_FOUND);
        assert_eq!(h.page("/r/nothex/log").await?.0, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn binary_attachment_gives_an_empty_clean_log() -> Test {
        let h = Harness::new()?;
        let mut binary =
            b"GPU [Lib.Font] <Error> font.cpp:7236 sceFontWritingGetRenderMetrics: INVALID_WRITING\n".to_vec();
        binary.extend_from_slice(&[0, 159, 146, 150, 0, 1]);
        let id = h.report_with_log(PSPORT, &binary).await?;
        let (status, page) = h.page(&format!("/r/{id}/log")).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("0 líneas examinadas"));
        assert!(page.contains("Ninguna línea"));
        Ok(())
    }

    #[tokio::test]
    async fn clean_log_stops_at_the_line_cap() -> Test {
        let h = Harness::new()?;
        let line = "GPU [Lib.Font] <Error> font.cpp:7236 sceFontWritingGetRenderMetrics: INVALID_WRITING\n";
        let id = h.report_with_log(PSPORT, line.repeat(2100).as_bytes()).await?;
        let (status, page) = h.page(&format!("/r/{id}/log")).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("2000 líneas examinadas: 2000 mostradas"));
        assert!(page.contains("tope de lectura"));
        assert_eq!(page.matches("<tr><td>").count(), 2000);
        Ok(())
    }
}
