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

/// One line of `/proc/self/mountinfo`.
struct Mount {
    /// Absolute, as the kernel reports it.
    point: String,
    /// Whether the `ro` option is set.
    read_only: bool,
    /// The backing device or, for pseudo filesystems, its name.
    source: String,
}

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
    for mount in table.lines().filter_map(parse).filter(is_removable) {
        let path = PathBuf::from(mount.point);
        if path.is_dir() && !mounts.contains(&path) {
            mounts.push(path);
        }
    }
    mounts
}

/// Mountinfo is external data: a line that doesn't have the documented shape,
/// or a mount point that isn't absolute, is dropped.
fn parse(line: &str) -> Option<Mount> {
    let mut fields = line.split(' ');
    let point = unescape(fields.nth(4)?)?;
    let read_only = fields.next()?.split(',').any(|option| option == "ro");
    let source = fields.skip_while(|field| *field != "-").nth(2)?;
    point.starts_with('/').then_some(Mount {
        point,
        read_only,
        source: unescape(source)?,
    })
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
fn is_removable(mount: &Mount) -> bool {
    !mount.read_only
        && mount.source.starts_with("/dev/")
        && (mount.source.starts_with("/dev/mmcblk")
            || AUTOMOUNT_PARENTS.iter().any(|parent| {
                mount
                    .point
                    .strip_prefix(parent)
                    .is_some_and(|rest| rest.starts_with('/'))
            }))
}
