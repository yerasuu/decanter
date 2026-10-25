//! The shapes a token of a failure line may have, and the private ones it may not.

use super::{Mode, Private, core};

const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "h", "hpp", "inc", "rs", "py", "sh", "json", "ini", "toml", "cfg",
];
const IMAGE_EXTENSIONS: &[&str] = &["dll", "so", "prx", "sprx", "exe", "elf", "bin", "self", "dylib"];
const IMAGE_PREFIXES: &[&str] = &["lib", "eboot", "sce", "psport"];
const SECRET_PREFIXES: &[&str] = &["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "sk-"];
const SCRIPT_SCHEMES: &[&str] = &["javascript:", "vbscript:"];
const NAME_SYMBOLS: &str = "®™©–—‘’“”…·•";
const TAGS: &[&str] = &[
    "Error", "Critical", "Warning", "Info", "Debug", "path", "id", "addr", "ip", "host", "time", "name", "text",
];
const OS_WORDS: &[&str] = &[
    "steamos",
    "windows",
    "macos",
    "darwin",
    "linux",
    "ubuntu",
    "debian",
    "fedora",
    "arch",
    "nixos",
    "opensuse",
    "leap",
    "tumbleweed",
    "manjaro",
    "mint",
    "bazzite",
    "chimeraos",
    "nobara",
    "cachyos",
    "endeavouros",
    "garuda",
    "gentoo",
    "alpine",
    "rolling",
    "lts",
    "server",
    "workstation",
    "home",
    "pro",
    "enterprise",
    "education",
    "holo",
    "silverblue",
    "kinoite",
    "edition",
    "os",
    "x",
];
const GPU_WORDS: &[&str] = &[
    "radeon",
    "graphics",
    "pro",
    "vega",
    "iris",
    "xe",
    "arc",
    "plus",
    "max",
    "ultra",
    "series",
    "laptop",
    "mobile",
    "ti",
    "super",
    "core",
    "geforce",
    "gtx",
    "rtx",
    "quadro",
    "tesla",
    "integrated",
    "gpu",
    "uhd",
    "hd",
    "apple",
    "intel",
    "nvidia",
    "amd",
    "radeonsi",
    "vangogh",
    "workstation",
    "generation",
    "renderer",
];
const DRIVER_NAMES: &[&str] = &["radv", "amdvlk", "nvidia", "intel", "mesa", "anv"];

fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn all_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `sceKernelOpen`, `image-relative`, `Vulkan::Instance`; not an id, a token or a MAC address that
/// happens to be made of word characters.
pub fn is_word(text: &str) -> bool {
    text.split("::").all(|part| {
        part.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'~')
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'~'))
    }) && !is_secret_shape(text)
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// `ghp_…`, `sk-…`, `steam7656…`: the way a token or an account id starts, whatever follows.
pub fn has_secret_prefix(text: &str) -> bool {
    SECRET_PREFIXES
        .iter()
        .any(|prefix| starts_with_ignore_case(text, prefix))
        || ["steam", "s-1-"].iter().any(|prefix| {
            starts_with_ignore_case(text, prefix)
                && text
                    .get(prefix.len()..)
                    .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
        })
}

fn is_mac(text: &str) -> bool {
    let mut groups = text.split('-');
    (0..6).all(|_| groups.next().is_some_and(|g| g.len() == 2 && all_hex(g))) && groups.next().is_none()
}

/// A run of letters and digits that reads as encoded bytes: mixed case with digits scattered in it
/// (`AbC123dEf456`), or a long one with far more capitals than a CamelCase name has. Identifiers
/// that are all capitals (`CUSA03173`), all lowercase, or end in a short number are not.
fn is_random_run(run: &str) -> bool {
    let count = |keep: fn(&u8) -> bool| run.bytes().filter(|b| keep(b)).count();
    let (upper, lower, digits) = (
        count(u8::is_ascii_uppercase),
        count(u8::is_ascii_lowercase),
        count(u8::is_ascii_digit),
    );
    if run.len() < 8 || upper == 0 || lower == 0 {
        return false;
    }
    if digits == 0 {
        return run.len() >= 16 && upper * 3 > run.len();
    }
    let groups = run
        .split(|c: char| !c.is_ascii_digit())
        .filter(|group| !group.is_empty());
    groups.clone().count() >= 4 || groups.into_iter().any(|group| group.len() >= 3)
}

