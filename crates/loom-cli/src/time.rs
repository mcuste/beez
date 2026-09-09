//! UTC timestamps for Loom's own lines and for run directory names.

use std::time::{SystemTime, UNIX_EPOCH};

/// One instant split into UTC calendar fields.
struct Parts {
    year: u64,
    month: u64,
    day: u64,
    hour: u64,
    minute: u64,
    second: u64,
    millisecond: u32,
}

/// Formats `time` as a UTC date and time, to the millisecond.
///
/// Local time needs the time zone database, so Loom reports UTC and marks it
/// with the `Z`.
pub(crate) fn utc(time: SystemTime) -> String {
    let parts = split(time);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        parts.year,
        parts.month,
        parts.day,
        parts.hour,
        parts.minute,
        parts.second,
        parts.millisecond
    )
}

/// Formats `time` as a UTC stamp without separators, for a directory name.
///
/// The stamp sorts by name in the order the runs happened.
pub(crate) fn compact_utc(time: SystemTime) -> String {
    let parts = split(time);

    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}{:03}Z",
        parts.year,
        parts.month,
        parts.day,
        parts.hour,
        parts.minute,
        parts.second,
        parts.millisecond
    )
}

fn split(time: SystemTime) -> Parts {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let day_seconds = seconds.rem_euclid(86_400);

    Parts {
        year,
        month,
        day,
        hour: day_seconds.div_euclid(3_600),
        minute: day_seconds.rem_euclid(3_600).div_euclid(60),
        second: day_seconds.rem_euclid(60),
        millisecond: since_epoch.subsec_millis(),
    }
}

/// Splits days since 1970-01-01 into a year, a month and a day.
///
/// This is Howard Hinnant's civil-from-days algorithm, for days at or after
/// the epoch only.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era = (day_of_era - day_of_era.div_euclid(1_460) + day_of_era.div_euclid(36_524)
        - day_of_era.div_euclid(146_096))
    .div_euclid(365);
    let year = year_of_era + era * 400;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era.div_euclid(4) - year_of_era.div_euclid(100));
    // March is month zero, so January and February belong to the year after.
    let shifted_month = (5 * day_of_year + 2).div_euclid(153);
    let day = day_of_year - (153 * shifted_month + 2).div_euclid(5) + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };

    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, compact_utc, utc};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn splits_days_into_a_civil_date() {
        let expected = [
            (0, (1970, 1, 1)),
            (19_675, (2023, 11, 14)),
            (20_453, (2025, 12, 31)),
            // Leap days, including the year 2000 century rule.
            (11_016, (2000, 2, 29)),
            (12_477, (2004, 2, 29)),
            (47_482, (2100, 1, 1)),
        ];

        for (days, date) in expected {
            assert_eq!(civil_from_days(days), date, "{days} days");
        }
    }

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
