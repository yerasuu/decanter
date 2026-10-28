//! A raw log as the admin pages show it: lines classified by what they report, and each line rendered as escaped
//! text with control and direction characters made visible and a cap on its width. A log is untrusted bytes; every
//! function here is linear in what it reads.

use std::fmt::Write as _;

use crate::html::escape_html;

/// How far into a line its class is looked for: a log line names its level and its fault near the start, and the
/// cap keeps a 5 MiB line from costing a full scan per marker.
const CLASSIFY_BYTES: usize = 4096;
/// Bytes sampled to tell a text log from a binary file.
const SNIFF_BYTES: usize = 64 * 1024;

/// What a highlighted line reports, in the order a line that fits several is classed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Fault,
    Calls,
    Error,
    Failed,
    Warn,
}

impl Mark {
    pub const ALL: [Self; 5] = [Self::Fault, Self::Calls, Self::Error, Self::Failed, Self::Warn];

    pub const fn class(self) -> &'static str {
        match self {
            Self::Fault => "fault",
            Self::Calls => "calls",
            Self::Error => "error",
            Self::Failed => "failed",
            Self::Warn => "warn",
        }
    }

    /// The text marker that says the same as the colour, for readers that do not see it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fault => "FALLO",
            Self::Calls => "LLAMADAS",
            Self::Error => "ERROR",
            Self::Failed => "FALLIDA",
            Self::Warn => "AVISO",
        }
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[must_use]
pub fn classify(line: &str) -> Option<Mark> {
    let head = line.get(..line.floor_char_boundary(CLASSIFY_BYTES)).unwrap_or(line);
    let any = |needles: &[&str]| needles.iter().any(|needle| head.contains(needle));
    if any(&["Guest fault", "Host fault", "mprotect failed", "VirtualProtect failed"]) {
        Some(Mark::Fault)
    } else if head.contains("Calls: summary") {
        Some(Mark::Calls)
    } else if any(&["<Error>", "<Critical>", "ERROR"]) {
        Some(Mark::Error)
    } else if ["failed", "stub", "unimplemented", "not implemented"]
        .iter()
        .any(|needle| contains_ignore_ascii_case(head, needle))
    {
        Some(Mark::Failed)
    } else if any(&["<Warning>", "WARN"]) {
        Some(Mark::Warn)
    } else {
        None
    }
}

/// Mostly NUL bytes, other control bytes or invalid UTF-8 at the start: not a text log.
#[must_use]
pub fn is_binary(bytes: &[u8]) -> bool {
    let sample = bytes.get(..SNIFF_BYTES).unwrap_or(bytes);
    let invalid: usize = sample.utf8_chunks().map(|chunk| chunk.invalid().len()).sum();
    let controls = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x1b))
        .count();
    (invalid + controls) * 10 > sample.len()
}

/// Characters that would hide, reorder or break the text around them: C0 and C1 controls (tab aside), DEL, the
/// bidirectional controls, the line and paragraph separators and the zero-width ones.
const fn is_hidden(c: char) -> bool {
    matches!(c, '\0'..='\u{8}' | '\u{a}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{61c}' | '\u{200b}'..='\u{200f}'
        | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

fn marker(c: char, out: &mut String) {
    out.push_str("<span class=\"ctl\">");
    let code = u32::from(c);
    match char::from_u32(0x2400 + code).filter(|_| code < 0x20) {
        Some(picture) => out.push(picture),
        None if c == '\u{7f}' => out.push('\u{2421}'),
        None => {
            let _ = write!(out, "&lt;U+{code:04X}&gt;");
        }
    }
    out.push_str("</span>");
}

/// Appends `line` as markup: escaped, every hidden character shown as a marker, and cut after `max_chars`
/// characters. Answers how many bytes were cut.
pub fn render_line(line: &str, max_chars: usize, out: &mut String) -> usize {
    let mut run_start = 0;
    let mut cut = None;
    for (count, (at, c)) in line.char_indices().enumerate() {
        if count == max_chars {
            cut = Some(at);
            break;
        }
        if is_hidden(c) {
            out.push_str(&escape_html(line.get(run_start..at).unwrap_or_default()));
            marker(c, out);
            run_start = at + c.len_utf8();
        }
    }
    let end = cut.unwrap_or(line.len());
    out.push_str(&escape_html(line.get(run_start..end).unwrap_or_default()));
    line.len() - end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(line: &str, max: usize) -> (String, usize) {
        let mut out = String::new();
        let cut = render_line(line, max, &mut out);
        (out, cut)
    }

    #[test]
    fn classes_follow_the_markers_in_their_order() {
        for (line, mark) in [
            ("Runtime: Guest fault at +0x1234 (SIGSEGV)", Some(Mark::Fault)),
            ("Host fault: write to 0x0", Some(Mark::Fault)),
            ("mprotect failed for 0x1000", Some(Mark::Fault)),
            ("VirtualProtect failed: 87", Some(Mark::Fault)),
            ("Calls: summary stub 1 sceNpTrophyRegisterContext", Some(Mark::Calls)),
            (
                "GPU [Lib.Font] <Error> font.cpp:7236 INVALID_WRITING",
                Some(Mark::Error),
            ),
            (
                "Runtime: import x (sceNetInit) is a stub: it succeeds",
                Some(Mark::Failed),
            ),
            ("open FAILED with 2", Some(Mark::Failed)),
            ("GPU [Render.Vulkan] <Warning> Extension unavailable", Some(Mark::Warn)),
            ("Frame pacing: median 4.16 ms", None),
        ] {
            assert_eq!(classify(line), mark, "{line}");
        }
    }

    #[test]
    fn hostile_text_is_escaped_and_hidden_characters_are_shown() {
        let (out, cut) = rendered("<script>alert(1)</script>\u{1b}[31mred\0\u{202e}evil\u{7f}\u{85}", 1000);
        assert_eq!(cut, 0);
        assert!(!out.contains("<script") && out.contains("&lt;script&gt;"));
        assert!(!out.contains('\u{1b}') && !out.contains('\0') && !out.contains('\u{202e}'));
        for shown in ["\u{241b}", "\u{2400}", "&lt;U+202E&gt;", "\u{2421}", "&lt;U+0085&gt;"] {
            assert!(out.contains(shown), "{shown} in {out}");
        }
        assert_eq!(out.matches("class=\"ctl\"").count(), 5);
    }

    #[test]
    fn width_cap_counts_characters_and_reports_the_cut_bytes() {
        let (out, cut) = rendered(&"é".repeat(10), 4);
        assert_eq!((out.as_str(), cut), ("éééé", 12));
        assert_eq!(rendered("short", 10).1, 0);
    }

    #[test]
    fn binary_is_told_from_a_log_with_a_few_controls() {
        assert!(is_binary(&[0_u8; 4096]));
        assert!(is_binary(&[0xff_u8, 0xfe, 0x00, 0x01].repeat(100)));
        let mut log = "GPU line with text\n".repeat(200).into_bytes();
        log.extend_from_slice(b"\0\0\x1b[0m");
        assert!(!is_binary(&log));
        assert!(!is_binary(b""));
    }
}
