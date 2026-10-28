//! A bounded line diff for comparing two runs' logs (#1063), written here because no diff crate is in the offline
//! cache. Lines are interned to integers, the common head and tail are trimmed, and Myers' O((N+M)·D) algorithm runs
//! on what is left with D capped at `MAX_EDITS`, whose trace costs O(D²) memory. Past `max_lines` lines in both logs,
//! or past `MAX_EDITS`, it falls back to comparing line by line at the same position, keeping the first
//! `FALLBACK_ROWS` differing lines and counting the rest. With `normalize`, volatile tokens (times, long hex and
//! decimal numbers, the `# ended`, `# duration` and `# exit` lines) compare equal; the rows still show the real
//! text.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

/// Edit distance past which the diff falls back.
pub const MAX_EDITS: usize = 1000;
/// Differing rows a fallback keeps.
pub const FALLBACK_ROWS: usize = 500;
/// Lines of an equal run kept at each end, as context for the changes around it.
pub const CONTEXT: usize = 3;
/// Decimal digits above which a number is volatile.
const MAX_STABLE_DIGITS: usize = 4;
/// Hex digits from which a number written with or without `0x` is volatile (an address, a hash).
const MIN_VOLATILE_HEX: usize = 4;
const MIN_BARE_HEX: usize = 8;

pub struct Side<'a> {
    /// From 1.
    pub number: usize,
    pub text: &'a str,
}

/// A run of equal lines: where it starts on each side (from 1), how long it is, and its first and last lines.
pub struct Run<'a> {
    pub a: usize,
    pub b: usize,
    pub len: usize,
    pub head: Vec<&'a str>,
    pub tail: Vec<&'a str>,
}

