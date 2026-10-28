//! The public, clean version of a raw log (decision #1060).
//!
//! A raw log is never published. What is published is the list of its lines that are instances
//! of the report vocabulary's templates, each kept as its template id and typed slots, with counts
//! of the rest. A line stays only when a template matches it and every slot also passes the
//! stricter rule of [`is_public_slot`]; any other line, and the `{_}` text of a kept one, is
//! dropped and counted. The readable text is rebuilt from the template and the slots when it is
//! shown, never taken from the line, so the result holds no free text by construction.

use serde::{Deserialize, Serialize};

use crate::schema::{Message, Product, Slot};
use crate::sources::day_log::failing_line;
use crate::text::cap_line;
use crate::validate::{Rejection, Rule};
use crate::vocab::{self, Family, MAX_LINE, Matcher};

/// Bumped when the stored form changes meaning.
pub const CLEAN_VERSION: u32 = 1;
/// Lines a clean log keeps at most; past it the rest of the log is not examined.
pub const MAX_CLEAN_LINES: usize = 2000;
/// Bytes of a raw log read at most, the server's own cap on an attachment.
pub const MAX_CLEAN_INPUT: usize = 5 * 1024 * 1024;
/// The stored form of the largest clean log is a few hundred kilobytes; anything above is refused
/// before it is parsed.
pub const MAX_CLEAN_BYTES: usize = 1024 * 1024;

/// Single-word function names that real failure lines carry; any other plain word in a call slot
/// could be a user or host name (#1033), so it is not published.
const CALL_WORDS: &[&str] = &[
    "Allocate",
    "Bind",
    "Close",
    "Create",
    "Destroy",
    "Dispatch",
    "Draw",
    "Flush",
    "Free",
    "Init",
    "Initialize",
    "Load",
    "Map",
    "Open",
    "Present",
    "Read",
    "Release",
    "Reset",
    "Signal",
    "Start",
    "Stop",
    "Submit",
    "Unload",
    "Unmap",
    "Wait",
    "Write",
    "main",
    "operator",
];

/// First words of the C library's snake-case names (`pthread_create`, `__cxa_atexit`); a name made
/// only of plain words joined by `_` with any other head (`john_smith`) could be someone's.
const SNAKE_HEADS: &[&str] = &[
    "aio",
    "clock",
    "cxa",
    "fcntl",
    "get",
    "gettimeofday",
    "is",
    "mem",
    "mmap",
    "mprotect",
    "munmap",
    "nanosleep",
    "posix",
    "pthread",
    "sce",
    "sched",
    "sem",
    "set",
    "sigaction",
    "std",
    "str",
    "sys",
];

/// Image stems that are one plain word and still name no one.
const IMAGE_WORDS: &[&str] = &[
    "KERNELBASE",
    "decanter",
    "eboot",
    "libc",
    "libdl",
    "libm",
    "libpthread",
    "ntdll",
    "psport",
    "ucrtbase",
];

/// Plain words that stand between a Vulkan valid-usage id's command and its number
/// (`VUID-vkCmdDraw-None-09600`).
const VUID_WORDS: &[&str] = &[
    "Builtin",
    "Capability",
    "Component",
    "Execution",
    "Input",
    "Interface",
    "Location",
    "None",
    "Output",
    "Shader",
    "Storage",
    "Uniform",
    "Variable",
    "Workgroup",
    "buffer",
    "count",
    "device",
    "extent",
    "flags",
    "format",
    "image",
    "layout",
    "memory",
    "module",
    "offset",
    "pipeline",
    "queue",
    "range",
    "sampler",
    "samples",
    "size",
    "stage",
    "stride",
    "type",
];

/// What of a log may be shown to anyone: the stored form, re-checked by [`validate_clean_log`]
/// whenever it is read back.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanLog {
    pub version: u32,
    pub product: Product,
    /// Lines of the log examined, blank ones included.
    pub total: u32,
    /// Examined lines at error level (psport's `ERROR:`, `STOP:`, faults and GPU `<Error>` or
    /// `<Critical>`; decanter's `ERROR`), kept or not.
    pub errors: u32,
    /// Examined lines not kept: `total` minus the kept ones.
    pub omitted: u32,
    /// The log was longer than [`MAX_CLEAN_INPUT`] or the line cap was reached: the rest of it was
    /// not examined and is in no count.
    pub capped: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<CleanLine>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanLine {
    /// 1-based, in the raw log.
    pub line: u32,
    pub message: Message,
}

const fn family(product: Product) -> Family {
    match product {
        Product::Decanter => Family::Decanter,
        Product::Psport => Family::Psport,
    }
}

