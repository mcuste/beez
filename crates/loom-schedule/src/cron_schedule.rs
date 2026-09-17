//! A cron expression that keeps its crontab meaning.
//!
//! The `cron` crate parses the Quartz form of cron, which starts with a
//! seconds field and counts days of week from Sunday as 1. A crontab has no
//! seconds field and counts days of week from Sunday as 0. Every expression
//! moves to the Quartz form here, so the rest of Loom means crontab.

use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use crate::error::ScheduleError;
use crate::instant::{from_utc, to_utc};
use crate::offset::UtcOffset;

/// Field position of the seconds, which Loom holds to one fixed value.
const SECONDS: usize = 0;
/// Field position of the day of month.
const DAY_OF_MONTH: usize = 3;
/// Field position of the day of week.
const DAY_OF_WEEK: usize = 5;

/// A cron expression and the offset it is read in.
///
/// Loom accepts the five-field crontab form, the six-field form that starts
/// with seconds, the seven-field form that ends with a year, and the `@daily`
/// style macros.
///
/// Days of week count from Sunday as 0, and 7 is Sunday as well.
///
/// A crontab reads a restricted day of month and a restricted day of week as
/// "either day", so `0 3 13 * Fri` means the 13th of any month and every
/// Friday. Loom keeps that meaning, which needs two schedules, because the
/// parser underneath keeps only the days both fields hold.
#[derive(Clone, Debug)]
pub struct CronSchedule {
    text: String,
    offset: UtcOffset,
    primary: Box<cron::Schedule>,
    /// Holds the day-of-week half when a crontab would read "either day".
    alternate: Option<Box<cron::Schedule>>,
}

impl CronSchedule {
    /// Reads `expression` and fires it in `offset`.
    pub fn new(expression: &str, offset: UtcOffset) -> Result<Self, ScheduleError> {
        let text = expression.trim();
        if text.is_empty() {
            return Err(ScheduleError::Empty);
        }
        let (primary, alternate) = if text.starts_with('@') {
            (parse(text)?, None)
        } else {
            let fields = normalize(text)?;
            let field = |position: usize| fields.get(position).map_or("*", String::as_str);
            if restricts(field(DAY_OF_MONTH)) && restricts(field(DAY_OF_WEEK)) {
                (
                    parse(&with_field(&fields, DAY_OF_WEEK, "*"))?,
                    Some(parse(&with_field(&fields, DAY_OF_MONTH, "*"))?),
                )
            } else {
                (parse(&fields.join(" "))?, None)
            }
        };

        Ok(Self {
            text: text.to_owned(),
            offset,
            primary,
            alternate,
        })
    }

    /// The first time the expression matches after `time`.
    ///
    /// Returns nothing when no match is left, such as for an expression that
    /// names a year in the past.
    #[must_use]
    pub fn next_after(&self, time: SystemTime) -> Option<SystemTime> {
        let offset = self.offset.fixed()?;
        let start = to_utc(time)?.with_timezone(&offset);
        let primary = self.primary.after(&start).next();
        let alternate = self
            .alternate
            .as_ref()
            .and_then(|schedule| schedule.after(&start).next());
        let next = match (primary, alternate) {
            (Some(primary), Some(alternate)) => primary.min(alternate),
            (Some(next), None) | (None, Some(next)) => next,
            (None, None) => return None,
        };

        from_utc(next.to_utc())
    }
}

/// Two schedules are the same when they are written the same way.
impl PartialEq for CronSchedule {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.offset == other.offset
    }
}

impl Eq for CronSchedule {}

impl FromStr for CronSchedule {
    type Err = ScheduleError;

    fn from_str(expression: &str) -> Result<Self, Self::Err> {
        Self::new(expression, UtcOffset::UTC)
    }
}

impl fmt::Display for CronSchedule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)?;
        if self.offset != UtcOffset::UTC {
            write!(formatter, " {}", self.offset)?;
        }
        Ok(())
    }
}

/// Puts an expression in the form the parser reads.
///
/// A crontab expression has no seconds field, so it gains one that holds the
/// start of the minute, and its days of week move to the numbers the parser
/// counts in.
fn normalize(text: &str) -> Result<Vec<String>, ScheduleError> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    let fields: Vec<&str> = match fields.len() {
        5 => std::iter::once("0").chain(fields).collect(),
        6 | 7 => fields,
        count => return Err(ScheduleError::FieldCount(count)),
    };
    // A workflow run must not start more than once a minute.
    if !fields
        .get(SECONDS)
        .is_some_and(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(ScheduleError::SubMinute);
    }

    fields
        .into_iter()
        .enumerate()
        .map(|(position, field)| {
            if position == DAY_OF_WEEK {
                shift_days_of_week(field)
            } else {
                Ok(field.to_owned())
            }
        })
        .collect()
}

