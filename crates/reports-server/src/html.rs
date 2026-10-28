//! The only way text reaches a page: through [`escape_html`]. A report is untrusted input even after it
//! passes validation, so nothing dynamic is written to a page without it.

use std::fmt::Write as _;

use serde_json::Value;

pub const TITLE: &str = "Informes de errores de decanter";

const STYLE: &str = "body{font:15px/1.5 system-ui,sans-serif;margin:0 auto;max-width:70rem;padding:1rem 16px;color:#1c1c1e;background:#fff}\
a{color:#0a58ca}h1{font-size:1.4rem}h2{font-size:1.1rem}table{border-collapse:collapse;width:100%;margin:1rem 0}\
th,td{border-bottom:1px solid #d0d0d5;padding:.35rem .5rem;text-align:left;vertical-align:top;overflow-wrap:anywhere}\
th{background:#f2f2f7}pre{background:#f2f2f7;padding:.75rem;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere}\
code{font-family:ui-monospace,monospace}.mark{background:#d1f0d9;padding:0 .4rem;border-radius:3px}\
form{display:flex;flex-wrap:wrap;gap:.5rem;align-items:end}label{display:flex;flex-direction:column;font-size:.85rem}\
nav a{margin-right:1rem}footer{margin-top:2rem;font-size:.85rem;color:#555}\
@media(prefers-color-scheme:dark){body{background:#1c1c1e;color:#f2f2f7}a{color:#6ea8fe}th,pre{background:#2c2c2e}\
th,td{border-color:#3a3a3c}.mark{background:#1e5631}footer{color:#aaa}}";

#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `body` is markup already built from escaped parts.
#[must_use]
pub fn layout(page_title: &str, body: &str) -> String {
    let title = escape_html(page_title);
    format!(
        "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\"><title>{title}</title><style>{STYLE}</style></head><body>\
         <nav><a href=\"/\">{}</a></nav><h1>{title}</h1>{body}\
         <footer>Los informes se publican solos al superar la validación del esquema cerrado; \
         no incluyen rutas, nombres ni direcciones.</footer></body></html>",
        escape_html(TITLE)
    )
}

/// A scalar as the text it shows; nothing in a report is rendered as markup.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

fn flatten(prefix: &str, value: &Value, rows: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, inner, rows);
            }
        }
        Value::Array(items) => {
            for (index, inner) in items.iter().enumerate() {
                flatten(&format!("{prefix}[{index}]"), inner, rows);
            }
        }
        other => rows.push((prefix.to_owned(), scalar(other))),
    }
}

/// Every field of the report as a two-column table of escaped text.
#[must_use]
pub fn fields_table(report: &Value) -> String {
    let mut rows = Vec::new();
    flatten("", report, &mut rows);
    let mut out = String::from("<table><thead><tr><th>Campo</th><th>Valor</th></tr></thead><tbody>");
    for (path, value) in rows {
        let _ = write!(
            out,
            "<tr><td><code>{}</code></td><td>{}</td></tr>",
            escape_html(&path),
            escape_html(&value)
        );
    }
    out.push_str("</tbody></table>");
    out
}

fn slots_text(slots: Option<&Value>) -> String {
    let parts: Vec<String> = slots
        .and_then(Value::as_array)
        .map(|slots| {
            slots
                .iter()
                .map(|slot| format!("{}={}", scalar(&slot["type"]), scalar(&slot["value"])))
                .collect()
        })
        .unwrap_or_default();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// The message template and its slots as plain text: `psport.gpu.failed_rc (class=Lib.Font, ...)`, plus the
/// cause template when there is one. Escaping is the caller's.
#[must_use]
pub fn message_text(report: &Value) -> String {
    let message = &report["failure"]["message"];
    let Some(template) = message["template"].as_str() else {
        return "sin plantilla (solo el tipo)".to_owned();
    };
    let mut text = format!("{template}{}", slots_text(message.get("slots")));
    if let Some(cause) = message["cause"]["template"].as_str() {
        let _ = write!(text, " \u{2190} {cause}{}", slots_text(message["cause"].get("slots")));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn escapes_markup_and_quotes() {
        let hostile = "<img src=x onerror=alert(1)>\"'&<script>";
        let out = escape_html(hostile);
        assert_eq!(out, "&lt;img src=x onerror=alert(1)&gt;&quot;&#39;&amp;&lt;script&gt;");
        assert!(!out.contains('<') && !out.contains('"') && !out.contains('\''));
    }

    #[test]
    fn fields_table_escapes_keys_and_values() {
        let report = json!({"<k>": "<b>x</b>", "list": [{"a": "\"q\""}]});
        let table = fields_table(&report);
        assert!(!table.contains("<b>"));
        assert!(table.contains("&lt;b&gt;x&lt;/b&gt;"));
        assert!(table.contains("&lt;k&gt;"));
        assert!(table.contains("list[0].a"));
    }

    #[test]
    fn layout_escapes_its_title() {
        assert!(!layout("<x>", "").contains("<title><x>"));
    }

    #[test]
    fn message_text_shows_template_and_slots() {
        let report = json!({"failure": {"message": {"template": "a.b", "slots": [{"type": "number", "value": 3}],
            "cause": {"template": "cause.x"}}}});
        assert_eq!(message_text(&report), "a.b (number=3) \u{2190} cause.x");
        assert_eq!(message_text(&json!({"failure": {}})), "sin plantilla (solo el tipo)");
    }
}
