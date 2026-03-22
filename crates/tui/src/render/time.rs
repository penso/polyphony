use chrono::{DateTime, Local, Utc};

/// Format a UTC timestamp for table listings: "YYYY-MM-DD HH:MM" in local time.
pub(crate) fn format_listing_time(dt: DateTime<Utc>) -> String {
    let local: DateTime<Local> = dt.into();
    local.format("%Y-%m-%d %H:%M").to_string()
}

/// Format a UTC timestamp for detail modals: "YYYY-MM-DD HH:MM:SS" in local time.
pub(crate) fn format_detail_time(dt: DateTime<Utc>) -> String {
    let local: DateTime<Local> = dt.into();
    local.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Format a UTC timestamp as local wall-clock time: "HH:MM".
pub(crate) fn format_short_time(dt: DateTime<Utc>) -> String {
    let local: DateTime<Local> = dt.into();
    local.format("%H:%M").to_string()
}
