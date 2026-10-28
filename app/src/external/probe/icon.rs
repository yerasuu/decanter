//! Icons and images read from a game's files, checked before anything
//! decodes them: dimensions come from the header first, decoding runs
//! under [`image::Limits`], and what is kept is a PNG or JPEG file.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits};

use super::Image;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const GROUP_ENTRY_SIZE: usize = 14;
/// Windows draws nothing larger than 256 px from an icon group.
const MAX_ICON_SIDE: u32 = 256;
/// A cover or background shipped beside a game is a few thousand pixels
/// across at most.
const MAX_IMAGE_SIDE: u32 = 4096;
/// What decoding one icon may allocate.
const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;
/// Entries looked at in one icon group.
const MAX_GROUP_ENTRIES: usize = 64;

/// One image of an icon group (`GRPICONDIRENTRY`), as the group lists it.
pub(super) struct GroupEntry {
    pub(super) id: u16,
    /// The entry's first 8 bytes (size, colours, planes, depth), which an
    /// `.ico` file's directory entry repeats before its own size and offset.
    raw: [u8; 8],
    side: u32,
    bit_count: u16,
}

/// The largest image of the group up to 256 px, the deepest colour among
/// those of one size.
pub(super) fn best_in_group(group: &[u8]) -> Option<GroupEntry> {
    if super::source::u16_le(group, 2)? != 1 {
        return None;
    }
    let count = usize::from(super::source::u16_le(group, 4)?).min(MAX_GROUP_ENTRIES);
    group
        .get(6..)?
        .as_chunks::<GROUP_ENTRY_SIZE>()
        .0
        .iter()
        .take(count)
        .filter_map(|entry| {
            let raw: [u8; 8] = entry.get(..8)?.try_into().ok()?;
            // 0 stands for 256 in the one byte the format gives a side.
            let side = match entry.first().copied()? {
                0 => MAX_ICON_SIDE,
                side => u32::from(side),
            };
            Some(GroupEntry {
                id: super::source::u16_le(entry, 12)?,
                raw,
                side,
                bit_count: super::source::u16_le(entry, 6)?,
            })
        })
        .max_by_key(|entry| (entry.side, entry.bit_count))
}

/// An `RT_ICON` image as a PNG: one already stored as PNG as it is, a DIB
/// wrapped in a one-image `.ico` and decoded.
pub(super) fn to_png(image: &[u8], entry: &GroupEntry) -> Option<Vec<u8>> {
    if image.starts_with(PNG_SIGNATURE) {
        let (width, height) = dimensions(image, ImageFormat::Png)?;
        return (width <= MAX_ICON_SIDE && height <= MAX_ICON_SIDE).then(|| image.to_vec());
    }
    let size = u32::try_from(image.len()).ok()?;
    let mut ico = Vec::with_capacity(image.len().checked_add(22)?);
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&entry.raw);
    ico.extend_from_slice(&size.to_le_bytes());
    ico.extend_from_slice(&22_u32.to_le_bytes());
    ico.extend_from_slice(image);
    decode_to_png(&ico, ImageFormat::Ico, MAX_ICON_SIDE)
}

/// An image file found beside a game, kept as it is when it is a PNG or
/// JPEG of a sane size, converted when it is an `.ico`.
pub(super) fn checked(bytes: Vec<u8>) -> Option<Image> {
    let format = image::guess_format(&bytes).ok()?;
    match format {
        ImageFormat::Png | ImageFormat::Jpeg => {
            let (width, height) = dimensions(&bytes, format)?;
            (width <= MAX_IMAGE_SIDE && height <= MAX_IMAGE_SIDE).then(|| Image {
                extension: if format == ImageFormat::Png { "png" } else { "jpg" },
                bytes,
            })
        }
        ImageFormat::Ico => decode_to_png(&bytes, format, MAX_ICON_SIDE).map(|bytes| Image {
            bytes,
            extension: "png",
        }),
        _ => None,
    }
}

fn limits(side: u32) -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

fn dimensions(bytes: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .ok()
}

fn decode_to_png(bytes: &[u8], format: ImageFormat, side: u32) -> Option<Vec<u8>> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits(side));
    let decoded = reader
        .decode()
        .inspect_err(|err| tracing::debug!(%err, "embedded image did not decode"))
        .ok()?;
    let mut png = Cursor::new(Vec::new());
    decoded
        .write_to(&mut png, ImageFormat::Png)
        .inspect_err(|err| tracing::debug!(%err, "embedded image did not encode as PNG"))
        .ok()?;
    Some(png.into_inner())
}
