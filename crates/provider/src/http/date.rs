//! The IMF-fixdate of an HTTP `Date` header as Unix seconds (RFC 9110,
//! 5.6.7). The usage-limit wait is measured from the failed reply's own
//! `Date`, so no clock is read (`docs/model-routing.md`, "Protocols and
//! providers").

/// `text` as Unix seconds, when it is an IMF-fixdate such as `Sun, 06 Nov
/// 1994 08:49:37 GMT`. The weekday keeps its three-letter shape but is not
/// checked against the date; the day is two digits, the year four, and the
/// zone exactly `GMT`.
pub(super) fn http_date(text: &str) -> Option<u64> {
    let (weekday, rest) = text.split_once(", ")?;
    if weekday.len() != 3 || !weekday.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return None;
    }
    let mut fields = rest.split(' ');
    let (day, month, year, time, zone) = match (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) {
        (Some(day), Some(month), Some(year), Some(time), Some(zone), None) => {
            (day, month, year, time, zone)
        }
        _ => return None,
    };
    if zone != "GMT" {
        return None;
    }
    let day = two_digits(day)?;
    let month = numbered_month(month)?;
    let year = four_digits(year)?;
    let mut clock = time.split(':');
    let (hour, minute, second) = match (clock.next(), clock.next(), clock.next(), clock.next()) {
        (Some(hour), Some(minute), Some(second), None) => {
            (two_digits(hour)?, two_digits(minute)?, two_digits(second)?)
        }
        _ => return None,
    };
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_since_epoch(year, month, day)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Exactly two ASCII digits as a number.
fn two_digits(text: &str) -> Option<u64> {
    if text.len() != 2 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Exactly four ASCII digits as a number.
fn four_digits(text: &str) -> Option<u64> {
    if text.len() != 4 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The month's number, spelled exactly as IMF-fixdate spells it.
fn numbered_month(month: &str) -> Option<u64> {
    match month {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

/// Whether `year` is a leap year in the civil calendar.
fn is_leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// The days in `month` of `year`.
fn month_length(year: u64, month: u64) -> u64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// The days from the epoch to `year`-`month`-`day`, rejecting a year before
/// the epoch and a day the month does not hold.
fn days_since_epoch(year: u64, month: u64, day: u64) -> Option<u64> {
    if year < 1970 || !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > month_length(year, month) {
        return None;
    }
    // March starts the counted year, so February's leap day lands last.
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let shifted_month = if month <= 2 { month + 12 } else { month };
    let era = shifted_year / 400;
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * (shifted_month - 3) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
#[path = "date_tests.rs"]
mod tests;