fn is_secret_shape(text: &str) -> bool {
    text.bytes().filter(u8::is_ascii_digit).count() >= 6
        || has_secret_prefix(text)
        || is_mac(text)
        || text.split(|c: char| !c.is_ascii_alphanumeric()).any(is_random_run)
}

fn has_drive(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.next() == Some(b':')
        && bytes.next().is_none_or(|b| b == b'\\' || b == b'/')
}

pub fn is_path_like(text: &str) -> bool {
    text.contains(['/', '\\']) || text.starts_with('~') || has_drive(text)
}

fn is_uuid(text: &str) -> bool {
    let mut groups = text.split('-');
    [8, 4, 4, 4, 12]
        .iter()
        .all(|&len| groups.next().is_some_and(|g| g.len() == len && all_hex(g)))
        && groups.next().is_none()
}

fn is_ipv4(text: &str) -> bool {
    let mut parts = text.split('.');
    (0..4).all(|_| {
        parts
            .next()
            .is_some_and(|p| all_digits(p) && p.parse::<u16>().is_ok_and(|n| n <= 255))
    }) && parts.next().is_none()
}

fn is_ipv6(text: &str) -> bool {
    text.matches(':').count() >= 2 && text.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
}

fn is_date(text: &str) -> bool {
    [[4, 2, 2], [2, 2, 4]].iter().any(|lens| {
        let mut parts = text.split('-');
        lens.iter()
            .all(|&len| parts.next().is_some_and(|p| p.len() == len && all_digits(p)))
            && parts.next().is_none()
    })
}

/// `21:26`, `9:26PM`, `21:26:25.573Z`.
fn is_clock(text: &str) -> bool {
    let text = text.trim_end_matches('Z');
    let text = ["AM", "PM", "am", "pm"]
        .iter()
        .find_map(|meridiem| text.strip_suffix(meridiem))
        .unwrap_or(text);
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    let mut parts = whole.split(':');
    let hour = parts
        .next()
        .is_some_and(|p| (1..=2).contains(&p.len()) && all_digits(p));
    let rest = parts.clone().count();
    hour && (1..=2).contains(&rest) && parts.all(|p| p.len() == 2 && all_digits(p)) && all_digits(fraction)
}

/// `2026-10-24`, `2026-10-24T21:26:25Z`, `21:26`, `21:26:25.573`.
fn is_time(text: &str) -> bool {
    match text.split_once('T') {
        Some((day, time)) => is_date(day) && is_clock(time),
        None => is_date(text) || is_clock(text),
    }
}

/// An IPv4 address written as one decimal integer, in the private and loopback blocks a home
/// network and a game's own socket use (`192.168.0.0/16`, `172.16.0.0/12`).
fn is_integer_ip(whole: &str) -> bool {
    whole.parse::<u32>().is_ok_and(|n| {
        (0xC0A8_0000..=0xC0A8_FFFF).contains(&n) || (0xAC10_0000..=0xAC1F_FFFF).contains(&n) || n == 0x7F00_0001
    })
}

