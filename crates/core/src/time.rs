//! RFC 3339 timestamps without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default();
    rfc3339(seconds)
}

/// UTC, second precision: `2026-11-11T08:59:00Z`.
pub fn rfc3339(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let second_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

/// `9 Nov`-style date for commit lists. UTC.
pub fn short_date(unix_seconds: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
    format!("{day} {}", MONTHS[(month - 1) as usize])
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// `6 days ago`-style relative time for commit lists. A future instant
/// (clock skew between machines) reads as "just now".
pub fn ago(unix_seconds: i64, now: i64) -> String {
    let delta = now - unix_seconds;
    if delta < 60 {
        return "just now".into();
    }
    let (value, unit) = if delta < 3_600 {
        (delta / 60, "minute")
    } else if delta < 86_400 {
        (delta / 3_600, "hour")
    } else if delta < 7 * 86_400 {
        (delta / 86_400, "day")
    } else if delta < 30 * 86_400 {
        (delta / (7 * 86_400), "week")
    } else if delta < 365 * 86_400 {
        (delta / (30 * 86_400), "month")
    } else {
        (delta / (365 * 86_400), "year")
    };
    if value == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{value} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_794_384_145), "2026-11-11T08:02:25Z");
        assert_eq!(short_date(1_794_384_145), "11 Nov");
    }

    #[test]
    fn humanizes_time_since() {
        let now = 1_794_384_145;
        assert_eq!(ago(now - 5, now), "just now");
        assert_eq!(ago(now + 120, now), "just now");
        assert_eq!(ago(now - 90, now), "1 minute ago");
        assert_eq!(ago(now - 7_200, now), "2 hours ago");
        assert_eq!(ago(now - 6 * 86_400, now), "6 days ago");
        assert_eq!(ago(now - 20 * 86_400, now), "2 weeks ago");
        assert_eq!(ago(now - 100 * 86_400, now), "3 months ago");
        assert_eq!(ago(now - 800 * 86_400, now), "2 years ago");
    }
}
