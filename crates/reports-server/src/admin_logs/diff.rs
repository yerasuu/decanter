//! Two runs side by side: `logdiff` over both logs, rendered as one table with the line numbers of both sides, a
//! text marker for each kind of row and equal runs folded to their ends.

use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::{NAV, TITLE, game_cell, html_page};
use crate::admin::Admin;
use crate::app::{AppState, internal, not_found};
use crate::html::escape_html;
use crate::logdiff::{CONTEXT, Diff, Fallback, MAX_EDITS, Row, Run, Side, diff as line_diff};
use crate::logtext::render_line;
use crate::store::LogRow;

/// Characters of one side of a row shown.
const CELL_CHARS: usize = 500;

#[derive(Deserialize)]
pub struct DiffParams {
    a: Option<String>,
    b: Option<String>,
    normalize: Option<String>,
}

fn form(a: &str, b: &str, normalized: bool) -> String {
    format!(
        "<form method=\"get\" action=\"/admin/logs/diff\">\
         <label>Log A<input name=\"a\" value=\"{}\" maxlength=\"33\" required></label>\
         <label>Log B<input name=\"b\" value=\"{}\" maxlength=\"33\" required></label>\
         <label><span><input type=\"checkbox\" name=\"normalize\" value=\"1\"{}> ignorar horas, direcciones y \
         números largos</span></label><button type=\"submit\">Comparar</button></form>",
        escape_html(a),
        escape_html(b),
        if normalized { " checked" } else { "" },
    )
}

/// What a page needs to link a line number to the viewer.
struct Link<'a> {
    id: &'a str,
    page_lines: usize,
}

impl Link<'_> {
    fn number(&self, number: usize) -> String {
        format!(
            "<a href=\"/admin/logs/{}?page={}#L{number}\">{number}</a>",
            self.id,
            (number - 1) / self.page_lines + 1
        )
    }
}

fn cell(text: &str) -> String {
    let mut out = String::new();
    if render_line(text, CELL_CHARS, &mut out) > 0 {
        out.push_str("<span class=\"cut\"> &hellip;</span>");
    }
    out
}

fn side_cells(side: Option<&Side<'_>>, link: &Link<'_>) -> String {
    side.map_or_else(
        || "<td class=\"n\"></td><td></td>".to_owned(),
        |side| {
            format!(
                "<td class=\"n\">{}</td><td>{}</td>",
                link.number(side.number),
                cell(side.text)
            )
        },
    )
}

fn row_html(
    class: &str,
    mark: &str,
    left: Option<&Side<'_>>,
    right: Option<&Side<'_>>,
    links: &[Link<'_>; 2],
) -> String {
    format!(
        "<tr class=\"{class}\">{}<td class=\"m\">{mark}</td>{}</tr>",
        side_cells(left, &links[0]),
        side_cells(right, &links[1])
    )
}

fn folded(count: usize, out: &mut String) {
    let _ = write!(
        out,
        "<tr class=\"fold\"><td colspan=\"5\">&hellip; {count} líneas iguales</td></tr>"
    );
}

/// An equal run shows `CONTEXT` lines at each end a change touches and folds the rest.
fn same_run(run: &Run<'_>, first: bool, last: bool, links: &[Link<'_>; 2], out: &mut String) {
    let span = CONTEXT.min(run.len);
    let mut shown = Vec::with_capacity(2 * CONTEXT);
    if !first {
        shown.extend(0..span);
    }
    if !last {
        shown.extend(run.len - span..run.len);
    }
    shown.sort_unstable();
    shown.dedup();
    let tail_start = run.len - run.tail.len();
    let mut next = 0;
    for offset in shown {
        let text = if offset < run.head.len() {
            run.head.get(offset)
        } else {
            offset.checked_sub(tail_start).and_then(|i| run.tail.get(i))
        };
        let Some(text) = text else { continue };
        if offset > next {
            folded(offset - next, out);
        }
        let side = |start: usize| Side {
            number: start + offset,
            text,
        };
        out.push_str(&row_html("", "=", Some(&side(run.a)), Some(&side(run.b)), links));
        next = offset + 1;
    }
    if run.len > next {
        folded(run.len - next, out);
    }
}