/// `5`, `-2142896048`, `65.4ms`, `101.0%`, `#0`: at most ten digits in all and three after the
/// point, since a longer run is an account or profile id rather than a count or an error code; an
/// unsigned ten-digit one starting with 1 is a Unix time.
fn number(text: &str) -> Option<Result<(), Private>> {
    let unsigned = text.trim_start_matches(['+', '-', '#']);
    let split = unsigned
        .bytes()
        .position(|b| !b.is_ascii_digit() && b != b'.')
        .unwrap_or(unsigned.len());
    let (digits, suffix) = unsigned.split_at(split);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "0"));
    let unit = suffix.len() <= 4 && suffix.bytes().all(|b| b.is_ascii_alphabetic() || b == b'%');
    if !(all_digits(whole) && all_digits(fraction) && unit) {
        return None;
    }
    let fraction_len = digits.split_once('.').map_or(0, |(_, fraction)| fraction.len());
    Some(
        if whole.len() == 10 && whole.starts_with('1') && !text.starts_with('-') {
            Err(Private::Timestamp)
        } else if whole.len() + fraction_len > 10 || fraction_len > 3 {
            Err(Private::LongNumber)
        } else if fraction_len == 0 && is_integer_ip(whole) {
            Err(Private::IpAddress)
        } else {
            Ok(())
        },
    )
}

/// `0x8046000a`, `+0x5ef`: codes and offsets fit in 32 bits; wider values are addresses.
fn hex(text: &str) -> Option<Result<(), Private>> {
    let digits = text.trim_start_matches(['+', '-']).strip_prefix("0x")?;
    all_hex(digits).then_some(if digits.len() > 8 {
        Err(Private::Address)
    } else {
        Ok(())
    })
}

/// `0xC0A80101`: eight digits fill an IPv4 address, so outside an error code or an offset it is
/// taken for one.
pub fn is_wide_hex(text: &str) -> bool {
    text.trim_start_matches(['+', '-'])
        .strip_prefix("0x")
        .is_some_and(|digits| digits.len() == 8 && all_hex(digits))
}

/// `libpsport-gpu.dll`, `eboot.bin`: the file name of an executable image, without folders. Only
/// the names the port and the system libraries have, so a user's own file name never passes as one.
pub fn is_image(text: &str) -> bool {
    text.split_once('.').is_some_and(|(stem, extension)| {
        IMAGE_EXTENSIONS.contains(&extension)
            && IMAGE_PREFIXES
                .iter()
                .any(|prefix| starts_with_ignore_case(stem, prefix))
            && !stem.contains(':')
            && is_word(stem)
    })
}

/// `26.2.4`: up to three parts of four digits. A part with a leading zero is octal in an address, a
/// part of four digits in three is a year, and `192.168.1` is the start of an address.
fn dotted_numbers(text: &str) -> Result<(), Private> {
    let parts = text.split('.').count();
    let is_byte = |p: &str| p.parse::<u16>().is_ok_and(|n| n <= 255);
    let private_start = text
        .split('.')
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .is_some_and(|n| matches!(n, 10 | 127 | 169 | 172 | 192));
    if parts == 3 && text.split('.').any(|p| p.len() == 4) {
        Err(Private::Timestamp)
    } else if (parts == 4 && text.split('.').any(|p| p.len() > 1 && p.starts_with('0')))
        || (parts == 3 && text.starts_with('0') && !text.starts_with("0."))
        || (parts == 3 && private_start && text.split('.').all(is_byte))
    {
        Err(Private::IpAddress)
    } else if parts <= 3 && text.split('.').all(|p| p.len() <= 4) {
        Ok(())
    } else {
        Err(Private::HostName)
    }
}

/// `font.cpp`, `libpsport-gpu.dll+0x308914`, `Lib.Font`, `26.2.4`; any other dotted name is
/// taken for a host.
fn dotted(text: &str) -> Result<(), Private> {
    if let Some((image, offset)) = text.split_once('+') {
        return if is_image(image) && hex(offset).is_some_and(|h| h.is_ok()) {
            Ok(())
        } else {
            Err(Private::Unrecognized)
        };
    }
    if text.split('.').all(all_digits) {
        return dotted_numbers(text);
    }
    let source = text
        .split_once('.')
        .is_some_and(|(stem, extension)| is_word(stem) && SOURCE_EXTENSIONS.contains(&extension));
    let classes = text
        .split('.')
        .all(|p| p.bytes().next().is_some_and(|b| b.is_ascii_uppercase()) && is_word(p));
    if source || classes || is_image(text) {
        Ok(())
    } else if text.split('.').all(|p| !p.is_empty()) {
        Err(Private::HostName)
    } else {
        Err(Private::Unrecognized)
    }
}

