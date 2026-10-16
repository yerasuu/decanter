//! Small display-formatting helpers for the detail pane — kept apart from
//! rendering so `detail.rs` stays about layout, not string wrangling.

use chrono::{DateTime, Datelike as _, Local, NaiveDate, Timelike as _};
use rust_i18n::t;

pub(super) fn duration(seconds_total: i64) -> String {
    if seconds_total <= 0 {
        return t!("library.never_played").into_owned();
    }
    let hours = seconds_total / 3600;
    let minutes = (seconds_total % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// A moment the player lived (a session), so it reads in their own time zone.
pub(super) fn unix_date(seconds: i64) -> Option<String> {
    DateTime::from_timestamp(seconds, 0).map(|dt| date(dt.with_timezone(&Local).date_naive()))
}

/// Stores give release days as UTC midnight; shifting that to local time
/// would show the day before anywhere west of Greenwich.
pub(super) fn utc_date(seconds: i64) -> Option<String> {
    DateTime::from_timestamp(seconds, 0).map(|dt| date(dt.date_naive()))
}

pub(super) fn date_time(at: DateTime<Local>) -> String {
    let (is_pm, hour12) = at.hour12();
    let time = t!(
        "common.date.time",
        hour24 = format!("{:02}", at.hour()),
        hour12 = hour12,
        minute = format!("{:02}", at.minute()),
        meridiem = if is_pm { "PM" } else { "AM" }
    );
    format!("{}, {time}", date(at.date_naive()))
}

fn date(day: NaiveDate) -> String {
    let months = t!("common.date.months");
    let month = usize::try_from(day.month0())
        .ok()
        .and_then(|index| months.split_whitespace().nth(index))
        .map_or_else(|| day.month().to_string(), str::to_owned);
    t!("common.date.format", day = day.day(), month = month, year = day.year()).into_owned()
}

/// GamesDB's aggregated score is a 0–100 float kept as text; anything else
/// a store might put there is shown as it came.
pub(super) fn rating(raw: &str) -> String {
    raw.parse::<f64>()
        .ok()
        .filter(|score| score.is_finite())
        .map_or_else(|| raw.to_owned(), |score| format!("{score:.0}/100"))
}
