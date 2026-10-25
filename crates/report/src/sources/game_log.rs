//! A psport game log.
//!
//! psport's own (`bbport/logs/*.log`) or decanter's run log of a game (`psport/logs/*-game-*.log`),
//! which adds a `# key: value` header. Only the lines a report draws from are read, each by its
//! exact psport form; every other line (displays, gamepads, the window, paths, arguments) is
//! never looked at.

use std::collections::BTreeMap;

use crate::schema::{Call, CallCode, Calls, FailureKind, Fault, FaultSide, Frame, Gpu, GpuVendor, Os, Perf, Setting};
use crate::validate::rules::{MAX_CALLS, MAX_FRAMES, is_title_id};

/// What a log says, raw: [`crate::build`] fits each value to its field.
#[derive(Debug, Default)]
pub struct GameLog {
    pub decanter_version: Option<String>,
    pub decanter_commit: Option<String>,
    pub psport_version: Option<String>,
    pub psport_commit: Option<String>,
    pub title_id: Option<String>,
    pub game_version: Option<String>,
    /// `YYYY-MM-DD` of the header's `started:`.
    pub day: Option<String>,
    pub game_name: Option<String>,
    pub profile: bool,
    pub os: Option<Os>,
    pub gpu: Option<Gpu>,
    pub settings: BTreeMap<Setting, String>,
    pub calls: Option<Calls>,
    /// The first fault, with its line for the message.
    pub fault: Option<(String, Fault)>,
    /// The first `STOP:` or error line and how many times it repeated.
    pub error: Option<(FailureKind, String, u32)>,
    pub perf: Option<Perf>,
}

const SETTINGS: &[(&str, Setting)] = &[
    ("PS_ASPECT_RATIO", Setting::AspectRatio),
    ("PS_FPS", Setting::FpsMode),
    ("PS_FPS_LIMIT", Setting::FpsLimit),
    ("PS_FRAME_STATS", Setting::FrameStats),
    ("PS_FULLSCREEN", Setting::Fullscreen),
    ("PS_GPU_PROFILE", Setting::GpuProfile),
    ("PS_HDR", Setting::Hdr),
    ("PS_LANGUAGE", Setting::Language),
    ("PS_MENU_LANGUAGE", Setting::MenuLanguage),
    ("PS_MODS_ENABLED", Setting::ModsEnabled),
    ("PS_ONLINE", Setting::Online),
    ("PS_PC_MODEL", Setting::PcModel),
    ("PS_PERMISSIVE", Setting::Permissive),
    ("PS_PREBUILT", Setting::Prebuilt),
    ("PS_PRESENT_MODE", Setting::PresentMode),
    ("PS_SKIP_GAME_CHECK", Setting::SkipGameCheck),
    ("PS_VK_VALIDATION", Setting::VkValidation),
];

struct FrameStats {
    samples: u32,
    fps_sum: f64,
    fps_min: f64,
    worst_ms: f64,
}

