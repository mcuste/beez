//! A fixed offset from UTC.

use std::fmt;
use std::str::FromStr;

use chrono::FixedOffset;

use crate::error::ScheduleError;

/// Largest offset any real time zone uses.
const LIMIT_SECONDS: i32 = 18 * 3_600;

/// A fixed offset from UTC, in seconds east.
///
/// Loom does not read the time zone database, so an offset stays the same all
/// year. A schedule in a zone with daylight saving moves by one hour twice a
/// year.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UtcOffset(i32);

impl UtcOffset {
    /// UTC itself.
    pub const UTC: Self = Self(0);

    /// Seconds east of UTC.
    #[must_use]
    pub fn seconds(self) -> i32 {
        self.0
    }

    pub(crate) fn fixed(self) -> Option<FixedOffset> {
        FixedOffset::east_opt(self.0)
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
            Some((hours, minutes)) => (hours, minutes),
            // A bare hour count, such as +02.
            None => (rest, "0"),
        };
        let hours = hours.parse::<i32>().map_err(|_| invalid())?;
        let minutes = minutes.parse::<i32>().map_err(|_| invalid())?;
        if hours < 0 || !(0..=59).contains(&minutes) {
            return Err(invalid());
        }
        let seconds = sign * (hours * 3_600 + minutes * 60);
        if seconds.abs() > LIMIT_SECONDS {
            return Err(invalid());
        }
        Ok(Self(seconds))
    }
}

impl fmt::Display for UtcOffset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return formatter.write_str("Z");
        }
        let sign = if self.0 < 0 { '-' } else { '+' };
        let total = self.0.abs();
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
    use super::UtcOffset;
    use crate::error::ScheduleError;

    fn parse(value: &str) -> Result<UtcOffset, ScheduleError> {
        value.parse()
    }

    #[test]
    fn reads_utc_by_several_names() {
        for value in ["Z", "z", "UTC", "utc"] {
            assert_eq!(parse(value), Ok(UtcOffset::UTC), "{value}");
        }
    }

    #[test]
    fn reads_offsets_east_and_west() {
        assert_eq!(parse("+02:00").unwrap().seconds(), 7_200);
        assert_eq!(parse("-05:30").unwrap().seconds(), -19_800);
        assert_eq!(parse("+02").unwrap().seconds(), 7_200);
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
            "", "02:00", "+", "+2:0:0", "+19:00", "-19:00", "+02:60", "+aa:00",
        ] {
            assert!(parse(value).is_err(), "{value} was accepted");
        }
    }
}
