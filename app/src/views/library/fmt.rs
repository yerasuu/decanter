//! Small display-formatting helpers for the detail pane — kept apart from
//! rendering so `detail.rs` stays about layout, not string wrangling.

pub(super) fn duration(seconds_total: i64) -> String {
    if seconds_total <= 0 {
        return "Sin jugar".to_string();
    }
    let hours = seconds_total / 3600;
    let minutes = (seconds_total % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub(super) fn unix_date(seconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(seconds, 0).map(|dt| dt.format("%d/%m/%Y").to_string())
}