/// Moves a crontab day of week to the number the parser counts in.
///
/// A crontab counts Sunday as 0 and accepts 7 for Sunday as well. The Quartz
/// form counts Sunday as 1, so every number moves up by one. Without this,
/// `1-5` would mean Sunday to Thursday instead of Monday to Friday. Day names
/// and steps mean the same in both, so they stay as they are.
fn shift_days_of_week(field: &str) -> Result<String, ScheduleError> {
    let terms = field
        .split(',')
        .map(|term| {
            // A step divides a range, so only the range holds days.
            let (days, step) = match term.split_once('/') {
                Some((days, step)) => (days, Some(step)),
                None => (term, None),
            };
            let days = days
                .split('-')
                .map(shift_day_of_week)
                .collect::<Result<Vec<_>, _>>()?
                .join("-");
            Ok(match step {
                Some(step) => format!("{days}/{step}"),
                None => days,
            })
        })
        .collect::<Result<Vec<_>, ScheduleError>>()?;

    Ok(terms.join(","))
}

fn shift_day_of_week(day: &str) -> Result<String, ScheduleError> {
    if day.is_empty() || !day.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(day.to_owned());
    }
    let number = day
        .parse::<u32>()
        .map_err(|_| ScheduleError::DayOfWeek(day.to_owned()))?;
    if number > 7 {
        return Err(ScheduleError::DayOfWeek(day.to_owned()));
    }

    // Both 0 and 7 are Sunday, which the parser counts as 1.
    Ok((number % 7 + 1).to_string())
}

/// True when a field names days rather than every day.
fn restricts(field: &str) -> bool {
    field != "*" && field != "?"
}

