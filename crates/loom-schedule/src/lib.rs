//! When a workflow runs: a cron expression, or one instant.
//!
//! Cron expressions keep the meaning they have in a crontab. Loom accepts the
//! five-field form, the six-field form that starts with seconds, and the
//! `@daily` style macros.
//!
//! Times are UTC unless a schedule names a fixed offset. Loom does not read
//! the time zone database, so a schedule cannot follow a daylight saving rule.

mod cron_schedule;
mod error;
mod instant;
mod offset;

pub use cron_schedule::CronSchedule;
pub use error::ScheduleError;
pub use instant::{format_instant, parse_instant};
pub use offset::UtcOffset;

use std::time::SystemTime;

/// When a job runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Schedule {
    /// Runs every time the expression matches.
    Cron(CronSchedule),
    /// Runs once, at one instant.
    Once(SystemTime),
}

impl Schedule {
    /// The first time this schedule fires after `time`.
    ///
    /// A one-time schedule that already fired returns nothing, and so does a
    /// cron expression that has no match left, such as one with a past year.
    #[must_use]
    pub fn next_after(&self, time: SystemTime) -> Option<SystemTime> {
        match self {
            Self::Cron(schedule) => schedule.next_after(time),
            Self::Once(instant) => (*instant > time).then_some(*instant),
        }
    }

    /// True when the schedule can fire more than once.
    #[must_use]
    pub fn is_recurring(&self) -> bool {
        matches!(self, Self::Cron(_))
    }
}

impl std::fmt::Display for Schedule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cron(schedule) => schedule.fmt(formatter),
            Self::Once(instant) => formatter.write_str(&format_instant(*instant)),
        }
    }
}