pub enum Row<'a> {
    Same(Run<'a>),
    Removed(Side<'a>),
    Added(Side<'a>),
    Changed(Side<'a>, Side<'a>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub lines_a: usize,
    pub lines_b: usize,
    pub equal: usize,
    pub changed: usize,
    pub removed: usize,
    pub added: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// More lines than `max_lines` in both logs together.
    TooLong,
    /// More than `MAX_EDITS` edits apart.
    TooDifferent,
}

pub struct Diff<'a> {
    pub rows: Vec<Row<'a>>,
    pub summary: Summary,
    pub fallback: Option<Fallback>,
    /// The fallback kept only the first `FALLBACK_ROWS` differing rows; the summary counts them all.
    pub truncated: bool,
}

const fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn digits(bytes: &[u8], at: usize, count: usize) -> bool {
    bytes
        .get(at..at + count)
        .is_some_and(|run| run.iter().all(u8::is_ascii_digit))
}

fn run_len(bytes: &[u8], at: usize, keep: impl Fn(u8) -> bool) -> usize {
    bytes
        .get(at..)
        .map_or(0, |rest| rest.iter().take_while(|&&b| keep(b)).count())
}

/// `HH:MM:SS` with an optional fraction; its length.
fn time_len(bytes: &[u8], at: usize) -> Option<usize> {
    let fixed = digits(bytes, at, 2)
        && bytes.get(at + 2) == Some(&b':')
        && digits(bytes, at + 3, 2)
        && bytes.get(at + 5) == Some(&b':')
        && digits(bytes, at + 6, 2);
    if !fixed {
        return None;
    }
    let fraction = if bytes.get(at + 8) == Some(&b'.') {
        let len = run_len(bytes, at + 9, |b| b.is_ascii_digit());
        if len > 0 { len + 1 } else { 0 }
    } else {
        0
    };
    Some(8 + fraction)
}

/// `YYYY-MM-DD`, then optionally `T` or a space, a time and a zone (`Z`, `+HH:MM`, `+HHMM`); its length.
fn datetime_len(bytes: &[u8], at: usize) -> Option<usize> {
    let date = digits(bytes, at, 4)
        && bytes.get(at + 4) == Some(&b'-')
        && digits(bytes, at + 5, 2)
        && bytes.get(at + 7) == Some(&b'-')
        && digits(bytes, at + 8, 2);
    if !date {
        return None;
    }
    let mut len = 10;
    if matches!(bytes.get(at + len), Some(b'T' | b' '))
        && let Some(time) = time_len(bytes, at + len + 1)
    {
        len += 1 + time;
        match bytes.get(at + len) {
            Some(b'Z') => len += 1,
            Some(b'+' | b'-') if digits(bytes, at + len + 1, 2) => {
                let colon = usize::from(bytes.get(at + len + 3) == Some(&b':'));
                if digits(bytes, at + len + 3 + colon, 2) {
                    len += 5 + colon;
                }
            }
            _ => {}
        }
    }
    Some(len)
}

/// The volatile token starting at `at` (a word boundary), as its length and its stand-in.
fn volatile_token(bytes: &[u8], at: usize) -> Option<(usize, &'static str)> {
    if let Some(len) = datetime_len(bytes, at).or_else(|| time_len(bytes, at)) {
        return Some((len, "<time>"));
    }
    if bytes.get(at) == Some(&b'0') && matches!(bytes.get(at + 1), Some(b'x' | b'X')) {
        let len = run_len(bytes, at + 2, |b| b.is_ascii_hexdigit());
        return (len >= MIN_VOLATILE_HEX).then_some((len + 2, "0x#"));
    }
    let decimal = run_len(bytes, at, |b| b.is_ascii_digit());
    if decimal > MAX_STABLE_DIGITS {
        return Some((decimal, "#"));
    }
    let hex = run_len(bytes, at, |b| b.is_ascii_hexdigit());
    let ends = !bytes.get(at + hex).copied().is_some_and(is_word);
    let has_digit = bytes
        .get(at..at + hex)
        .is_some_and(|run| run.iter().any(u8::is_ascii_digit));
    (hex >= MIN_BARE_HEX && ends && has_digit).then_some((hex, "#"))
}

/// `line` with its volatile tokens replaced, for comparison only. Linear: every token is consumed once.
#[must_use]
pub fn normalize(line: &str) -> Cow<'_, str> {
    if let Some(volatile) = ["# ended", "# duration", "# exit"]
        .into_iter()
        .find(|prefix| line.starts_with(prefix))
    {
        return Cow::Borrowed(volatile);
    }
    let bytes = line.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let boundary = at == 0 || !bytes.get(at - 1).copied().is_some_and(is_word);
        if boundary && let Some((len, stand_in)) = volatile_token(bytes, at) {
            let text = out.get_or_insert_with(|| String::with_capacity(line.len()));
            text.push_str(line.get(copied..at).unwrap_or_default());
            text.push_str(stand_in);
            at += len;
            copied = at;
        } else {
            at += 1;
        }
    }
    out.map_or(Cow::Borrowed(line), |mut text| {
        text.push_str(line.get(copied..).unwrap_or_default());
        Cow::Owned(text)
    })
}

