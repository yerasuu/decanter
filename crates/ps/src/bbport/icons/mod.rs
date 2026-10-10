//! Button icons that follow the gamepad layout: Bloodborne's PS4 glyphs
//! replaced, in its menu textures, by the pad button that reaches each
//! input under the layout in use.
//!
//! The game draws them from `menu/common.tpf.dcx`: whole 32x32 `KG_*`
//! textures for menus and key guides, and 26 px icons inside two atlases
//! for prompts in the world (ladders, lamps). decanter writes a copy of
//! that file into a mod of its own that only it manages (see
//! [`super::mods`]), rebuilt when the layout or the base file changes.

mod bc7;
mod dcx;
mod gnm;
mod tpf;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::pad::PadLayout;

/// Bumped whenever the drawing changes, so cached files are rebuilt.
const GENERATION: u32 = 2;

/// How the pad's own buttons are labelled, so the icons read like the pad
/// in the player's hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconStyle {
    /// Cross, circle, square, triangle; L1/R1, L2/R2.
    Ps,
    /// A B X Y faces; LB/RB, LT/RT.
    Abxy,
    /// B A Y X faces (A on the east button); L/R, ZL/ZR, + and -.
    Bayx,
}

impl IconStyle {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ps => "ps",
            Self::Abxy => "abxy",
            Self::Bayx => "bayx",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Ps, Self::Abxy, Self::Bayx]
            .into_iter()
            .find(|style| style.id() == id)
    }

    /// From an SDL gamepad: its USB vendor inside the GUID first (bytes
    /// 4-5, little-endian), its name second. Anything not known to label
    /// its buttons otherwise gets A B X Y, the most common.
    pub fn for_gamepad(guid: &str, name: &str) -> Self {
        let vendor = guid
            .get(8..12)
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            .map(u16::swap_bytes);
        let name = name.to_lowercase();
        match vendor {
            Some(0x054c) => Self::Ps,
            Some(0x057e) => Self::Bayx,
            _ if ["dualsense", "dualshock", "ps4", "ps5"]
                .iter()
                .any(|hint| name.contains(hint)) =>
            {
                Self::Ps
            }
            _ if ["nintendo", "pro controller", "joy-con"]
                .iter()
                .any(|hint| name.contains(hint)) =>
            {
                Self::Bayx
            }
            _ => Self::Abxy,
        }
    }
}

/// The game file's path inside a game or mod folder.
pub const COMMON_TPF: &str = "dvdroot_ps4/menu/common.tpf.dcx";

/// `(texture, input)`: menu glyphs, whole textures, each standing for one
/// PS4 input. Bloodborne names them the Japanese way: `KG_Cancel` is ✕.
const KEY_GUIDE: [(&str, &str); 13] = [
    ("KG_Cancel", "cross"),
    ("KG_OK", "circle"),
    ("KG_R_L", "square"),
    ("KG_R_U", "triangle"),
    ("KG_L1", "l1"),
    ("KG_R1", "r1"),
    ("KG_L2", "l2"),
    ("KG_R2", "r2"),
    ("KG_L3", "l3"),
    ("KG_R3", "r3"),
    ("KG_Start", "options"),
    ("KG_TP_L", "touchpad"),
    ("KG_TP_R", "touchpad"),
];

/// The analog sticks are no button and move with no layout.
const STICKS: [(&str, &str); 2] = [("KG_LS", "stick-l"), ("KG_RS", "stick-r")];

/// `(atlas, x, y, input, dimmed)`: the 26 px prompt icons, measured in
/// Bloodborne 1.09's `common.tpf`. The atlases are 1024 wide, BC7.
const PROMPTS: [(&str, usize, usize, &str, bool); 5] = [
    ("MENU_Common_00091", 661, 123, "circle", false),
    ("MENU_Common_00091", 694, 123, "cross", false),
    ("MENU_Common_00091", 726, 123, "l3", false),
    ("MENU_Common_00092", 663, 107, "triangle", false),
    ("MENU_Common_00092", 703, 107, "cross", true),
];

/// The originals have a soft glow past their edge; this much around each
/// prompt icon is cleared, rounded out to whole blocks.
const PROMPT_MARGIN: usize = 4;

