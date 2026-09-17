//! UTC timestamps for Loom's own lines and for run directory names.

use std::time::SystemTime;

use chrono::{DateTime, Utc};

/// Formats `time` as a UTC date and time, to the millisecond.
///
/// Local time needs the time zone database, so Loom reports UTC and marks it
/// with the `Z`.
pub fn utc(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Formats `time` as a UTC stamp without separators, for a directory name.
///
/// The stamp sorts by name in the order the runs happened.
pub fn compact_utc(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y%m%dT%H%M%S%3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{compact_utc, utc};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn formats_a_utc_timestamp() {
        let time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_042);

        assert_eq!(utc(time), "2023-11-14T22:13:20.042Z");
    }

    #[test]
    fn formats_the_last_second_of_a_year() {
        let time = UNIX_EPOCH + Duration::from_secs(1_767_225_599);

        assert_eq!(utc(time), "2025-12-31T23:59:59.000Z");
    }

    #[test]
    fn formats_a_leap_day() {
        let time = UNIX_EPOCH + Duration::from_secs(951_782_401);

        assert_eq!(utc(time), "2000-02-29T00:00:01.000Z");
    }

    #[test]
    fn formats_a_compact_stamp_for_a_directory_name() {
        let time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_042);

        assert_eq!(compact_utc(time), "20231114T221320042Z");
    }

    #[test]
    fn orders_compact_stamps_by_name() {
        let earlier = compact_utc(UNIX_EPOCH + Duration::from_millis(1_700_000_000_042));
        let later = compact_utc(UNIX_EPOCH + Duration::from_millis(1_700_000_000_500));

        assert!(earlier < later, "{earlier} is not before {later}");
    }
}