/// `deck`, `Deck`, `DECK`: letters only, in one case or capitalized.
fn is_plain_word(text: &str) -> bool {
    let rest = text.get(1..).unwrap_or_default();
    !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_alphabetic())
        && (rest.bytes().all(|b| b.is_ascii_lowercase()) || text.bytes().all(|b| b.is_ascii_uppercase()))
}

/// Ids, serials, tokens and addresses carry runs of digits or many groups of them; code names
/// (`sceNpMatching2SetDefaultRequestOptParam`, `libSceFios2`) carry two groups at most.
fn has_id_digits(text: &str) -> bool {
    let bytes = text.as_bytes();
    let groups = bytes
        .iter()
        .enumerate()
        .filter(|&(at, b)| b.is_ascii_digit() && (at == 0 || bytes.get(at - 1).is_some_and(|p| !p.is_ascii_digit())))
        .count();
    groups > 2 || bytes.windows(4).any(|w| w.iter().all(u8::is_ascii_digit))
}

/// A part of an identifier that names code rather than someone: never a lone plain word unless
/// `words` lists it.
fn is_code_name(part: &str, words: &[&str]) -> bool {
    !part.is_empty() && !has_id_digits(part) && (!is_plain_word(part) || words.contains(&part))
}

/// `sceKernelOpen`, `Foo::Bar` with each part starting as an identifier does, so `fe80::1c2b` (an
/// address the call shape also reads) is refused.
fn is_public_call(name: &str) -> bool {
    name.split("::").all(|part| {
        let core = part.trim_start_matches(['_', '~']);
        let mut words = core.split('_').filter(|word| !word.is_empty());
        let snake_of_plain_words = core.contains('_') && words.clone().all(is_plain_word);
        core.starts_with(|c: char| c.is_ascii_alphabetic())
            && !core.contains(':')
            && is_code_name(core, CALL_WORDS)
            && (!snake_of_plain_words || words.next().is_some_and(|head| SNAKE_HEADS.contains(&head)))
    })
}

/// A listed system image, a `lib` one (`libpsport-gpu.dll`, `libSceFios2.prx`) or a lowercase name
/// with a version or width in it (`vulkan-1.dll`, `nvoglv64.dll`); an executable's name is often
/// the user's own, so a plain one is refused.
fn is_public_image(name: &str) -> bool {
    let Some((stem, _)) = name.rsplit_once('.') else {
        return false;
    };
    let parts_fit = |text: &str| {
        !text.contains('.')
            && !has_id_digits(text)
            && text
                .split(['-', '_'])
                .all(|part| !part.is_empty() && !has_id_digits(part))
    };
    IMAGE_WORDS.contains(&stem)
        || stem
            .strip_prefix("lib")
            .is_some_and(|rest| !rest.is_empty() && parts_fit(rest))
        || (stem.len() <= 24
            && stem.bytes().any(|b| b.is_ascii_digit())
            && stem
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && parts_fit(stem))
}

/// `VUID-<command or structure>-<words>-<five digits>`, every word a code name.
fn is_public_vuid(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    let (Some(&"VUID"), Some(api), Some(number)) = (parts.first(), parts.get(1), parts.last()) else {
        return false;
    };
    let middle = parts.get(2..parts.len().saturating_sub(1)).unwrap_or_default();
    ["vk", "Vk", "Runtime", "Standalone"]
        .iter()
        .any(|prefix| api.starts_with(prefix))
        && is_code_name(api, &[])
        && number.len() == 5
        && number.bytes().all(|b| b.is_ascii_digit())
        && !has_id_digits(&middle.concat())
        && middle
            .iter()
            .all(|part| part.starts_with(|c: char| c.is_ascii_alphabetic()) && is_code_name(part, VUID_WORDS))
}

/// Stricter than the vocabulary's own slot rules, for text anyone reads: a shape rule cannot tell
/// a one-word user, host or device name from a technical word, and the server has no identity list
/// to tell them apart, so a slot that could be one is refused. `VK_ERROR_DEVICE_LOST` passes,
/// `DELL` does not; `sceKernelOpen` passes, `yerasuu` does not.
fn is_public_slot(slot: &Slot) -> bool {
    match slot {
        Slot::Call(name) => is_public_call(name),
        Slot::Image(name) => is_public_image(name),
        Slot::Const(name) => name.contains('_') && name.split('_').all(|part| !part.is_empty() && !has_id_digits(part)),
        Slot::Vuid(name) => is_public_vuid(name),
        Slot::Number(_) | Slot::Offset(_) | Slot::Code(_) | Slot::Version(_) | Slot::Class(_) | Slot::Choice(_) => true,
    }
}

fn is_public(message: &Message) -> bool {
    message.slots.iter().all(is_public_slot)
        && message
            .cause
            .as_ref()
            .is_none_or(|cause| cause.slots.iter().all(is_public_slot))
}

