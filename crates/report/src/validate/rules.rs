//! What each field may hold. The predicates are public to the crate so the builder keeps an
//! optional value only when it fits, rather than producing a report this check then refuses.

use super::{Rejection, Rule, text_rejections};
use crate::schema::{
    Build, Calls, DecanterReport, Failure, FailureKind, Fault, Game, Gpu, GpuVendor, Perf, Product, PsportReport,
    Report, SCHEMA, Setting, System,
};
use crate::text::{Mode, has_secret_prefix, is_driver, is_gpu_model_tail, is_image, is_os_version, is_pre_release};

pub const MAX_MESSAGE: usize = 300;
pub const MAX_CALLS: usize = 64;
pub const MAX_FRAMES: usize = 32;
pub const MAX_COUNT: u32 = 1_000_000;
pub const MAX_CALL_COUNT: u64 = 1_000_000_000;

fn digits(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_digit())
}

/// `YYYY-MM-DD` with a real month and day, the only form of time a report keeps.
pub fn is_day(text: &str) -> bool {
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && year.parse().is_ok_and(|y: u16| (2020..2200).contains(&y))
        && month.parse().is_ok_and(|m: u8| (1..=12).contains(&m))
        && day
            .parse()
            .is_ok_and(|d: u8| (1..=days_in_month(year, month)).contains(&d))
}

fn days_in_month(year: &str, month: &str) -> u8 {
    let leap = year
        .parse::<u16>()
        .is_ok_and(|y| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0));
    match month {
        "02" if leap => 29,
        "02" => 28,
        "04" | "06" | "09" | "11" => 30,
        _ => 31,
    }
}

/// `CUSA03173`, `PPSA01234`: four capitals and five digits, the id `param.sfo` gives a title.
pub fn is_title_id(text: &str) -> bool {
    text.len() == 9
        && text.bytes().take(4).all(|b| b.is_ascii_uppercase())
        && text.get(4..).is_some_and(|d| digits(d, 5))
}

/// `01.04`, the `APP_VER`/`VERSION` form of `param.sfo`.
pub fn is_game_version(text: &str) -> bool {
    text.split_once('.')
        .is_some_and(|(major, minor)| digits(major, 2) && digits(minor, 2))
}