/// Rewrites one field of an expression.
fn with_field(fields: &[String], position: usize, value: &str) -> String {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| if index == position { value } else { field })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A parsed schedule is large, so it lives behind a pointer.
fn parse(expression: &str) -> Result<Box<cron::Schedule>, ScheduleError> {
    cron::Schedule::from_str(expression)
        .map(Box::new)
        .map_err(|error| ScheduleError::Expression(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::CronSchedule;
    use crate::error::ScheduleError;
    use crate::instant::{format_instant, parse_instant};
    use crate::offset::UtcOffset;

    /// The next `count` fires after the start of 2026-09-09, as UTC text.
    fn fires(expression: &str, count: usize) -> Vec<String> {
        fires_in(expression, UtcOffset::UTC, count)
    }

    fn fires_in(expression: &str, offset: UtcOffset, count: usize) -> Vec<String> {
        let schedule = CronSchedule::new(expression, offset).unwrap();
        let mut time = parse_instant("2026-09-09T00:00:00Z").unwrap();
        let mut fires = Vec::new();
        for _ in 0..count {
            let Some(next) = schedule.next_after(time) else {
                break;
            };
            fires.push(format_instant(next));
            time = next;
        }

        fires
    }

    #[test]
    fn reads_the_five_field_crontab_form() {
        assert_eq!(
            fires("0 3 * * *", 2),
            ["2026-09-09T03:00:00Z", "2026-09-10T03:00:00Z"]
        );
    }

    #[test]
    fn reads_the_six_field_form_the_same_way() {
        assert_eq!(fires("0 0 3 * * *", 2), fires("0 3 * * *", 2));
    }

    #[test]
    fn reads_the_seven_field_form_with_a_year() {
        assert_eq!(fires("0 0 3 1 1 * 2027", 2), ["2027-01-01T03:00:00Z"]);
    }

    #[test]
    fn reads_the_macros() {
        assert_eq!(
            fires("@daily", 2),
            ["2026-09-10T00:00:00Z", "2026-09-11T00:00:00Z"]
        );
        assert_eq!(
            fires("@hourly", 2),
            ["2026-09-09T01:00:00Z", "2026-09-09T02:00:00Z"]
        );
    }

    #[test]
    fn reads_ranges_lists_and_steps() {
        assert_eq!(
            fires("*/15 9 * * *", 3),
            [
                "2026-09-09T09:00:00Z",
                "2026-09-09T09:15:00Z",
                "2026-09-09T09:30:00Z"
            ]
        );
        assert_eq!(
            fires("0 9,17 * * Mon-Fri", 3),
            [
                "2026-09-09T09:00:00Z",
                "2026-09-09T17:00:00Z",
                "2026-09-10T09:00:00Z"
            ]
        );
    }

    /// A crontab reads a restricted day of month and day of week as either
    /// day, so this fires on every 13th and on every Friday.
    #[test]
    fn reads_a_day_of_month_and_a_day_of_week_as_either_day() {
        assert_eq!(
            fires("0 3 13 * Fri", 4),
            [
                "2026-09-11T03:00:00Z",
                "2026-09-13T03:00:00Z",
                "2026-09-18T03:00:00Z",
                "2026-09-25T03:00:00Z"
            ]
        );
    }

    #[test]
    fn keeps_one_restricted_day_field_as_it_is() {
        assert_eq!(fires("0 3 13 * *", 1), ["2026-09-13T03:00:00Z"]);
        assert_eq!(fires("0 3 * * Fri", 1), ["2026-09-11T03:00:00Z"]);
    }

    #[test]
    fn fires_a_restricted_day_of_week_in_the_named_offset() {
        // 03:00 in +02:00 is 01:00 UTC.
        assert_eq!(
            fires_in("0 3 * * *", "+02:00".parse().unwrap(), 2),
            ["2026-09-09T01:00:00Z", "2026-09-10T01:00:00Z"]
        );
        assert_eq!(
            fires_in("0 3 * * *", "-05:00".parse().unwrap(), 1),
            ["2026-09-09T08:00:00Z"]
        );
    }

    /// A crontab counts Sunday as 0, and accepts 7 for Sunday as well.
    #[test]
    fn counts_days_of_week_from_sunday_as_zero() {
        // 2026-09-09 is a Wednesday.
        assert_eq!(fires("0 3 * * 0", 1), ["2026-09-13T03:00:00Z"]);
        assert_eq!(fires("0 3 * * 7", 1), ["2026-09-13T03:00:00Z"]);
        assert_eq!(fires("0 3 * * 1", 1), ["2026-09-14T03:00:00Z"]);
        assert_eq!(fires("0 3 * * 6", 1), ["2026-09-12T03:00:00Z"]);
    }

    #[test]
    fn reads_a_numbered_day_of_week_as_a_named_one() {
        assert_eq!(fires("0 3 * * 1-5", 3), fires("0 3 * * Mon-Fri", 3));
        assert_eq!(fires("0 3 * * 6,0", 3), fires("0 3 * * Sat,Sun", 3));
        assert_eq!(fires("0 3 * * 1-5/2", 3), fires("0 3 * * Mon,Wed,Fri", 3));
    }

    #[test]
    fn rejects_a_day_of_week_outside_the_week() {
        assert_eq!(
            CronSchedule::new("0 3 * * 8", UtcOffset::UTC),
            Err(ScheduleError::DayOfWeek("8".to_owned()))
        );
    }

    #[test]
    fn fires_strictly_after_the_time_it_is_given() {
        let schedule = CronSchedule::new("0 3 * * *", UtcOffset::UTC).unwrap();
        let fire = parse_instant("2026-09-09T03:00:00Z").unwrap();

        assert_eq!(
            schedule.next_after(fire).map(format_instant),
            Some("2026-09-10T03:00:00Z".to_owned())
        );
    }

    #[test]
    fn stops_after_the_last_year_it_names() {
        let schedule = CronSchedule::new("0 0 3 1 1 * 2020", UtcOffset::UTC).unwrap();
        let time = parse_instant("2026-09-09T00:00:00Z").unwrap();

        assert_eq!(schedule.next_after(time), None);
    }

    #[test]
    fn rejects_a_schedule_that_fires_more_than_once_a_minute() {
        for expression in [
            "* * * * * *",
            "*/15 * * * * *",
            "0,30 * * * * *",
            "0-5 * * * * *",
        ] {
            assert_eq!(
                CronSchedule::new(expression, UtcOffset::UTC),
                Err(ScheduleError::SubMinute),
                "{expression} was accepted"
            );
        }
    }

    #[test]
    fn rejects_an_expression_of_the_wrong_length() {
        assert_eq!(
            CronSchedule::new("0 3 * *", UtcOffset::UTC),
            Err(ScheduleError::FieldCount(4))
        );
        assert_eq!(
            CronSchedule::new("0 0 3 * * * 2027 1", UtcOffset::UTC),
            Err(ScheduleError::FieldCount(8))
        );
        assert_eq!(
            CronSchedule::new("   ", UtcOffset::UTC),
            Err(ScheduleError::Empty)
        );
    }

    #[test]
    fn rejects_a_field_the_parser_does_not_know() {
        assert!(matches!(
            CronSchedule::new("0 3 * * Funday", UtcOffset::UTC),
            Err(ScheduleError::Expression(_))
        ));
    }

    #[test]
    fn writes_the_expression_back_as_it_was_written() {
        assert_eq!(
            CronSchedule::new("0 3 * * *", UtcOffset::UTC)
                .unwrap()
                .to_string(),
            "0 3 * * *"
        );
        assert_eq!(
            CronSchedule::new("0 3 * * *", "+02:00".parse().unwrap())
                .unwrap()
                .to_string(),
            "0 3 * * * +02:00"
        );
    }
}