fn key(line: &str, normalized: bool) -> Cow<'_, str> {
    if normalized {
        normalize(line)
    } else {
        Cow::Borrowed(line)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Edit {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

const UNREACHED: usize = usize::MAX;

/// The edit graph: `n` lines of the old side across, `m` of the new one down.
#[derive(Clone, Copy)]
struct Grid {
    n: usize,
    m: usize,
}

/// Where the furthest path of step `d` on diagonal index `j` (`k = j - d`) starts, from step `d - 1`'s `previous`
/// (indexed the same way): `true` for a move down (an insertion), and the `x` after the move. Only moves that stay
/// inside the grid count, so no path can end outside it.
fn start(previous: &[usize], d: usize, j: usize, grid: Grid) -> Option<(bool, usize)> {
    let fits = |x: usize| x <= grid.n && (x + d).checked_sub(j).is_some_and(|y| y <= grid.m);
    let down = previous.get(j).copied().filter(|&x| x != UNREACHED && fits(x));
    let right = j
        .checked_sub(2)
        .and_then(|i| previous.get(i))
        .copied()
        .filter(|&x| x != UNREACHED)
        .map(|x| x + 1)
        .filter(|&x| fits(x));
    match (down, right) {
        (Some(x_down), Some(x_right)) if x_right > x_down => Some((false, x_right)),
        (Some(x_down), _) => Some((true, x_down)),
        (None, Some(x_right)) => Some((false, x_right)),
        (None, None) => None,
    }
}

/// The shortest edit script from `old` to `new`, or `None` past `max_edits`.
fn myers(old: &[u32], new: &[u32], max_edits: usize) -> Option<Vec<Edit>> {
    let grid = Grid {
        n: old.len(),
        m: new.len(),
    };
    let mut trace: Vec<Vec<usize>> = Vec::new();
    for d in 0..=max_edits.min(grid.n + grid.m) {
        let mut reach = vec![UNREACHED; 2 * d + 1];
        for j in (0..=2 * d).step_by(2) {
            let x = if d == 0 {
                0
            } else {
                match trace.last().and_then(|previous| start(previous, d, j, grid)) {
                    Some((_, x)) => x,
                    None => continue,
                }
            };
            let y = x + d - j;
            let slide = old
                .get(x..)
                .unwrap_or_default()
                .iter()
                .zip(new.get(y..).unwrap_or_default())
                .take_while(|(left, right)| left == right)
                .count();
            if let Some(slot) = reach.get_mut(j) {
                *slot = x + slide;
            }
            if x + slide == grid.n && y + slide == grid.m {
                trace.push(reach);
                return Some(backtrack(&trace, grid));
            }
        }
        trace.push(reach);
    }
    None
}

fn backtrack(trace: &[Vec<usize>], grid: Grid) -> Vec<Edit> {
    let mut edits = Vec::with_capacity(grid.n + grid.m);
    let (mut x, mut y) = (grid.n, grid.m);
    for d in (1..trace.len()).rev() {
        let found = (x + d)
            .checked_sub(y)
            .zip(trace.get(d - 1))
            .and_then(|(j, previous)| start(previous, d, j, grid));
        let Some((down, start_x)) = found else {
            break;
        };
        while x > start_x && y > 0 {
            x -= 1;
            y -= 1;
            edits.push(Edit::Equal(x, y));
        }
        if down {
            y = y.saturating_sub(1);
            edits.push(Edit::Insert(y));
        } else {
            x = x.saturating_sub(1);
            edits.push(Edit::Delete(x));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        edits.push(Edit::Equal(x, y));
    }
    edits.reverse();
    edits
}

/// Collects equal lines into one row that keeps only its ends.
struct RunBuilder<'a> {
    a: usize,
    b: usize,
    len: usize,
    head: Vec<&'a str>,
    tail: VecDeque<&'a str>,
}

impl<'a> RunBuilder<'a> {
    const fn new() -> Self {
        Self {
            a: 0,
            b: 0,
            len: 0,
            head: Vec::new(),
            tail: VecDeque::new(),
        }
    }

    fn push(&mut self, a: usize, b: usize, line: &'a str) {
        if self.len == 0 {
            (self.a, self.b) = (a + 1, b + 1);
        }
        if self.head.len() < CONTEXT {
            self.head.push(line);
        } else {
            self.tail.push_back(line);
            if self.tail.len() > CONTEXT {
                self.tail.pop_front();
            }
        }
        self.len += 1;
    }

    fn finish(&mut self, rows: &mut Vec<Row<'a>>, summary: &mut Summary) {
        if self.len == 0 {
            return;
        }
        summary.equal += self.len;
        let done = std::mem::replace(self, Self::new());
        rows.push(Row::Same(Run {
            a: done.a,
            b: done.b,
            len: done.len,
            head: done.head,
            tail: done.tail.into(),
        }));
    }
}

fn side<'a>(lines: &[&'a str], index: usize) -> Side<'a> {
    Side {
        number: index + 1,
        text: lines.get(index).copied().unwrap_or_default(),
    }
}

