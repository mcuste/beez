//! Reports a schedule Beez cannot use.

/// Reports an invalid schedule.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScheduleError {
    /// The expression is empty.
    #[error("schedule must not be empty")]
    Empty,
    /// The expression does not have five, six, or seven fields.
    #[error(
        "cron expression has {0} fields, but needs 5 (minute hour day-of-month month \
         day-of-week), 6 with leading seconds, or 7 with a trailing year"
    )]
    FieldCount(usize),
    /// The cron crate rejected the expression.
    #[error("{0}")]
    Expression(String),
    /// The seconds field is not one fixed value.
    #[error("the seconds field must be one fixed value, so a schedule fires at most once a minute")]
    SubMinute,
    /// A day of week is outside 0 to 7.
    #[error("day of week {0} is not 0 to 7, where both 0 and 7 mean Sunday")]
    DayOfWeek(String),
    /// The instant is not an RFC 3339 date and time.
    #[error("{0} is not an RFC 3339 date and time, such as 2026-09-10T03:00:00Z")]
    Instant(String),
    /// The offset is not a fixed offset from UTC.
    #[error("{0} is not a fixed offset from UTC, such as Z, +02:00, or -05:30")]
    Offset(String),
}
