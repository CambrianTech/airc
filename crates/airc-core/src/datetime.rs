//! UTC timestamp helpers used by shell compatibility adapters.

use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateTimeError {
    InvalidFormat,
    InvalidDate,
    InvalidTime,
}

impl fmt::Display for DateTimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => f.write_str("timestamp must match YYYY-MM-DDTHH:MM:SSZ"),
            Self::InvalidDate => f.write_str("timestamp date is invalid"),
            Self::InvalidTime => f.write_str("timestamp time is invalid"),
        }
    }
}

impl Error for DateTimeError {}

pub fn iso_to_epoch(timestamp: &str) -> Result<i64, DateTimeError> {
    if timestamp.len() != 20
        || timestamp.as_bytes()[4] != b'-'
        || timestamp.as_bytes()[7] != b'-'
        || timestamp.as_bytes()[10] != b'T'
        || timestamp.as_bytes()[13] != b':'
        || timestamp.as_bytes()[16] != b':'
        || timestamp.as_bytes()[19] != b'Z'
    {
        return Err(DateTimeError::InvalidFormat);
    }

    let year = parse_i32(&timestamp[0..4])?;
    let month = parse_u32(&timestamp[5..7])?;
    let day = parse_u32(&timestamp[8..10])?;
    let hour = parse_u32(&timestamp[11..13])?;
    let minute = parse_u32(&timestamp[14..16])?;
    let second = parse_u32(&timestamp[17..19])?;

    if hour > 23 || minute > 59 || second > 59 {
        return Err(DateTimeError::InvalidTime);
    }
    if month == 0 || month > 12 {
        return Err(DateTimeError::InvalidDate);
    }
    let month_days = days_in_month(year, month);
    if day == 0 || day > month_days {
        return Err(DateTimeError::InvalidDate);
    }

    let days = days_from_civil(year, month, day);
    Ok(days * 86_400 + i64::from(hour * 3_600 + minute * 60 + second))
}

fn parse_i32(value: &str) -> Result<i32, DateTimeError> {
    parse_digits(value)
        .and_then(|parsed| i32::try_from(parsed).map_err(|_| DateTimeError::InvalidFormat))
}

fn parse_u32(value: &str) -> Result<u32, DateTimeError> {
    parse_digits(value)
        .and_then(|parsed| u32::try_from(parsed).map_err(|_| DateTimeError::InvalidFormat))
}

fn parse_digits(value: &str) -> Result<u64, DateTimeError> {
    value.bytes().try_fold(0u64, |acc, byte| match byte {
        b'0'..=b'9' => Ok(acc * 10 + u64::from(byte - b'0')),
        _ => Err(DateTimeError::InvalidFormat),
    })
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::from(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    use super::{iso_to_epoch, DateTimeError};

    #[test]
    fn known_timestamp_matches_legacy_python() {
        assert_eq!(iso_to_epoch("2026-01-15T12:34:56Z").unwrap(), 1_768_480_496);
    }

    #[test]
    fn unix_epoch_round_trips_to_zero() {
        assert_eq!(iso_to_epoch("1970-01-01T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn leap_day_is_valid_only_in_leap_year() {
        assert_eq!(iso_to_epoch("2024-02-29T00:00:00Z").unwrap(), 1_709_164_800);
        assert_eq!(
            iso_to_epoch("2023-02-29T00:00:00Z"),
            Err(DateTimeError::InvalidDate)
        );
    }

    #[test]
    fn malformed_timestamps_fail_without_epoch() {
        assert_eq!(iso_to_epoch(""), Err(DateTimeError::InvalidFormat));
        assert_eq!(
            iso_to_epoch("not-a-timestamp"),
            Err(DateTimeError::InvalidFormat)
        );
        assert_eq!(
            iso_to_epoch("2026-01-15 12:34:56Z"),
            Err(DateTimeError::InvalidFormat)
        );
        assert_eq!(
            iso_to_epoch("2026-01-15T25:34:56Z"),
            Err(DateTimeError::InvalidTime)
        );
    }
}
