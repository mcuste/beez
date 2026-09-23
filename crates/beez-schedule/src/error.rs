//! Reports a schedule Beez cannot use.

use std::fmt;

/// Reports an invalid schedule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleError {
    /// The expression is empty.
    Empty,
    /// The expression does not have five, six, or seven fields.
    FieldCount(usize),
    /// The cron crate rejected the expression.
    Expression(String),
    /// The seconds field is not one fixed value.
    SubMinute,
    /// A day of week is outside 0 to 7.
    DayOfWeek(String),
    /// The instant is not an RFC 3339 date and time.
    Instant(String),
    /// The offset is not a fixed offset from UTC.
    Offset(String),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("schedule must not be empty"),
            Self::FieldCount(count) => write!(
                formatter,
                "cron expression has {count} fields, but needs 5 (minute hour day-of-month month \
                 day-of-week), 6 with leading seconds, or 7 with a trailing year"
            ),
            Self::Expression(error) => formatter.write_str(error),
            Self::SubMinute => formatter.write_str(
                "the seconds field must be one fixed value, so a schedule fires at most once a \
                 minute",
            ),
            Self::DayOfWeek(value) => write!(
                formatter,
                "day of week {value} is not 0 to 7, where both 0 and 7 mean Sunday"
            ),
            Self::Instant(value) => write!(
                formatter,
                "{value} is not an RFC 3339 date and time, such as 2026-09-10T03:00:00Z"
            ),
            Self::Offset(value) => write!(
                formatter,
                "{value} is not a fixed offset from UTC, such as Z, +02:00, or -05:30"
            ),
        }
    }
}

impl std::error::Error for ScheduleError {}