/// Characters that draw nothing or reorder what is around them: format, line-separator, private
/// use and tag characters, and the full-width forms of ASCII, which spell an address or a mail
/// the ASCII rules would catch.
const fn is_hidden(c: char) -> bool {
    matches!(
        c as u32,
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x180E
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x206F
            | 0xE000..=0xF8FF
            | 0xFEFF
            | 0xFF01..=0xFF5E
            | 0xFFF9..=0xFFFB
            | 0xFFFE..=0xFFFF
            | 0xE_0000..=0xE_007F
            | 0xF_0000..=0xF_FFFD
            | 0x10_0000..=0x10_FFFD
    )
}

const fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// A word of a game's name: no hidden characters, at most two combining marks in a row, and its
/// ASCII characters (the non-ASCII letters dropped) pass as they would in any other mode.
fn name_token(token: &str, mode: Mode) -> Result<(), Private> {
    let mut marks = 0;
    for c in token.chars() {
        marks = if is_combining(c) { marks + 1 } else { 0 };
        if is_hidden(c) || marks > 2 {
            return Err(Private::NonAscii);
        }
    }
    let symbols_only = token
        .chars()
        .all(|c| c.is_ascii_punctuation() || NAME_SYMBOLS.contains(c));
    if !symbols_only && !token.chars().any(char::is_alphanumeric) {
        return Err(Private::NonAscii);
    }
    let ascii: String = token.chars().filter(char::is_ascii).collect();
    if ascii.is_empty() {
        Ok(())
    } else {
        classify_in(&ascii, mode, false)
    }
}

/// The shape of one token: `Ok` when it may stay as written.
pub fn classify(token: &str, mode: Mode) -> Result<(), Private> {
    classify_in(token, mode, false)
}

/// [`classify`] for a token that follows the word `code` or `offset`, where eight hex digits are
/// a code or an offset and not an address.
pub fn classify_in(token: &str, mode: Mode, after_code: bool) -> Result<(), Private> {
    let token = core(token);
    if token.is_empty() {
        return Ok(());
    }
    if token.chars().any(char::is_control) {
        return Err(Private::Unrecognized);
    }
    let lower = token.to_ascii_lowercase();
    if SCRIPT_SCHEMES.iter().any(|scheme| lower.contains(scheme)) {
        return Err(Private::Unrecognized);
    }
    if !token.is_ascii() {
        return if mode == Mode::Name && !token.contains(['/', '\\', '@', ':']) {
            name_token(token, mode)
        } else {
            Err(Private::NonAscii)
        };
    }
    if token.contains("://") {
        return Err(Private::Url);
    }
    if is_path_like(token) {
        let parts = token.split('/').count();
        if !token.split('/').all(|p| all_digits(p) && p.len() <= 4) || parts > 3 {
            return Err(Private::Path);
        }
        return if parts == 3 && token.split('/').any(|p| p.len() == 4) {
            Err(Private::Timestamp)
        } else if parts == 2 {
            Ok(())
        } else {
            Err(Private::Path)
        };
    }
    if token.contains('@') {
        return Err(Private::Email);
    }
    if is_uuid(token) {
        return Err(Private::Uuid);
    }
    if is_time(token) {
        return Err(Private::Timestamp);
    }
    if is_ipv4(token) || is_ipv6(token) {
        return Err(Private::IpAddress);
    }
    if token.contains(':') && !token.contains("::") {
        return token
            .split(':')
            .try_for_each(|part| classify_in(part, mode, after_code));
    }
    // A pointer printed zero-padded (`000000080593C000`), with or without letters in it.
    if token.len() == 16 && all_hex(token) {
        return Err(Private::Address);
    }
    if is_wide_hex(token) && !after_code {
        return Err(Private::Address);
    }
    if let Some(shape) = hex(token).or_else(|| number(token)) {
        return shape;
    }
    if token.len() >= 9 && all_hex(token) {
        return Err(Private::LongNumber);
    }
    if token.contains('.') {
        return dotted(token);
    }
    if is_word(token) || token.bytes().all(|b| b"+-*|&!?#%^.:".contains(&b)) {
        return Ok(());
    }
    Err(Private::Unrecognized)
}