/// The icon art, drawn by `assets/glyphs/generate.py`.
fn glyph(style: IconStyle, name: &str, size: u32) -> Option<&'static [u8]> {
    macro_rules! art {
        ($($name:literal),* $(,)?) => {
            match (style, name, size) {
                $(
                    (IconStyle::Abxy, $name, 32) => Some(include_bytes!(concat!("../../../assets/glyphs/abxy/", $name, "_32.png"))),
                    (IconStyle::Abxy, $name, 26) => Some(include_bytes!(concat!("../../../assets/glyphs/abxy/", $name, "_26.png"))),
                    (IconStyle::Ps, $name, 32) => Some(include_bytes!(concat!("../../../assets/glyphs/ps/", $name, "_32.png"))),
                    (IconStyle::Ps, $name, 26) => Some(include_bytes!(concat!("../../../assets/glyphs/ps/", $name, "_26.png"))),
                    (IconStyle::Bayx, $name, 32) => Some(include_bytes!(concat!("../../../assets/glyphs/bayx/", $name, "_32.png"))),
                    (IconStyle::Bayx, $name, 26) => Some(include_bytes!(concat!("../../../assets/glyphs/bayx/", $name, "_26.png"))),
                )*
                _ => None,
            }
        };
    }
    art!(
        "a",
        "b",
        "x",
        "y",
        "leftshoulder",
        "rightshoulder",
        "lefttrigger",
        "righttrigger",
        "leftstick",
        "rightstick",
        "misc1",
        "paddle1",
        "paddle2",
        "paddle3",
        "touchpad",
        "start",
        "back",
        "dpup",
        "dpdown",
        "dpleft",
        "dpright",
        "stick-l",
        "stick-r",
    )
}

fn decode(png: &[u8]) -> io::Result<image::RgbaImage> {
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map(|image| image.to_rgba8())
        .map_err(io::Error::other)
}

/// The art for `input`'s first button under `layout`; `None` for an
/// unbound input, which keeps its original icon.
fn art_for(style: IconStyle, layout: &PadLayout, input: &str, size: u32) -> Option<&'static [u8]> {
    let button = layout
        .buttons(input)
        .split(',')
        .next()
        .filter(|button| !button.is_empty())?;
    glyph(style, button, size)
}

fn encode(
    image: &image::RgbaImage,
    x0: usize,
    y0: usize,
    blocks_wide: usize,
    blocks_high: usize,
    linear: &mut [u8],
    stride: usize,
) {
    for by in 0..blocks_high {
        for bx in 0..blocks_wide {
            let pixels: [bc7::Pixel; 16] = std::array::from_fn(|i| {
                let (x, y) = ((x0 + bx) * 4 + i % 4, (y0 + by) * 4 + i / 4);
                image.get_pixel(x as u32, y as u32).0
            });
            let at = ((y0 + by) * stride + x0 + bx) * 16;
            linear[at..at + 16].copy_from_slice(&bc7::encode_block(&pixels));
        }
    }
}

/// `tpf` with every slot it knows redrawn for `layout`. A texture whose
/// size or format isn't the expected one (another game version) is left
/// as it is.
fn redraw(tpf: &mut [u8], layout: &PadLayout, style: IconStyle) -> io::Result<usize> {
    let textures = tpf::textures(tpf)?;
    let find = |name: &str| textures.iter().find(|texture| texture.name == name);
    let mut redrawn = 0;

    let whole = KEY_GUIDE
        .iter()
        .filter_map(|&(texture, input)| Some((texture, art_for(style, layout, input, 32)?)))
        .chain(
            STICKS
                .iter()
                .filter_map(|&(texture, art)| Some((texture, glyph(style, art, 32)?))),
        );
    for (name, art) in whole {
        let Some(texture) = find(name).filter(|t| (t.width, t.height, t.dxgi, t.size) == (32, 32, 98, 1024)) else {
            tracing::warn!(texture = name, "key guide texture not as expected; left alone");
            continue;
        };
        let image = decode(art)?;
        let mut linear = vec![0; texture.size];
        encode(&image, 0, 0, 8, 8, &mut linear, 8);
        tpf[texture.offset..texture.offset + texture.size].copy_from_slice(&gnm::tile(&linear, 32, 32, 16));
        redrawn += 1;
    }

    for atlas in ["MENU_Common_00091", "MENU_Common_00092"] {
        let Some(texture) = find(atlas).filter(|t| t.width == 1024 && t.dxgi == 98) else {
            tracing::warn!(texture = atlas, "prompt atlas not as expected; left alone");
            continue;
        };
        let stride = texture.width / 4;
        let mut linear = gnm::detile(
            &tpf[texture.offset..texture.offset + texture.size],
            texture.width,
            texture.height,
            16,
        );
        for &(_, x, y, input, dimmed) in PROMPTS.iter().filter(|prompt| prompt.0 == atlas) {
            let Some(art) = art_for(style, layout, input, 26) else {
                continue;
            };
            let mut icon = decode(art)?;
            if dimmed {
                for pixel in icon.pixels_mut() {
                    pixel.0[3] = (u16::from(pixel.0[3]) * 3 / 5) as u8;
                }
            }
            // Whole blocks around the icon, cleared and drawn afresh, so no
            // block mixes old and new pixels.
            let (bx0, by0) = ((x - PROMPT_MARGIN) / 4, (y - PROMPT_MARGIN) / 4);
            let (bx1, by1) = (
                (x + 26 + PROMPT_MARGIN).div_ceil(4),
                (y + 26 + PROMPT_MARGIN).div_ceil(4),
            );
            let mut area = image::RgbaImage::new(1024, texture.height as u32);
            image::imageops::overlay(&mut area, &icon, x as i64, y as i64);
            encode(&area, bx0, by0, bx1 - bx0, by1 - by0, &mut linear, stride);
            redrawn += 1;
        }
        let tiled = gnm::tile(&linear, texture.width, texture.height, 16);
        tpf[texture.offset..texture.offset + texture.size].copy_from_slice(&tiled);
    }
    Ok(redrawn)
}

