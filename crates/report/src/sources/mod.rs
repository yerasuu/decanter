//! What decanter keeps locally that a report is built from. Each source takes the file's text, so
//! the caller decides what to read and nothing here touches the filesystem.

pub mod day_log;
pub mod failures;
pub mod game_log;

/// `YYYY-MM-DD` (UTC) of a Unix time: the day is all a report keeps of when something happened.
/// Days-from-civil inverse (H. Hinnant), as `decanter_ps::failures::format_time` does.
#[must_use]
pub fn day_of(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or_default();
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}
