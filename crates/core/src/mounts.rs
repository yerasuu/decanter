//! Removable drives and SD cards that are mounted right now, offered as
//! places to put a library.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "the iterator chains over a mountinfo line cost more than the 512-byte threshold"
)]

use std::fs;
use std::path::PathBuf;

/// Where udisks and its desktop front ends mount removable media.
const AUTOMOUNT_PARENTS: [&str; 2] = ["/run/media", "/media"];

/// Writable block-device mounts that look removable: under the automount
/// folders, or backed by an `mmcblk` card. Empty where `/proc/self/mountinfo`
/// doesn't exist.
#[must_use]
#[inline]
pub fn removable_mounts() -> Vec<PathBuf> {
    let Ok(table) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let mut mounts = Vec::new();
    for point in table.lines().filter_map(parse) {
        let path = PathBuf::from(point);
        if path.is_dir() && !mounts.contains(&path) {
            mounts.push(path);
        }
    }
    mounts
}

/// Mountinfo is external data: a line that doesn't have the documented shape,
/// or a mount point that isn't absolute, is dropped. The prefixes are tested on
/// the raw fields (the escapes only replace space, tab, newline and backslash)
/// so that only removable candidates are unescaped and allocated.
fn parse(line: &str) -> Option<String> {
    let mut fields = line.split(' ');
    let raw_point = fields.nth(4)?;
    let read_only = fields.next()?.split(',').any(|option| option == "ro");
    let raw_source = fields.skip_while(|field| *field != "-").nth(2)?;
    if read_only || !is_removable(raw_point, raw_source) {
        return None;
    }
    let point = unescape(raw_point)?;
    unescape(raw_source)?;
    point.starts_with('/').then_some(point)
}

/// Mountinfo writes space, tab, newline and backslash as `\040`, `\011`,
/// `\012` and `\134`.
fn unescape(field: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(field.len());
    let mut rest = field.bytes();
    while let Some(byte) = rest.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        let digits: Vec<u8> = rest.by_ref().take(3).collect();
        let text = std::str::from_utf8(&digits).ok()?;
        bytes.push(u8::from_str_radix(text, 8).ok()?);
    }
    String::from_utf8(bytes).ok()
}

/// Udisks puts its mounts under `/run/media` and `/media`; an SD card is
/// `mmcblk` wherever it is mounted.
fn is_removable(point: &str, source: &str) -> bool {
    source.starts_with("/dev/")
        && (source.starts_with("/dev/mmcblk")
            || AUTOMOUNT_PARENTS
                .iter()
                .any(|parent| point.strip_prefix(parent).is_some_and(|rest| rest.starts_with('/'))))
}
