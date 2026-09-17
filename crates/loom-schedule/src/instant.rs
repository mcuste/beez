//! One instant, as a UTC date and time.

use std::time::SystemTime;

use chrono::{DateTime, Utc};

use crate::error::ScheduleError;

/// Reads an RFC 3339 date and time, such as `2026-09-10T03:00:00Z`.
///
/// A value that names an offset keeps the instant it means, so
/// `2026-09-10T05:00:00+02:00` is the same instant as `2026-09-10T03:00:00Z`.
/// An instant before 1970 is rejected, because no schedule needs one.
pub fn parse_instant(value: &str) -> Result<SystemTime, ScheduleError> {
    let invalid = || ScheduleError::Instant(value.to_owned());
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| invalid())?;
    if parsed.timestamp() < 0 {
        return Err(invalid());
    }

    Ok(SystemTime::from(parsed))
}

/// Writes one instant as a UTC date and time, to the second.
#[must_use]
pub fn format_instant(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{format_instant, parse_instant};

    #[test]
    fn reads_and_writes_a_utc_instant() {
        let time = parse_instant("2026-09-10T03:00:00Z").unwrap();

        assert_eq!(format_instant(time), "2026-09-10T03:00:00Z");
    }

    #[test]
    fn keeps_the_instant_an_offset_names() {
        let time = parse_instant("2026-09-10T05:00:00+02:00").unwrap();

        assert_eq!(format_instant(time), "2026-09-10T03:00:00Z");
    }

    #[test]
    fn rejects_a_value_that_is_not_a_date_and_time() {
        for value in ["", "2026-09-10", "tomorrow", "2026-13-01T00:00:00Z"] {
            assert!(parse_instant(value).is_err(), "{value} was accepted");
        }
    }

    #[test]
    fn rejects_an_instant_before_the_epoch() {
        assert!(parse_instant("1969-07-20T20:17:00Z").is_err());
    }
}