impl GameLog {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut log = Self::default();
        let mut stats = FrameStats {
            samples: 0,
            fps_sum: 0.0,
            fps_min: f64::MAX,
            worst_ms: 0.0,
        };
        let mut in_fault = false;
        for line in text.lines() {
            if in_fault && line.starts_with([' ', '\t']) {
                if let Some((_, fault)) = &mut log.fault {
                    fault_frame(fault, line.trim());
                }
                continue;
            }
            in_fault = false;
            if let Some(header) = line.strip_prefix("# ") {
                log.header(header);
            } else if let Some(fault) = log.fault.is_none().then(|| fault_line(line)).flatten() {
                log.fault = Some((line.trim().to_owned(), fault));
                in_fault = true;
            } else if let Some(rest) = line.strip_prefix("Calls: summary") {
                summary(log.calls.get_or_insert_with(Calls::default), rest);
            } else if let Some(rest) = line.strip_prefix("Frame stats: ") {
                frame_stats(&mut stats, rest);
            } else if let Some(rest) = line.strip_prefix("GPU: ") {
                log.gpu = log.gpu.take().or_else(|| gpu(rest));
            } else if let Some(rest) = line.strip_prefix("Host features: ") {
                log.os = log.os.or_else(|| match rest.split(',').next() {
                    Some("linux") => Some(Os::Linux),
                    Some("windows") => Some(Os::Windows),
                    Some("macos") => Some(Os::Macos),
                    _ => None,
                });
            } else if let Some(rest) = line.strip_prefix("Game profile: ") {
                log.game_profile(rest);
            } else if let Some(rest) = line.strip_prefix("Patches: none for ") {
                log.none_for(rest);
            } else if let Some(kind) = error_kind(line) {
                match &mut log.error {
                    Some((_, seen, count)) if seen == line.trim() => *count = count.saturating_add(1),
                    Some(_) => {}
                    None => log.error = Some((kind, line.trim().to_owned(), 1)),
                }
            }
        }
        if stats.samples > 0 {
            log.perf = Some(Perf {
                samples: stats.samples,
                fps_mean: stats.fps_sum / f64::from(stats.samples),
                fps_min: stats.fps_min,
                worst_frame_ms: stats.worst_ms,
            });
        }
        log
    }

    fn header(&mut self, header: &str) {
        let Some((key, value)) = header.split_once(": ") else {
            return;
        };
        match key {
            "decanter" => {
                let (version, commit) = value.split_once(" (").unwrap_or((value, ""));
                self.decanter_version = Some(version.to_owned());
                self.decanter_commit = commit.strip_suffix(')').map(str::to_owned);
            }
            "port_commit" => self.psport_commit = Some(value.to_owned()),
            "title_id" => self.title_id = Some(value.to_owned()),
            "version" => self.game_version = Some(value.to_owned()),
            "started" => self.day = value.get(..10).map(str::to_owned),
            "env" => {
                let Some((name, value)) = value.split_once('=') else {
                    return;
                };
                if let Some(&(_, setting)) = SETTINGS.iter().find(|(env, _)| *env == name) {
                    self.settings.insert(setting, value.to_owned());
                }
                // The port's store path names its release (`/nix/store/<hash>-psport-0.1.4`).
                if let Some((_, tail)) = value.split_once("-psport-") {
                    self.psport_version = tail.split('/').next().map(str::to_owned);
                }
            }
            _ => {}
        }
    }

    /// `Bloodborne (CUSA03173)` or `Call of Duty: Black Ops (CUSA57547): title data applied`: a
    /// profile psport has for the title, named as the profile names it.
    fn game_profile(&mut self, rest: &str) {
        if let Some(none) = rest.strip_prefix("none for ") {
            self.none_for(none);
            return;
        }
        let Some((name, tail)) = rest.rsplit_once(" (") else {
            return;
        };
        let id = tail.split(')').next().unwrap_or_default();
        let (name, id) = if is_title_id(id) {
            (name, id)
        } else {
            let Some((name, tail)) = name.rsplit_once(" (") else {
                return;
            };
            (name, tail.split(')').next().unwrap_or_default())
        };
        if is_title_id(id) {
            self.game_name = Some(name.to_owned());
            self.title_id.get_or_insert_with(|| id.to_owned());
            self.profile = true;
        }
    }

    /// `none for CUSA43027: the generic paths only`, which psport's own log names its title by.
    fn none_for(&mut self, rest: &str) {
        let id = rest.split(':').next().unwrap_or_default();
        if self.title_id.is_none() && is_title_id(id) {
            self.title_id = Some(id.to_owned());
        }
    }
}

/// The same lines `decanter_ps::failures::classify` takes for a stop or an error, less its benign
/// first-start shader cache read.
fn error_kind(line: &str) -> Option<FailureKind> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("STOP:") {
        return Some(FailureKind::Stop);
    }
    let benign = line.contains("io_file.cpp") && line.contains("/profile.bin");
    let error = trimmed.starts_with("ERROR:") || line.contains("<Error>") || line.contains("<Critical>");
    (error && !benign).then_some(FailureKind::Error)
}

fn hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn signal(rest: &str) -> Option<(u8, &str)> {
    let rest = rest.strip_prefix(" (signal ")?;
    let (number, rest) = rest.split_once(')')?;
    Some((number.parse().ok()?, rest))
}

/// `Guest fault (signal 11) at guest offset 0x1fea9f4, ...` or
/// `Host fault (signal 11) in <path>\libpsport-gpu.dll+0x308914 (<symbol>), ...`.
fn fault_line(line: &str) -> Option<Fault> {
    let trimmed = line.trim_start();
    let (side, rest) = if let Some(rest) = trimmed.strip_prefix("Guest fault") {
        (FaultSide::Guest, rest)
    } else {
        (FaultSide::Host, trimmed.strip_prefix("Host fault")?)
    };
    let (signal, rest) = signal(rest)?;
    let mut fault = Fault {
        side,
        signal,
        frames: Vec::new(),
    };
    if let Some(offset) = rest.strip_prefix(" at guest offset ") {
        if let Some(offset) = hex(offset.split(',').next().unwrap_or_default()) {
            fault.frames.push(Frame {
                module: None,
                offset,
                symbol: None,
            });
        }
    } else if let Some(location) = rest.strip_prefix(" in ") {
        fault.frames.extend(image_frame(location));
    }
    Some(fault)
}