fn pair<'a>(a: &[&'a str], b: &[&'a str], removed: &mut Vec<usize>, added: &mut Vec<usize>, out: &mut Diff<'a>) {
    let both = removed.len().min(added.len());
    for (&left, &right) in removed.iter().zip(added.iter()) {
        out.rows.push(Row::Changed(side(a, left), side(b, right)));
    }
    for &left in removed.get(both..).unwrap_or_default() {
        out.rows.push(Row::Removed(side(a, left)));
    }
    for &right in added.get(both..).unwrap_or_default() {
        out.rows.push(Row::Added(side(b, right)));
    }
    out.summary.changed += both;
    out.summary.removed += removed.len() - both;
    out.summary.added += added.len() - both;
    removed.clear();
    added.clear();
}

fn from_edits<'a>(a: &[&'a str], b: &[&'a str], edits: &[Edit]) -> Diff<'a> {
    let mut out = Diff {
        rows: Vec::new(),
        summary: Summary {
            lines_a: a.len(),
            lines_b: b.len(),
            ..Summary::default()
        },
        fallback: None,
        truncated: false,
    };
    let mut run = RunBuilder::new();
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for edit in edits {
        match *edit {
            Edit::Equal(left, right) => {
                pair(a, b, &mut removed, &mut added, &mut out);
                run.push(left, right, a.get(left).copied().unwrap_or_default());
            }
            Edit::Delete(left) => {
                run.finish(&mut out.rows, &mut out.summary);
                removed.push(left);
            }
            Edit::Insert(right) => {
                run.finish(&mut out.rows, &mut out.summary);
                added.push(right);
            }
        }
    }
    pair(a, b, &mut removed, &mut added, &mut out);
    run.finish(&mut out.rows, &mut out.summary);
    out
}

/// Line `i` against line `i`, streaming both texts: constant memory beyond the rows it keeps.
fn positional<'a>(a: &'a str, b: &'a str, normalized: bool, fallback: Fallback) -> Diff<'a> {
    let mut out = Diff {
        rows: Vec::new(),
        summary: Summary::default(),
        fallback: Some(fallback),
        truncated: false,
    };
    let (mut left_lines, mut right_lines) = (a.lines(), b.lines());
    let mut run = RunBuilder::new();
    let mut kept = 0;
    let mut index = 0;
    loop {
        let row = match (left_lines.next(), right_lines.next()) {
            (None, None) => break,
            (Some(left), Some(right)) if key(left, normalized) == key(right, normalized) => {
                if kept < FALLBACK_ROWS {
                    run.push(index, index, left);
                } else {
                    out.summary.equal += 1;
                }
                index += 1;
                continue;
            }
            (Some(left), Some(right)) => {
                out.summary.changed += 1;
                Row::Changed(
                    Side {
                        number: index + 1,
                        text: left,
                    },
                    Side {
                        number: index + 1,
                        text: right,
                    },
                )
            }
            (Some(left), None) => {
                out.summary.removed += 1;
                Row::Removed(Side {
                    number: index + 1,
                    text: left,
                })
            }
            (None, Some(right)) => {
                out.summary.added += 1;
                Row::Added(Side {
                    number: index + 1,
                    text: right,
                })
            }
        };
        index += 1;
        if kept < FALLBACK_ROWS {
            run.finish(&mut out.rows, &mut out.summary);
            out.rows.push(row);
            kept += 1;
        } else {
            out.truncated = true;
        }
    }
    run.finish(&mut out.rows, &mut out.summary);
    out.summary.lines_a = a.lines().count();
    out.summary.lines_b = b.lines().count();
    out
}

/// Compares `a` with `b` line by line. Linear in the input, plus O((N+M)·D) on the part between the common head and
/// tail, with N+M at most `max_lines` and D at most `MAX_EDITS`.
#[must_use]
pub fn diff<'a>(a: &'a str, b: &'a str, normalized: bool, max_lines: usize) -> Diff<'a> {
    if a.lines().count() + b.lines().count() > max_lines {
        return positional(a, b, normalized, Fallback::TooLong);
    }
    let left: Vec<&str> = a.lines().collect();
    let right: Vec<&str> = b.lines().collect();
    let mut interned: HashMap<Cow<'a, str>, u32> = HashMap::with_capacity(left.len() + right.len());
    let mut intern = |line: &'a str| {
        let next = u32::try_from(interned.len()).unwrap_or(u32::MAX);
        *interned.entry(key(line, normalized)).or_insert(next)
    };
    let ids_a: Vec<u32> = left.iter().map(|line| intern(line)).collect();
    let ids_b: Vec<u32> = right.iter().map(|line| intern(line)).collect();
    let head = ids_a.iter().zip(&ids_b).take_while(|(x, y)| x == y).count();
    let tail = ids_a
        .get(head..)
        .unwrap_or_default()
        .iter()
        .rev()
        .zip(ids_b.get(head..).unwrap_or_default().iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let middle_a = ids_a.get(head..ids_a.len() - tail).unwrap_or_default();
    let middle_b = ids_b.get(head..ids_b.len() - tail).unwrap_or_default();
    let Some(middle) = myers(middle_a, middle_b, MAX_EDITS) else {
        return positional(a, b, normalized, Fallback::TooDifferent);
    };
    let mut edits = Vec::with_capacity(head + middle.len() + tail);
    edits.extend((0..head).map(|i| Edit::Equal(i, i)));
    edits.extend(middle.into_iter().map(|edit| match edit {
        Edit::Equal(x, y) => Edit::Equal(x + head, y + head),
        Edit::Delete(x) => Edit::Delete(x + head),
        Edit::Insert(y) => Edit::Insert(y + head),
    }));
    let (end_a, end_b) = (ids_a.len() - tail, ids_b.len() - tail);
    edits.extend((0..tail).map(|i| Edit::Equal(end_a + i, end_b + i)));
    from_edits(&left, &right, &edits)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn numbered(prefix: &str, count: usize) -> String {
        let mut text = String::new();
        for n in 0..count {
            let _ = writeln!(text, "{prefix}{n}");
        }
        text
    }

    fn shape(diff: &Diff<'_>) -> Vec<String> {
        diff.rows
            .iter()
            .map(|row| match row {
                Row::Same(run) => format!("={}@{}/{}", run.len, run.a, run.b),
                Row::Removed(side) => format!("-{}:{}", side.number, side.text),
                Row::Added(side) => format!("+{}:{}", side.number, side.text),
                Row::Changed(left, right) => format!("~{}:{}>{}:{}", left.number, left.text, right.number, right.text),
            })
            .collect()
    }

    #[test]
    fn equal_logs_are_one_run() {
        let text = "a\nb\nc\nd\ne\nf\ng\nh";
        let same = diff(text, text, false, 1000);
        assert_eq!(shape(&same), ["=8@1/1"]);
        assert_eq!((same.summary.equal, same.summary.changed, same.fallback), (8, 0, None));
        if let Some(Row::Same(run)) = same.rows.first() {
            assert_eq!(
                (run.head.as_slice(), run.tail.as_slice()),
                (&["a", "b", "c"][..], &["f", "g", "h"][..])
            );
        }
        assert!(diff("", "", false, 10).rows.is_empty());
    }

    #[test]
    fn added_removed_and_changed_lines_carry_both_numbers() {
        let a = "start\nkeep 1\nold\nkeep 2\ngone\nend";
        let b = "start\nkeep 1\nnew\nkeep 2\nend\nextra";
        let result = diff(a, b, false, 1000);
        assert_eq!(
            shape(&result),
            ["=2@1/1", "~3:old>3:new", "=1@4/4", "-5:gone", "=1@6/5", "+6:extra"]
        );
        let summary = result.summary;
        assert_eq!(
            (summary.equal, summary.changed, summary.removed, summary.added),
            (4, 1, 1, 1)
        );
        assert_eq!((summary.lines_a, summary.lines_b), (6, 6));
        assert_eq!(shape(&diff("", "x\ny", false, 10)), ["+1:x", "+2:y"]);
        assert_eq!(shape(&diff("x", "", false, 10)), ["-1:x"]);
    }

    #[test]
    fn the_script_is_minimal() {
        let a = "a\nb\nc\na\nb\nb\na";
        let b = "c\nb\na\nb\na\nc";
        let result = diff(a, b, false, 1000);
        let edits = result.summary.removed + result.summary.added + 2 * result.summary.changed;
        assert_eq!(edits, 5, "Myers' example: D = 5");
        assert_eq!(result.summary.equal, 4);
    }

    #[test]
    fn volatile_tokens_compare_equal_only_when_normalized() {
        let a = "[2026-10-17 13:57:01.821] frame 123456 at 0x7f934f8a12 took 4 ms\n# ended 2026-10-17T13:58:00Z\n\
                 GXWorker sema +0x263ae6a hash 7f934f8a\nsame 12 CUSA03173";
        let b = "[2026-10-18 20:35:32.854] frame 998877 at 0x7ffe00112233 took 4 ms\n# ended 2026-10-18T20:36:00Z\n\
                 GXWorker sema +0x263bbbb hash 00ff11aa\nsame 12 CUSA03173";
        let raw = diff(a, b, false, 1000);
        assert_eq!(raw.summary.changed, 3);
        let normalized = diff(a, b, true, 1000);
        assert_eq!(shape(&normalized), ["=4@1/1"]);
        assert_eq!(
            normalize("t=13:57:01 id CUSA03173 v 1234 x 12345 sceFoo12345 0x12"),
            "t=<time> id CUSA03173 v 1234 x # sceFoo12345 0x12"
        );
        assert_eq!(normalize("deadbeefcafe word"), "deadbeefcafe word", "no digit: a word");
        assert!(matches!(normalize("plain line"), Cow::Borrowed(_)));
    }

    #[test]
    fn too_long_and_too_different_fall_back_to_positions() {
        let a = numbered("a", 50);
        let mut b = String::new();
        for n in 0..50 {
            let _ = writeln!(b, "{}{n}", if n % 10 == 0 { "a" } else { "b" });
        }
        let long = diff(&a, &b, false, 60);
        assert_eq!(long.fallback, Some(Fallback::TooLong));
        assert_eq!((long.summary.changed, long.summary.equal), (45, 5));
        assert_eq!(long.rows.first().map(|row| matches!(row, Row::Same(_))), Some(true));

        let a = numbered("left ", 3000);
        let b = numbered("right ", 2000);
        let different = diff(&a, &b, false, 100_000);
        assert_eq!(different.fallback, Some(Fallback::TooDifferent));
        assert!(different.truncated);
        assert_eq!(different.rows.len(), FALLBACK_ROWS);
        let summary = different.summary;
        assert_eq!((summary.changed, summary.removed, summary.added), (2000, 1000, 0));
        assert_eq!((summary.lines_a, summary.lines_b), (3000, 2000));
    }

    #[test]
    fn worst_cases_stay_bounded() {
        let started = std::time::Instant::now();
        let (x, y) = (numbered("x", 10_000), numbered("y", 10_000));
        let different = diff(&x, &y, true, 20_000);
        assert_eq!(different.fallback, Some(Fallback::TooDifferent));
        let (a, b) = ("a\n".repeat(2_621_440), "b\n".repeat(2_621_440));
        let long = diff(&a, &b, true, 20_000);
        assert_eq!(long.fallback, Some(Fallback::TooLong));
        assert_eq!((long.rows.len(), long.summary.changed), (FALLBACK_ROWS, 2_621_440));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_large_equal_head_and_tail_cost_nothing_in_the_diff() {
        let body = numbered("line ", 9000);
        let a = format!("{body}x\n{body}");
        let b = format!("{body}y\n{body}");
        let result = diff(&a, &b, false, 40_000);
        assert_eq!(result.fallback, None);
        assert_eq!(shape(&result), ["=9000@1/1", "~9001:x>9001:y", "=9000@9002/9002"]);
    }
}
