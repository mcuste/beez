//! A fixed offset from UTC.

use std::fmt;
use std::str::FromStr;

use chrono::FixedOffset;

use crate::error::ScheduleError;

/// Largest offset any real time zone uses.
const LIMIT_SECONDS: i32 = 18 * 3_600;

/// A fixed offset from UTC.
///
/// Beez does not read the time zone database, so an offset stays the same all
/// year. A schedule in a zone with daylight saving moves by one hour twice a
/// year.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UtcOffset(FixedOffset);

impl UtcOffset {
    /// UTC itself.
    pub const UTC: Self = match FixedOffset::east_opt(0) {
        Some(offset) => Self(offset),
        // Zero is always a valid offset, and a const panics at compile time.
        None => panic!("zero is a valid offset"),
    };

    pub(crate) const fn fixed(self) -> FixedOffset {
        self.0
    }

    /// Seconds east of UTC.
    const fn seconds(self) -> i32 {
        self.0.local_minus_utc()
    }
}

impl Default for UtcOffset {
    fn default() -> Self {
        Self::UTC
    }
}

impl FromStr for UtcOffset {
    type Err = ScheduleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || ScheduleError::Offset(value.to_owned());
        if matches!(value, "Z" | "z" | "UTC" | "utc") {
            return Ok(Self::UTC);
        }
        let (sign, rest) = match value.split_at_checked(1) {
            Some(("+", rest)) => (1, rest),
            Some(("-", rest)) => (-1, rest),
            _ => return Err(invalid()),
        };
        let (hours, minutes) = match rest.split_once(':') {
            Some((hours, minutes)) if minutes.len() == 2 => (hours, minutes),
            Some(_) => return Err(invalid()),
            // A bare hour count, such as +02.
            None => (rest, "0"),
        };
        let hours = digits(hours).ok_or_else(invalid)?;
        let minutes = digits(minutes)
            .filter(|minutes| *minutes <= 59)
            .ok_or_else(invalid)?;
        let seconds = hours
            .checked_mul(3_600)
            .and_then(|seconds| seconds.checked_add(minutes * 60))
            .filter(|seconds| *seconds <= LIMIT_SECONDS)
            .ok_or_else(invalid)?;
        FixedOffset::east_opt(sign * seconds)
            .map(Self)
            .ok_or_else(invalid)
    }
}

/// Reads one or more ASCII digits and nothing else, so no sign slips in.
fn digits(text: &str) -> Option<i32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl fmt::Display for UtcOffset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.seconds();
        if seconds == 0 {
            return formatter.write_str("Z");
        }
        let sign = if seconds < 0 { '-' } else { '+' };
        let total = seconds.abs();
        write!(
            formatter,
            "{sign}{:02}:{:02}",
            total.div_euclid(3_600),
            total.rem_euclid(3_600).div_euclid(60)
        )
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::UtcOffset;
    use crate::error::ScheduleError;

    fn parse(value: &str) -> Result<UtcOffset, ScheduleError> {
        value.parse()
    }

    fn east(seconds: i32) -> UtcOffset {
        UtcOffset(FixedOffset::east_opt(seconds).unwrap())
    }

    #[test]
    fn reads_utc_by_several_names() {
        for value in ["Z", "z", "UTC", "utc"] {
            assert_eq!(parse(value), Ok(UtcOffset::UTC), "{value}");
        }
    }

    #[test]
    fn reads_offsets_east_and_west() {
        assert_eq!(parse("+02:00"), Ok(east(7_200)));
        assert_eq!(parse("-05:30"), Ok(east(-19_800)));
        assert_eq!(parse("+02"), Ok(east(7_200)));
    }

    #[test]
    fn rejects_an_hour_count_too_large_to_hold() {
        for value in ["+999999", "-999999", "+99999999999", "+999999:00"] {
            assert!(parse(value).is_err(), "{value} was accepted");
        }
    }

    #[test]
    fn writes_an_offset_back_as_it_reads_it() {
        for value in ["Z", "+02:00", "-05:30", "+14:00"] {
            assert_eq!(parse(value).unwrap().to_string(), value, "{value}");
        }
    }

    #[test]
    fn rejects_an_offset_no_zone_uses() {
        for value in [
            "", "02:00", "+", "+2:0:0", "+19:00", "-19:00", "+02:60", "+aa:00", "++2", "+-0",
            "-+2", "+2:5", "+02:5", "+02:005", "+02:", "+:00", "+ 2", "+02:+5",
        ] {
            assert!(parse(value).is_err(), "{value} was accepted");
        }
    }
}
