//! One log line a minute with what the process holds, so a growth that is seen in a task manager
//! can be told from the log: what the process owns (anonymous), what is only mapped files and
//! libraries, and what the artwork cache keeps decoded.

use std::time::Duration;

use gpui::App;

const EVERY: Duration = Duration::from_secs(60);

/// The resident sizes `smaps_rollup` reports, in kibibytes.
struct Rollup {
    rss: u64,
    anonymous: u64,
    file: u64,
}

fn rollup() -> Option<Rollup> {
    let text = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    let field = |name: &str| {
        text.lines().find_map(|line| {
            line.strip_prefix(name)?
                .trim()
                .strip_suffix("kB")?
                .trim()
                .parse::<u64>()
                .ok()
        })
    };
    Some(Rollup {
        rss: field("Rss:")?,
        anonymous: field("Pss_Anon:")?,
        file: field("Pss_File:")?,
    })
}

pub fn start(cx: &App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(EVERY).await;
            let (images, decoded) = cx.update(|cx| crate::art::decoded_stats(cx));
            let decoded_mb = decoded / (1024 * 1024);
            match rollup() {
                Some(held) => tracing::info!(
                    target: "decanter::memory",
                    rss_mb = held.rss / 1024,
                    anonymous_mb = held.anonymous / 1024,
                    file_mb = held.file / 1024,
                    art_images = images,
                    art_decoded_mb = decoded_mb,
                    "process memory"
                ),
                None => tracing::info!(
                    target: "decanter::memory",
                    art_images = images,
                    art_decoded_mb = decoded_mb,
                    "process memory"
                ),
            }
        }
    })
    .detach();
}