/// `<path>\libpsport-gpu.dll+0x308914 (_ZN11PsFrameRate10IsUnlockedEv)`: the image's file name,
/// its offset and symbol; the folders are dropped here, before anything holds them.
fn image_frame(location: &str) -> Option<Frame> {
    let (place, symbol) = location
        .split_once(" (")
        .map_or((location, None), |(place, rest)| (place, rest.split(')').next()));
    let (image, offset) = place.trim().rsplit_once("+0x")?;
    let module = image.rsplit(['/', '\\']).next()?;
    Some(Frame {
        module: Some(module.to_owned()),
        offset: hex(offset.trim_end_matches(','))?,
        symbol: symbol.map(str::to_owned),
    })
}

/// The lines under a fault: `tid=.. rip=.. image-relative=<hex> ..` for the game's own code and
/// `#1 <image>+0x..` for a host image; bare `#1 <address>` lines are absolute and left out.
fn fault_frame(fault: &mut Fault, line: &str) {
    if fault.frames.len() >= MAX_FRAMES {
        return;
    }
    let frame = if let Some((_, rest)) = line.split_once("image-relative=") {
        rest.split_whitespace().next().and_then(hex).map(|offset| Frame {
            module: None,
            offset,
            symbol: None,
        })
    } else if line.starts_with('#') && line.contains("+0x") {
        line.split_once(' ').and_then(|(_, location)| image_frame(location))
    } else {
        None
    };
    if let Some(frame) = frame {
        let repeated = fault
            .frames
            .last()
            .is_some_and(|last| last.offset == frame.offset && last.module == frame.module);
        if !repeated {
            fault.frames.push(frame);
        }
    }
}

/// `Calls: summary <list> <count> <NID> (<name>)[ -> <code>]` or `... <name> (HLE)`, the format
/// `decanter_ps::failures::CallReport::parse` reads.
fn summary(calls: &mut Calls, rest: &str) {
    let mut fields = rest.trim_start().splitn(3, ' ');
    let (Some(list), Some(count), Some(call)) = (fields.next(), fields.next(), fields.next()) else {
        return;
    };
    let Ok(count) = count.parse() else {
        return;
    };
    let Some((import, tail)) = call.split_once(" (") else {
        return;
    };
    let Some((name, tail)) = tail.split_once(')') else {
        return;
    };
    let name = if name == "HLE" { import } else { name };
    let code = tail
        .trim()
        .strip_prefix("-> ")
        .and_then(|code| match code.strip_prefix("-1, errno ") {
            Some(errno) => errno.trim().parse().ok().map(CallCode::Errno),
            None => u32::from_str_radix(code.trim().strip_prefix("0x")?, 16)
                .ok()
                .map(CallCode::Sce),
        });
    let list = match list {
        "unimplemented" => &mut calls.unimplemented,
        "stub" => &mut calls.stubs,
        "failed" if code.is_some() => &mut calls.failed,
        "status" if code.is_some() => &mut calls.status,
        _ => return,
    };
    if list.len() < MAX_CALLS {
        list.push(Call {
            name: name.to_owned(),
            count,
            code,
        });
    }
}

/// `230.8 FPS, worst frame 65.4 ms (vblank 480 Hz); ...`.
fn frame_stats(stats: &mut FrameStats, rest: &str) {
    let Some((fps, rest)) = rest.split_once(" FPS, worst frame ") else {
        return;
    };
    let (Ok(fps), Some(Ok(worst))) = (fps.parse::<f64>(), rest.split(" ms").next().map(str::parse::<f64>)) else {
        return;
    };
    stats.samples = stats.samples.saturating_add(1);
    stats.fps_sum += fps;
    stats.fps_min = stats.fps_min.min(fps);
    stats.worst_ms = stats.worst_ms.max(worst);
}

/// `AMD Radeon RX 7900 XT (RADV NAVI31), 20464 MiB VRAM; radv 26.2.4 (Mesa 26.2.4), Vulkan 1.4.354`.
fn gpu(rest: &str) -> Option<Gpu> {
    let (model, rest) = rest.split_once(" MiB VRAM")?;
    let (model, _) = model.rsplit_once(", ")?;
    let vendor = [
        ("AMD ", GpuVendor::Amd),
        ("NVIDIA ", GpuVendor::Nvidia),
        ("Intel", GpuVendor::Intel),
        ("Apple ", GpuVendor::Apple),
    ]
    .into_iter()
    .find_map(|(brand, vendor)| model.starts_with(brand).then_some(vendor))?;
    let driver = rest
        .strip_prefix("; ")
        .and_then(|driver| driver.split([',', '(']).next())
        .map(|driver| driver.trim().to_owned());
    Some(Gpu {
        vendor,
        model: model.to_owned(),
        driver,
    })
}
