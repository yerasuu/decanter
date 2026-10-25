//! The shapes a token of a failure line may have, and the private ones it may not.

use super::{Mode, Private, core};

const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "h", "hpp", "inc", "rs", "py", "sh", "json", "ini", "toml", "cfg",
];
const IMAGE_EXTENSIONS: &[&str] = &["dll", "so", "prx", "sprx", "exe", "elf", "bin", "self", "dylib"];

fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn all_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `sceKernelOpen`, `image-relative`, `Vulkan::Instance`.
pub fn is_word(text: &str) -> bool {
    text.split("::").all(|part| {
        part.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'~')
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'~'))
    })
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
    let mut parts = text.split('-');
    [4, 2, 2]
        .iter()
        .all(|&len| parts.next().is_some_and(|p| p.len() == len && all_digits(p)))
        && parts.next().is_none()
}

fn is_clock(text: &str) -> bool {
    let text = text.trim_end_matches('Z');
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    let count = whole.split(':').count();
    (2..=3).contains(&count) && whole.split(':').all(|p| p.len() == 2 && all_digits(p)) && all_digits(fraction)
}

/// `2026-10-24`, `2026-10-24T21:26:25Z`, `21:26`, `21:26:25.573`.
fn is_time(text: &str) -> bool {
    match text.split_once('T') {
        Some((day, time)) => is_date(day) && is_clock(time),
        None => is_date(text) || is_clock(text),
    }
}

/// `5`, `-2142896048`, `65.4ms`, `101.0%`, `#0`: at most ten digits, since a longer run is an
/// account or profile id rather than a count or an error code.
fn number(text: &str) -> Option<Result<(), Private>> {
    let unsigned = text.trim_start_matches(['+', '-', '#']);
    let split = unsigned
        .bytes()
        .position(|b| !b.is_ascii_digit() && b != b'.')
        .unwrap_or(unsigned.len());
    let (digits, suffix) = unsigned.split_at(split);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "0"));
    let unit = suffix.len() <= 4 && suffix.bytes().all(|b| b.is_ascii_alphabetic() || b == b'%');
    (all_digits(whole) && all_digits(fraction) && unit).then_some(if whole.len() > 10 {
        Err(Private::LongNumber)
    } else {
        Ok(())
    })
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

/// `libpsport-gpu.dll`, `eboot.bin`: the file name of an executable image, without folders.
pub fn is_image(text: &str) -> bool {
    text.rsplit_once('.').is_some_and(|(stem, extension)| {
        IMAGE_EXTENSIONS.contains(&extension)
            && !stem.is_empty()
            && stem
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    })
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
    if text.split('.').all(all_digits) && text.split('.').count() <= 4 {
        return Ok(());
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

/// The shape of one token: `Ok` when it may stay as written.
pub fn classify(token: &str, mode: Mode) -> Result<(), Private> {
    let token = core(token);
    if token.is_empty() {
        return Ok(());
    }
    if token.chars().any(char::is_control) {
        return Err(Private::Unrecognized);
    }
    if !token.is_ascii() {
        return if mode == Mode::Name && !token.contains(['/', '\\', '@', ':']) {
            Ok(())
        } else {
            Err(Private::NonAscii)
        };
    }
    if token.contains("://") {
        return Err(Private::Url);
    }
    if is_path_like(token) {
        let fraction = token.split('/').all(|p| all_digits(p) && p.len() <= 4);
        return if fraction { Ok(()) } else { Err(Private::Path) };
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
        return token.split(':').try_for_each(|part| classify(part, mode));
    }
    // A pointer printed zero-padded (`000000080593C000`), with or without letters in it.
    if token.len() == 16 && all_hex(token) {
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
