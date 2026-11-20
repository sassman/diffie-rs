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

/// The inverse of [`rfc3339`], for the timestamps a forge reports:
/// strictly `YYYY-MM-DDTHH:MM:SSZ`, UTC, second precision. `None` for
/// anything else, offsets and fractional seconds included.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }
    // Every digit checked by position: `parse` alone would take a signed
    // 4-character year like `+126`.
    let number = |range: std::ops::Range<usize>| {
        bytes[range.clone()]
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| text.get(range))??
            .parse::<i64>()
            .ok()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A proleptic Gregorian date to days since 1970-01-01 (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
    format!("{} ago", span(delta))
}

/// How one instant relates to another, for the conflict panes: "5 days
/// newer", "2 hours older", or "same age" within a minute.
pub fn relative_age(unix_seconds: i64, than: i64) -> String {
    let delta = unix_seconds - than;
    if delta.abs() < 60 {
        return "same age".into();
    }
    if delta > 0 {
        format!("{} newer", span(delta))
    } else {
        format!("{} older", span(-delta))
    }
}

/// `3 days`-style span of a positive number of seconds, largest unit wins.
fn span(delta: i64) -> String {
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
        format!("1 {unit}")
    } else {
        format!("{value} {unit}s")
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
    }

    #[test]
    fn parses_what_it_formats() {
        for seconds in [0, 951_782_400, 1_794_384_145] {
            assert_eq!(parse_rfc3339(&rfc3339(seconds)), Some(seconds));
        }
    }

    #[test]
    fn refuses_other_date_shapes() {
        for text in [
            "",
            "2026-11-11",
            "2026-11-11T08:02:25",
            "2026-11-11T08:02:25+01:00",
            "2026-11-11T08:02:25.5Z",
            "2026-13-11T08:02:25Z",
            "2026-11-11T25:02:25Z",
            "2026-02-31T08:02:25Z",
            "+126-11-11T08:02:25Z",
            "not a date, 20 chars",
        ] {
            assert_eq!(parse_rfc3339(text), None, "{text:?}");
        }
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

    #[test]
    fn phrases_one_instant_against_another() {
        let base = 1_794_384_145;
        assert_eq!(relative_age(base, base), "same age");
        assert_eq!(relative_age(base + 59, base), "same age");
        assert_eq!(relative_age(base + 90, base), "1 minute newer");
        assert_eq!(relative_age(base - 2 * 3_600, base), "2 hours older");
        assert_eq!(relative_age(base + 5 * 86_400, base), "5 days newer");
        assert_eq!(relative_age(base - 20 * 86_400, base), "2 weeks older");
    }
}