/// The mod folder holding `common.tpf.dcx` redrawn for `layout` from
/// `base` (the game's file, or the one an enabled mod replaces it with).
/// Built once per layout and base; older builds are removed. File I/O and
/// some CPU: off the UI thread.
pub fn prepare(layout: &PadLayout, style: IconStyle, base: &Path) -> io::Result<PathBuf> {
    let meta = fs::metadata(base)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs());
    let key = format!(
        "{:x}",
        md5::compute(format!(
            "{GENERATION}|{}|{}|{modified}|{:?}",
            base.display(),
            meta.len(),
            layout.lines
        ))
    );
    let root = crate::paths::bbport_data().join("icons");
    let dir = root.join(&key);
    let file = dir.join(COMMON_TPF);
    if fs::metadata(&file).is_ok_and(|meta| meta.is_file()) {
        return Ok(dir);
    }
    let mut dcx = dcx::Dcx::parse(&fs::read(base)?)?;
    let redrawn = redraw(dcx.data_mut(), layout, style)?;
    let bytes = dcx.write()?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::files::write_atomic(&file, &bytes)?;
    tracing::info!(redrawn, style = style.id(), dir = %dir.display(), "built button icons for the gamepad layout");
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.file_name() != key.as_str() {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_a_layout_can_use_has_art_at_both_sizes() {
        for style in [IconStyle::Ps, IconStyle::Abxy, IconStyle::Bayx] {
            for (button, _) in super::super::pad::BUTTONS {
                for size in [32, 26] {
                    let art = glyph(style, button, size).unwrap_or_else(|| panic!("no {style:?} {button} at {size}"));
                    let image = decode(art).unwrap();
                    assert_eq!(image.dimensions(), (size, size));
                }
            }
        }
    }

    #[test]
    fn the_layout_picks_each_icon() {
        let [default, code_vein] = PadLayout::builtins();
        let abxy = IconStyle::Abxy;
        assert_eq!(art_for(abxy, &default, "cross", 32), glyph(abxy, "a", 32));
        assert_eq!(art_for(abxy, &code_vein, "r1", 32), glyph(abxy, "x", 32));
        assert_eq!(
            art_for(IconStyle::Ps, &default, "touchpad", 26),
            glyph(IconStyle::Ps, "back", 26)
        );
    }

    #[test]
    fn the_pad_decides_the_style() {
        // SDL GUIDs: bus, CRC, then the vendor little-endian.
        assert_eq!(
            IconStyle::for_gamepad("030000004c050000e60c000000016800", "PS5 Controller"),
            IconStyle::Ps
        );
        assert_eq!(
            IconStyle::for_gamepad("030000007e0500000920000000026800", "Pro Controller"),
            IconStyle::Bayx
        );
        assert_eq!(
            IconStyle::for_gamepad("03000000c82d00000b31000000010000", "8BitDo Ultimate 2"),
            IconStyle::Abxy
        );
        assert_eq!(IconStyle::for_gamepad("", "Wireless DualSense"), IconStyle::Ps);
    }

    /// Against the real file, when a dump is around: `BB_TEST_COMMON_TPF`.
    #[test]
    #[ignore = "needs Bloodborne's menu/common.tpf.dcx"]
    fn redraws_the_real_file() {
        let path = std::env::var_os("BB_TEST_COMMON_TPF").expect("BB_TEST_COMMON_TPF");
        let mut dcx = dcx::Dcx::parse(&fs::read(path).unwrap()).unwrap();
        let [default, _] = PadLayout::builtins();
        assert_eq!(redraw(dcx.data_mut(), &default, IconStyle::Abxy).unwrap(), 15 + 5);
        let back = dcx::Dcx::parse(&dcx.write().unwrap()).unwrap();
        let texture = tpf::textures(back.data())
            .unwrap()
            .into_iter()
            .find(|t| t.name == "KG_Cancel")
            .unwrap();
        let linear = gnm::detile(&back.data()[texture.offset..texture.offset + 1024], 32, 32, 16);
        let first: [u8; 16] = linear[..16].try_into().unwrap();
        assert_eq!(
            bc7::decode_mode6(&first)[0][3],
            0,
            "the corner of a round icon is clear"
        );
        // For a look by eye: the Code Vein layout's file, when asked for.
        if let Some(out) = std::env::var_os("BB_TEST_ICONS_OUT") {
            let source = std::env::var_os("BB_TEST_COMMON_TPF").expect("BB_TEST_COMMON_TPF");
            let mut dcx = dcx::Dcx::parse(&fs::read(source).unwrap()).unwrap();
            let [_, code_vein] = PadLayout::builtins();
            redraw(dcx.data_mut(), &code_vein, IconStyle::Ps).unwrap();
            fs::write(out, dcx.write().unwrap()).unwrap();
        }
    }
}
