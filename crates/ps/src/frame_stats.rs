//! What a run measured with `PS_FRAME_STATS=1`, read back from its log ([`crate::run_log`]).
//!
//! psport prints a `Frame stats:` line every 5 s or more from the video out thread
//! (`gpu/shadps4/core/libraries/videoout/driver.cpp`), and, while it compiles pipelines in the
//! background, a `GPU: ... pipelines compiled in the background` line at most every 5 s
//! (`gpu/shadps4/video_core/renderer_vulkan/vk_pipeline_cache.cpp`). Only these windows reach the
//! log, not each frame, so the figures below are built from them.

use std::fs::File;
use std::io::{self, BufRead as _, BufReader};
use std::path::Path;

/// Above any display psport drives: a larger value is a damaged line, not a measurement.
const MAX_FPS: f64 = 1000.0;

/// One `Frame stats:` window.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Frames presented over the window's length.
    pub fps: f64,
    /// The slowest frame of the window.
    pub worst_ms: f64,
    /// Shaders and pipelines compiled on the GPU thread, which waited for them.
    pub compiles: u64,
    pub compile_ms: f64,
}

/// The `GPU:` lines summed over the run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pipelines {
    pub background: u64,
    /// Draws skipped for a frame because their pipeline was still compiling.
    pub skipped: u64,
    /// Draws that waited for their pipeline instead (full-screen passes, indirect draws).
    pub waited: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RunStats {
    /// `YYYY-MM-DD HH:MM UTC`, from the log's header.
    pub started: Option<String>,
    pub samples: Vec<Sample>,
    pub pipelines: Pipelines,
}

impl RunStats {
    /// `None` when the run was not started with `PS_FRAME_STATS=1` (or bbport's `BB_`), read from
    /// the header's environment so the rest of an unmeasured log is never read. Lines that start
    /// like a measurement and don't parse are skipped and counted in a debug event.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        let mut reader = BufReader::new(File::open(path)?);
        let mut stats = Self::default();
        let mut buffer = Vec::new();
        let mut measured = false;
        loop {
            buffer.clear();
            if reader.read_until(b'\n', &mut buffer)? == 0 {
                return Ok(None);
            }
            let line = String::from_utf8_lossy(&buffer);
            let Some(field) = line.trim_end().strip_prefix("# ") else {
                continue;
            };
            if field == "---" {
                break;
            }
            if let Some(started) = field.strip_prefix("started: ") {
                stats.started = display_time(started);
            }
            measured |= matches!(field, "env: PS_FRAME_STATS=1" | "env: BB_FRAME_STATS=1");
        }
        if !measured {
            return Ok(None);
        }
        let mut malformed: u32 = 0;
        loop {
            buffer.clear();
            if reader.read_until(b'\n', &mut buffer)? == 0 {
                break;
            }
            if !stats.line(String::from_utf8_lossy(&buffer).trim()) {
                malformed = malformed.saturating_add(1);
            }
        }
        if malformed > 0 {
            tracing::debug!(
                log = %path.display(),
                malformed,
                samples = stats.samples.len(),
                "frame stats lines skipped"
            );
        }
        Ok(Some(stats))
    }

    /// `false` only for a line that starts like a measurement and does not parse.
    fn line(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix("Frame stats: ") {
            return parse_sample(rest).map(|sample| self.samples.push(sample)).is_some();
        }
        if let Some(rest) = line.strip_prefix("GPU: ")
            && rest.contains(" pipelines compiled in the background")
        {
            let Some((background, skipped, waited)) = parse_pipelines(rest) else {
                return false;
            };
            self.pipelines.background = self.pipelines.background.saturating_add(background);
            self.pipelines.skipped = self.pipelines.skipped.saturating_add(skipped);
            self.pipelines.waited = self.pipelines.waited.saturating_add(waited);
        }
        true
    }

    /// Mean of the windows' rates: each window is about 5 s long, so this is close to frames over
    /// time.
    pub fn average_fps(&self) -> Option<f64> {
        mean(self.samples.iter().map(|sample| sample.fps))
    }

    /// Approximated, since the log holds no single frame: each window's worst frame stands for its
    /// slowest 1 % (one frame of 150 at 30 FPS over 5 s, of 300 at 60), so the rate of their mean
    /// is the 1 % low; it reads a little low at higher rates, where one frame is less than 1 %.
    pub fn one_percent_low(&self) -> Option<f64> {
        mean(self.samples.iter().map(|sample| sample.worst_ms))
            .filter(|&worst| worst > 0.0)
            .map(|worst| 1000.0 / worst)
    }

    /// The fastest is the best window's mean frame time, since psport prints no fastest frame;
    /// the slowest is the worst frame of the run.
    pub fn frame_time_range(&self) -> Option<(f64, f64)> {
        let best_fps = self.samples.iter().map(|sample| sample.fps).fold(0.0, f64::max);
        let worst = self.samples.iter().map(|sample| sample.worst_ms).fold(0.0, f64::max);
        (best_fps > 0.0).then(|| (1000.0 / best_fps, worst))
    }

    /// Shaders and pipelines compiled on the GPU thread, and the milliseconds it waited for them.
    pub fn compiles(&self) -> (u64, f64) {
        self.samples.iter().fold((0, 0.0), |(count, ms), sample| {
            (count.saturating_add(sample.compiles), ms + sample.compile_ms)
        })
    }
}

fn mean(values: impl ExactSizeIterator<Item = f64>) -> Option<f64> {
    let count = values.len();
    (count > 0).then(|| values.sum::<f64>() / count as f64)
}

/// `59.8 FPS, worst frame 33.4 ms (vblank 60 Hz); 3 shader/pipeline compiles, 12.5 ms; ...`; the
/// compiles are optional, for ports that printed less.
fn parse_sample(text: &str) -> Option<Sample> {
    let (fps, rest) = text.split_once(" FPS, worst frame ")?;
    let (worst, rest) = rest.split_once(" ms")?;
    let fps = measurement(fps).filter(|&fps| fps <= MAX_FPS)?;
    let worst_ms = measurement(worst)?;
    let (compiles, compile_ms) = match rest
        .split_once("; ")
        .and_then(|(_, rest)| rest.split_once(" shader/pipeline compiles, "))
    {
        Some((count, rest)) => (count.parse().ok()?, measurement(rest.split_once(" ms")?.0)?),
        None => (0, 0.0),
    };
    Some(Sample {
        fps,
        worst_ms,
        compiles,
        compile_ms,
    })
}

/// `N pipelines compiled in the background, M draws went without theirs meanwhile, K waited for one`.
fn parse_pipelines(text: &str) -> Option<(u64, u64, u64)> {
    let (background, rest) = text.split_once(" pipelines compiled in the background, ")?;
    let (skipped, rest) = rest.split_once(" draws went without theirs meanwhile, ")?;
    let waited = rest.strip_suffix(" waited for one")?;
    Some((background.parse().ok()?, skipped.parse().ok()?, waited.parse().ok()?))
}

fn measurement(text: &str) -> Option<f64> {
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// `2026-10-16T09:41:07Z` as `2026-10-16 09:41 UTC`, the way failures are dated.
pub(crate) fn display_time(iso: &str) -> Option<String> {
    let well_formed = iso.len() == 20
        && iso.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        });
    if !well_formed {
        return None;
    }
    Some(format!("{} {} UTC", iso.get(..10)?, iso.get(11..16)?))
}
