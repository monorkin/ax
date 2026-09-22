//! Time without a chrono dependency: ISO timestamps for stored records.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// Seconds since the epoch of an RFC 3339 timestamp, as the usage API
/// writes them: `2026-09-22T16:20:00.123456+00:00`, or with a `Z`.
pub fn epoch_seconds_of(timestamp: &str) -> Option<i64> {
    let year: i64 = timestamp.get(0..4)?.parse().ok()?;
    let month: i64 = timestamp.get(5..7)?.parse().ok()?;
    let day: i64 = timestamp.get(8..10)?.parse().ok()?;
    let hour: i64 = timestamp.get(11..13)?.parse().ok()?;
    let minute: i64 = timestamp.get(14..16)?.parse().ok()?;
    let second: i64 = timestamp.get(17..19)?.parse().ok()?;

    let zone = timestamp.get(19..)?.trim_start_matches(|it: char| it == '.' || it.is_ascii_digit());
    let offset = match zone {
        "" | "Z" => 0,
        _ => {
            let hours: i64 = zone.get(1..3)?.parse().ok()?;
            let minutes: i64 = zone.get(4..6)?.parse().ok()?;
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            sign * (hours * 3600 + minutes * 60)
        }
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 of a date, the inverse of `civil_date`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// "3h 12m", "2d 4h", "12m": how long until something, to the minute.
pub fn span(seconds: i64) -> String {
    let minutes = seconds.max(0) / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs();
    format_epoch_seconds(seconds)
}

fn format_epoch_seconds(seconds: u64) -> String {
    let days_since_epoch = seconds / 86_400;
    let (year, month, day) = civil_date(days_since_epoch);
    let hour = seconds / 3600 % 24;
    let minute = seconds / 60 % 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_date(days_since_epoch: u64) -> (u64, u64, u64) {
    let days = days_since_epoch + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_apis_timestamps_are_read_whatever_their_zone() {
        assert_eq!(epoch_seconds_of("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds_of("2026-09-22T16:20:00Z"), Some(1_790_094_000));
        assert_eq!(epoch_seconds_of("2026-09-22T16:20:00.123456+00:00"), Some(1_790_094_000));
        assert_eq!(epoch_seconds_of("2026-09-22T18:20:00+02:00"), Some(1_790_094_000));
        assert_eq!(epoch_seconds_of("2024-02-29T00:00:00Z"), Some(1_709_164_800), "a leap day");
        assert_eq!(format_epoch_seconds(1_790_094_000), "2026-09-22T16:20:00Z", "and back again");
        assert_eq!(epoch_seconds_of("soon"), None);
    }

    #[test]
    fn a_span_is_said_to_the_minute() {
        assert_eq!(span(12 * 60 + 30), "12m");
        assert_eq!(span(3 * 3600 + 12 * 60), "3h 12m");
        assert_eq!(span(2 * 86_400 + 4 * 3600 + 59), "2d 4h");
        assert_eq!(span(-5), "0m");
    }
}
