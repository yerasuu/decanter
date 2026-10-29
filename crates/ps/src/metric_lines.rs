//! Which lines of a job's output are psport's periodic measurement digests, so the UI can show
//! them apart from the log.
//!
//! psport prints them every 5-10 s with `PS_FRAME_STATS=1`, mostly from the video out thread
//! (`gpu/shadps4/core/libraries/videoout/driver.cpp`); the rest come from the sibling file named
//! next to each prefix. Startup lines (`GPU memory:`, `Texture cache: VRAM budget`), warnings,
//! and errors are deliberately not here: they stay in the log.

/// Prefixes that only a periodic digest uses.
const PREFIXES: &[&str] = &[
    // vk_rasterizer.cpp
    "Texture helper: ",
    // vk_draw_prep.cpp
    "Draw preparation: ",
    // buffer_cache.cpp
    "GPU-written over ",
    // vk_breadcrumbs.cpp: the startup variants say "unavailable" or "no host-visible buffer"
    "GPU breadcrumbs: stream ",
    // buffer_cache_layer.cpp, printed when the copied total crosses a 64 MiB step
    "Layer memory: ",
    // page_manager.cpp
    "Write fault sites ",
    "Image write sites ",
    // driver.cpp
    "Gnm: overlap guard checksummed ",
    "Memory: VRAM ",
    "Buffer bindings: ",
    "Write notices: ",
    "Shader specializations: ",
    "GPU range allocators:",
    "Guest GPU memory: ",
    "Guest waits per frame ",
    "Guest sleeps per frame: ",
    "EOP fences: ",
    "Honest labels: ",
    "Recording: ",
    "Frame pacing: ",
    // equeue.cpp
    "Guest event waits per frame:",
    // vk_rasterizer.cpp
    "Render state memo: ",
    // vk_occlusion.cpp
    "Occlusion queries: ",
    "Runtime: guest threads created in the last ",
];

/// Prefixes whose startup or warning variants continue with text instead of a figure.
const FIGURE_PREFIXES: &[&str] = &[
    // driver.cpp; "Frame stats: no timestamps ..." is printed once by vk_gpu_profiler.cpp
    "Frame stats: ",
    // vk_rasterizer.cpp; "Draw pipe: register copy differs ..." is a diagnostic
    "Draw pipe: ",
    // driver.cpp; "Gnm: <verb> the commands of frame ..." is psport_submit_guard.cpp's warning
    "Gnm: ",
    // vk_gpu_profiler.cpp; "GPU profile: on (...)" is the startup line
    "GPU profile: ",
    // vk_object_motion.cpp; "Object motion: off (...)" is the startup line
    "Object motion: ",
    // vk_rasterizer.cpp: the per-frame stall breakdown ("Stall: 66.7 ms frame; ...") with four
    // indented lines, one block per slow frame; it fills the log at a few frames a second
    "Stall: ",
];

/// The head line of a digest. Its indented continuation lines are not recognised here: see
/// [`classify`].
#[must_use]
pub fn is_metric_line(line: &str) -> bool {
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
        || FIGURE_PREFIXES.iter().any(|prefix| {
            line.strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        || line
            .strip_prefix("GPU: ")
            .is_some_and(|rest| rest.contains(" pipelines compiled in the background"))
}

/// One flag per line, `true` for the lines that belong to a digest.
///
/// That is the head lines and the indented ones right after them (`  drains by reason/s: ...`), since psport prints a digest's
/// detail that way. Guest output can interleave between a head and its details, so an indented
/// line after anything else stays in the log.
pub fn classify<'a>(lines: impl IntoIterator<Item = &'a str>) -> impl Iterator<Item = bool> {
    let mut in_digest = false;
    lines.into_iter().map(move |line| {
        in_digest = is_metric_line(line) || (in_digest && line.starts_with(char::is_whitespace));
        in_digest
    })
}