fn is_psport_error(line: &str) -> bool {
    line.starts_with("ERROR:")
        || line.starts_with("STOP:")
        || line.contains("fault (signal")
        || line.contains("Fault (signal")
        || (line.starts_with("GPU [") && (line.contains("<Error>") || line.contains("<Critical>")))
}

/// The line's level and, when it can be one, its message as the vocabulary reads it.
fn read_line(line: &str, family: Family, matcher: &Matcher) -> (bool, Option<Message>) {
    let head = cap_line(line, MAX_LINE).trim();
    let whole = line.len() <= MAX_LINE;
    match family {
        Family::Psport => (is_psport_error(head), whole.then(|| matcher.normalize(head)).flatten()),
        Family::Decanter => (
            head.split_whitespace().nth(1) == Some("ERROR"),
            whole
                .then(|| failing_line(head))
                .flatten()
                .and_then(|message| matcher.normalize(&message)),
        ),
    }
}

/// The public, clean version of `text`, a raw log of `product`: the lines that are instances of
/// its vocabulary, at most `max_lines` (and never more than [`MAX_CLEAN_LINES`]), and counts of
/// the rest.
///
/// Linear in the input: only the first [`MAX_CLEAN_INPUT`] bytes are read, a line longer than
/// the vocabulary's longest is counted without being matched, and once the line cap is reached
/// nothing more is examined.
#[must_use]
pub fn clean_log(text: &str, product: Product, max_lines: usize) -> CleanLog {
    let max_lines = max_lines.min(MAX_CLEAN_LINES);
    let family = family(product);
    let matcher = Matcher::new(family);
    let input = cap_line(text, MAX_CLEAN_INPUT);
    let mut log = CleanLog {
        version: CLEAN_VERSION,
        product,
        total: 0,
        errors: 0,
        omitted: 0,
        capped: input.len() < text.len(),
        lines: Vec::new(),
    };
    for (index, line) in input.lines().enumerate() {
        if log.lines.len() >= max_lines {
            log.capped = true;
            break;
        }
        let Ok(number) = u32::try_from(index + 1) else {
            log.capped = true;
            break;
        };
        log.total = number;
        let (error, message) = read_line(line, family, &matcher);
        log.errors += u32::from(error);
        if let Some(message) = message.filter(is_public) {
            log.lines.push(CleanLine { line: number, message });
        }
    }
    log.omitted = log.total - u32::try_from(log.lines.len()).unwrap_or(log.total);
    log
}

/// Parses and checks a stored clean log of `product` before it is shown.
///
/// # Errors
///
/// Every rejection found: the shape is checked first and stops at its first miss; then the counts,
/// the line numbers, each message against the vocabulary and each slot against the public rule.
pub fn validate_clean_log(bytes: &[u8], product: Product) -> Result<CleanLog, Vec<Rejection>> {
    if bytes.len() > MAX_CLEAN_BYTES {
        return Err(vec![Rejection::new("", Rule::TooLarge)]);
    }
    let log: CleanLog = serde_json::from_slice(bytes).map_err(|_| vec![Rejection::new("", Rule::Syntax)])?;
    let mut out = Vec::new();
    if log.version != CLEAN_VERSION {
        out.push(Rejection::new("version", Rule::Schema));
    }
    if log.product != product {
        out.push(Rejection::new("product", Rule::Product));
    }
    if log.lines.len() > MAX_CLEAN_LINES {
        out.push(Rejection::new("lines", Rule::TooMany));
    }
    if log.errors > log.total {
        out.push(Rejection::new("errors", Rule::OutOfRange));
    }
    let kept = u32::try_from(log.lines.len()).unwrap_or(u32::MAX);
    if log.total.checked_sub(kept) != Some(log.omitted) {
        out.push(Rejection::new("omitted", Rule::OutOfRange));
    }
    let family = family(product);
    let mut previous = 0;
    for (index, line) in log.lines.iter().enumerate() {
        if line.line <= previous || line.line > log.total {
            out.push(Rejection::new(format!("lines[{index}].line"), Rule::OutOfRange));
        }
        previous = line.line;
        let path = format!("lines[{index}].message");
        vocab::check_at(&path, &line.message, family, &mut out);
        if !is_public(&line.message) {
            out.push(Rejection::new(path, Rule::BadSlot));
        }
    }
    if out.is_empty() { Ok(log) } else { Err(out) }
}

/// The readable text of a message of `product`'s vocabulary, rebuilt from its template and slots;
/// `None` when the template is unknown or a slot does not fit it.
#[must_use]
pub fn message_text(message: &Message, product: Product) -> Option<String> {
    vocab::render(message, family(product))
}