fn describe(row: &LogRow, lines: usize) -> String {
    let id = escape_html(&row.id);
    format!(
        "<a href=\"/admin/logs/{id}\"><code>{id}</code></a> {} &middot; {} &middot; {} &middot; {lines} líneas",
        game_cell(row),
        escape_html(&row.received_day),
        escape_html(&row.failure_kind),
    )
}

fn render(left: &LogRow, right: &LogRow, result: &Diff<'_>, normalized: bool, page_lines: usize) -> String {
    let mut body = String::from(NAV);
    body.push_str(&form(&left.id, &right.id, normalized));
    let summary = result.summary;
    let _ = write!(
        body,
        "<p>A: {}<br>B: {}</p><p>Iguales {} &middot; cambiadas {} &middot; quitadas de A {} &middot; añadidas en B {}{}</p>",
        describe(left, summary.lines_a),
        describe(right, summary.lines_b),
        summary.equal,
        summary.changed,
        summary.removed,
        summary.added,
        if normalized {
            " (horas, direcciones y números largos ignorados; las líneas iguales muestran el texto de A)"
        } else {
            ""
        },
    );
    if let Some(fallback) = result.fallback {
        let why = match fallback {
            Fallback::TooLong => "los dos logs juntos pasan de logs.diff_max_lines líneas".to_owned(),
            Fallback::TooDifferent => format!("se diferencian en más de {MAX_EDITS} líneas"),
        };
        let _ = write!(
            body,
            "<p class=\"cut\">Demasiado distintos para un diff ({why}): se comparan las líneas en la misma posición{}.</p>",
            if result.truncated {
                format!(
                    ", y se muestran las {} primeras diferentes",
                    crate::logdiff::FALLBACK_ROWS
                )
            } else {
                String::new()
            }
        );
    }
    let links = [
        Link {
            id: &left.id,
            page_lines,
        },
        Link {
            id: &right.id,
            page_lines,
        },
    ];
    if let [Row::Same(run)] = result.rows.as_slice() {
        let _ = write!(body, "<p>Los dos logs son iguales ({} líneas).</p>", run.len);
        return body;
    }
    body.push_str(
        "<table class=\"log diff\"><thead><tr><th colspan=\"2\">A</th><th></th><th colspan=\"2\">B</th></tr></thead><tbody>",
    );
    let last = result.rows.len().saturating_sub(1);
    for (index, row) in result.rows.iter().enumerate() {
        match row {
            Row::Same(run) => same_run(run, index == 0, index == last, &links, &mut body),
            Row::Removed(side) => body.push_str(&row_html("del", "&minus;", Some(side), None, &links)),
            Row::Added(side) => body.push_str(&row_html("add", "+", None, Some(side), &links)),
            Row::Changed(left_side, right_side) => {
                body.push_str(&row_html("chg", "~", Some(left_side), Some(right_side), &links));
            }
        }
    }
    body.push_str("</tbody></table>");
    body
}

pub async fn diff(_: Admin, State(state): State<AppState>, Query(params): Query<DiffParams>) -> Response {
    let normalized = params.normalize.is_some_and(|value| !value.is_empty());
    let (Some(a), Some(b)) = (
        params.a.filter(|id| !id.is_empty()),
        params.b.filter(|id| !id.is_empty()),
    ) else {
        let body = format!("{NAV}{}", form("", "", true));
        return html_page(StatusCode::OK, TITLE, &body);
    };
    let (left, right) = match tokio::try_join!(state.stores.log(&a), state.stores.log(&b)) {
        Ok((Some(left), Some(right))) => (left, right),
        Ok(_) => return not_found(),
        Err(error) => return internal("log_diff", &error),
    };
    let max_lines = state.config.logs.diff_max_lines as usize;
    let page_lines = state.config.logs.page_lines as usize;
    let rendered = tokio::task::spawn_blocking(move || {
        let ((left_row, left_bytes), (right_row, right_bytes)) = (left, right);
        let (left_text, right_text) = (
            String::from_utf8_lossy(&left_bytes),
            String::from_utf8_lossy(&right_bytes),
        );
        let result = line_diff(&left_text, &right_text, normalized, max_lines);
        render(&left_row, &right_row, &result, normalized, page_lines)
    })
    .await;
    match rendered {
        Ok(body) => html_page(StatusCode::OK, TITLE, &body),
        Err(error) => internal("log_diff", &error),
    }
}