pub fn is_commit(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `0.3.4`, `0.1.4-rc.1`.
pub fn is_version(text: &str) -> bool {
    let (core, pre) = text
        .split_once('-')
        .map_or((text, None), |(core, pre)| (core, Some(pre)));
    let mut parts = core.split('.');
    text.len() <= 32
        && (0..3).all(|_| {
            parts
                .next()
                .is_some_and(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
        })
        && parts.next().is_none()
        && pre.is_none_or(is_pre_release)
}

/// A C symbol or an Itanium-mangled one (`sceKernelOpen`, `_ZN11PsFrameRate10IsUnlockedEv`).
pub fn is_symbol(text: &str, max: usize) -> bool {
    text.len() <= max
        && !has_secret_prefix(text)
        && text
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether the GPU's model starts with its own vendor's brand, as Vulkan and DXGI word it.
pub fn is_gpu_model(vendor: GpuVendor, model: &str) -> bool {
    let brand = match vendor {
        GpuVendor::Amd => "AMD ",
        GpuVendor::Nvidia => "NVIDIA ",
        GpuVendor::Intel => "Intel",
        GpuVendor::Apple => "Apple ",
    };
    model.len() <= 96 && model.strip_prefix(brand).is_some_and(is_gpu_model_tail)
}

/// The values each setting takes; a free-form value has no place here.
pub fn is_setting_value(setting: Setting, value: &str) -> bool {
    match setting {
        Setting::FrameStats
        | Setting::Fullscreen
        | Setting::GpuProfile
        | Setting::Hdr
        | Setting::ModsEnabled
        | Setting::Online
        | Setting::PcModel
        | Setting::Permissive
        | Setting::Prebuilt
        | Setting::SkipGameCheck
        | Setting::VkValidation => matches!(value, "0" | "1"),
        Setting::AspectRatio => {
            let side = |side: &str| (1..=2).contains(&side.len()) && side.bytes().all(|b| b.is_ascii_digit());
            value
                .split_once(':')
                .is_some_and(|(width, height)| side(width) && side(height))
        }
        Setting::FpsMode => matches!(value, "uncap" | "native" | "vsync" | "limit" | "30" | "60"),
        Setting::FpsLimit => value.parse::<u16>().is_ok_and(|fps| fps <= 1000) && value.len() <= 4,
        Setting::Language => value.parse::<u8>().is_ok_and(|language| language <= 30) && value.len() <= 2,
        Setting::MenuLanguage => {
            let (language, region) = value.split_once('-').unwrap_or((value, "XX"));
            language.len() == 2
                && language.bytes().all(|b| b.is_ascii_lowercase())
                && region.len() == 2
                && region.bytes().all(|b| b.is_ascii_uppercase())
        }
        Setting::PresentMode => matches!(value, "Mailbox" | "Fifo" | "FifoRelaxed" | "Immediate"),
    }
}

/// Perf values keep one decimal: a longer fraction could carry a number of someone's choosing.
pub fn round_perf(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

pub fn is_perf_value(value: f64) -> bool {
    value.is_finite() && (0.0..=10_000.0).contains(&value) && value.to_bits() == round_perf(value).to_bits()
}

/// Every field rule over a report that already has the schema's shape.
///
/// # Errors
///
/// All the rejections found, each naming its field.
pub fn check(report: &Report) -> Result<(), Vec<Rejection>> {
    let mut out = Vec::new();
    match report {
        Report::Decanter(report) => decanter(report, &mut out),
        Report::Psport(report) => psport(report, &mut out),
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

fn decanter(report: &DecanterReport, out: &mut Vec<Rejection>) {
    common(report.schema, &report.date, out);
    if report.product != Product::Decanter {
        out.push(Rejection::new("product", Rule::Product));
    }
    build("decanter", &report.decanter, out);
    system(&report.system, out);
    failure(&report.failure, out);
    if matches!(report.failure.kind, FailureKind::Stop | FailureKind::Missing) {
        out.push(Rejection::new("failure.kind", Rule::Product));
    }
}

fn psport(report: &PsportReport, out: &mut Vec<Rejection>) {
    common(report.schema, &report.date, out);
    if report.product != Product::Psport {
        out.push(Rejection::new("product", Rule::Product));
    }
    build("decanter", &report.decanter, out);
    build("psport", &report.psport, out);
    system(&report.system, out);
    game(&report.game, out);
    for (&setting, value) in &report.settings {
        if !is_setting_value(setting, value) {
            let key = serde_json::to_value(setting).ok();
            let name = key.as_ref().and_then(serde_json::Value::as_str).unwrap_or_default();
            out.push(Rejection::new(format!("settings.{name}"), Rule::Pattern));
        }
    }
    failure(&report.failure, out);
    if let Some(calls) = &report.calls {
        self::calls(calls, out);
    }
    if let Some(fault) = &report.fault {
        self::fault(fault, out);
    }
    if let Some(perf) = &report.perf {
        self::perf(perf, out);
    }
}

fn common(schema: u32, date: &str, out: &mut Vec<Rejection>) {
    if schema != SCHEMA {
        out.push(Rejection::new("schema", Rule::Schema));
    }
    if !is_day(date) {
        out.push(Rejection::new("date", Rule::Pattern));
    }
}

fn build(path: &str, build: &Build, out: &mut Vec<Rejection>) {
    if build.version.as_deref().is_some_and(|v| !is_version(v)) {
        out.push(Rejection::new(format!("{path}.version"), Rule::Pattern));
    }
    if build.commit.as_deref().is_some_and(|c| !is_commit(c)) {
        out.push(Rejection::new(format!("{path}.commit"), Rule::Pattern));
    }
}

fn bounded(path: &str, value: &str, max: usize, mode: Mode, out: &mut Vec<Rejection>) {
    if value.len() > max {
        out.push(Rejection::new(path, Rule::TooLong));
    } else if value.is_empty() {
        out.push(Rejection::new(path, Rule::Pattern));
    } else {
        text_rejections(path, value, mode, out);
    }
}

/// A label made of words from a closed list and release numbers: the table is the whole rule.
fn closed(path: &str, value: &str, fits: fn(&str) -> bool, out: &mut Vec<Rejection>) {
    if value.len() > 48 {
        out.push(Rejection::new(path, Rule::TooLong));
    } else if !fits(value) {
        out.push(Rejection::new(path, Rule::Pattern));
    }
}

fn system(system: &System, out: &mut Vec<Rejection>) {
    if let Some(version) = &system.os_version {
        closed("system.os_version", version, is_os_version, out);
    }
    if let Some(gpu) = &system.gpu {
        self::gpu(gpu, out);
    }
}

fn gpu(gpu: &Gpu, out: &mut Vec<Rejection>) {
    bounded("system.gpu.model", &gpu.model, 96, Mode::Label, out);
    if !is_gpu_model(gpu.vendor, &gpu.model) {
        out.push(Rejection::new("system.gpu.model", Rule::Pattern));
    }
    if let Some(driver) = &gpu.driver {
        closed("system.gpu.driver", driver, is_driver, out);
    }
}

fn game(game: &Game, out: &mut Vec<Rejection>) {
    if !is_title_id(&game.title_id) {
        out.push(Rejection::new("game.title_id", Rule::Pattern));
    }
    if let Some(name) = &game.name {
        bounded("game.name", name, 128, Mode::Name, out);
    }
    if game.version.as_deref().is_some_and(|v| !is_game_version(v)) {
        out.push(Rejection::new("game.version", Rule::Pattern));
    }
}

fn failure(failure: &Failure, out: &mut Vec<Rejection>) {
    bounded("failure.message", &failure.message, MAX_MESSAGE, Mode::Message, out);
    if !(1..=MAX_COUNT).contains(&failure.count) {
        out.push(Rejection::new("failure.count", Rule::OutOfRange));
    }
}

fn calls(calls: &Calls, out: &mut Vec<Rejection>) {
    for (list, entries) in [
        ("unimplemented", &calls.unimplemented),
        ("stubs", &calls.stubs),
        ("failed", &calls.failed),
        ("status", &calls.status),
    ] {
        if entries.len() > MAX_CALLS {
            out.push(Rejection::new(format!("calls.{list}"), Rule::TooMany));
        }
        for (index, call) in entries.iter().enumerate() {
            if !is_symbol(&call.name, 96) {
                out.push(Rejection::new(format!("calls.{list}[{index}].name"), Rule::Pattern));
            }
            if !(1..=MAX_CALL_COUNT).contains(&call.count) {
                out.push(Rejection::new(format!("calls.{list}[{index}].count"), Rule::OutOfRange));
            }
        }
    }
}

fn fault(fault: &Fault, out: &mut Vec<Rejection>) {
    if !(1..=64).contains(&fault.signal) {
        out.push(Rejection::new("fault.signal", Rule::OutOfRange));
    }
    if fault.frames.len() > MAX_FRAMES {
        out.push(Rejection::new("fault.frames", Rule::TooMany));
    }
    for (index, frame) in fault.frames.iter().enumerate() {
        if frame.module.as_deref().is_some_and(|m| m.len() > 64 || !is_image(m)) {
            out.push(Rejection::new(format!("fault.frames[{index}].module"), Rule::Pattern));
        }
        if frame.symbol.as_deref().is_some_and(|s| !is_symbol(s, 128)) {
            out.push(Rejection::new(format!("fault.frames[{index}].symbol"), Rule::Pattern));
        }
        // An image is far smaller than 4 GiB; a larger offset is an absolute address.
        if u32::try_from(frame.offset).is_err() {
            out.push(Rejection::new(
                format!("fault.frames[{index}].offset"),
                Rule::OutOfRange,
            ));
        }
    }
}

fn perf(perf: &Perf, out: &mut Vec<Rejection>) {
    if !(1..=100_000).contains(&perf.samples) {
        out.push(Rejection::new("perf.samples", Rule::OutOfRange));
    }
    for (name, value) in [
        ("fps_mean", perf.fps_mean),
        ("fps_min", perf.fps_min),
        ("worst_frame_ms", perf.worst_frame_ms),
    ] {
        if !is_perf_value(value) {
            out.push(Rejection::new(format!("perf.{name}"), Rule::OutOfRange));
        }
    }
}