/// `text` with every allowed `<tag>` kept and any other `<` or `>` turned into `_` (`keep`) or left
/// in (`!keep`), so the caller can tell whether markup was there. A `_` is not a separator, so
/// the `<user>` a log writes inside a path leaves that path one token for the normalizer.
fn without_markup(text: &str, keep: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['<', '>']) {
        let (before, tail) = rest.split_at(at);
        out.push_str(before);
        let tag = tail
            .strip_prefix('<')
            .and_then(|inner| inner.split_once('>'))
            .filter(|(name, _)| TAGS.contains(name));
        if let Some((name, after)) = tag {
            if keep {
                out.push('<');
                out.push_str(name);
                out.push('>');
            }
            rest = after;
        } else {
            out.push(if keep { '_' } else { tail.chars().next().unwrap_or(' ') });
            rest = tail.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

/// Whether `text` could be read as HTML or a Markdown link by whoever shows it: an angle bracket
/// outside the log levels and placeholders, or `](`.
pub fn has_markup(text: &str) -> bool {
    without_markup(text, false).contains(['<', '>']) || text.contains("](")
}

/// `text` without the markup [`has_markup`] finds.
pub fn strip_markup(text: &str) -> String {
    without_markup(text, true).replace("](", "] (")
}

/// A word of hardware or system naming that is capitalized without being a person's name.
pub fn is_label_word(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    OS_WORDS.contains(&lower.as_str()) || GPU_WORDS.contains(&lower.as_str())
}

/// What follows a GPU's brand: words of the hardware vocabulary, model names that carry a digit,
/// and acronyms (`RX 7900 XT (RADV NAVI31)`), so a person's name has nowhere to go.
pub fn is_gpu_model_tail(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric()).all(|word| {
        word.is_empty()
            || is_label_word(word)
            || word.bytes().any(|b| b.is_ascii_digit())
            || word.bytes().all(|b| b.is_ascii_uppercase())
    })
}

/// `3.8.1`, `11`, `560.94`, `32.0.101.5972`: up to four parts of five digits.
fn is_release_number(text: &str) -> bool {
    text.split('.').count() <= 4 && text.split('.').all(|part| all_digits(part) && part.len() <= 5)
}

/// `-rc.1`, `-beta`, `-dev`: a pre-release suffix, the only one a version may carry.
pub fn is_pre_release(text: &str) -> bool {
    let (name, number) = text.split_once('.').unwrap_or((text, "0"));
    matches!(name, "rc" | "alpha" | "beta" | "dev") && all_digits(number) && number.len() <= 5
}

/// `SteamOS 3.8.1`, `Windows 11 26100`, `Ubuntu 24.04 LTS`: system names from a closed list and
/// release numbers.
pub fn is_os_version(text: &str) -> bool {
    let mut words = text.split(' ');
    words.clone().count() <= 6
        && words
            .next()
            .is_some_and(|word| OS_WORDS.contains(&word.to_ascii_lowercase().as_str()))
        && words.clone().any(is_release_number)
        && words.all(|word| is_release_number(word) || OS_WORDS.contains(&word.to_ascii_lowercase().as_str()))
}

/// `radv 26.2.4`, `nvidia 560.94`: a driver from a closed list and its version.
pub fn is_driver(text: &str) -> bool {
    text.split_once(' ').is_some_and(|(name, version)| {
        let (number, pre) = version.split_once('-').map_or((version, None), |(n, p)| (n, Some(p)));
        DRIVER_NAMES.contains(&name.to_ascii_lowercase().as_str())
            && is_release_number(number)
            && pre.is_none_or(is_pre_release)
    })
}
