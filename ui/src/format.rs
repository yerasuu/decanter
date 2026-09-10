//! Byte sizes, transfer rates and ETAs — shared by the download list and
//! the per-game install progress, which have to agree on how a number is
//! written or the same download reads as two different things.

const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];

/// Scales to the largest unit that leaves a number below 1024, so a size
/// stays readable at any magnitude. Sub-megabyte values get no decimals —
/// "0.7 KB" is noise where "717 B" is the actual number.
pub fn bytes(value: u64) -> String {
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn rate(bytes_per_second: f64) -> String {
    if bytes_per_second <= 0.0 {
        return "—".to_string();
    }
    format!("{}/s", bytes(bytes_per_second as u64))
}

/// Time left at the current rate. `None` when there's nothing meaningful
/// to divide by — a stalled transfer would otherwise show an ETA of
/// hours that changes wildly second to second.
pub fn eta(remaining_bytes: u64, bytes_per_second: f64) -> Option<String> {
    if bytes_per_second <= 0.0 || remaining_bytes == 0 {
        return None;
    }
    let seconds = (remaining_bytes as f64 / bytes_per_second).round() as i64;
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    Some(if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    })
}
