//! A cron expression that keeps its crontab meaning.
//!
//! The `cron` crate parses the Quartz form of cron, which starts with a
//! seconds field and counts days of week from Sunday as 1. A crontab has no
//! seconds field and counts days of week from Sunday as 0. Every expression
//! moves to the Quartz form here, so the rest of Beez means crontab.

use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use crate::error::ScheduleError;
use crate::offset::UtcOffset;
use crate::text::is_ascii_digits;

/// A cron expression and the offset it is read in.
///
/// Beez accepts the five-field crontab form, the six-field form that starts
/// with seconds, the seven-field form that ends with a year, and the `@daily`
/// style macros.
///
/// Days of week count from Sunday as 0, and 7 is Sunday as well.
///
/// A crontab reads a restricted day of month and a restricted day of week as
/// "either day", so `0 3 13 * Fri` means the 13th of any month and every
/// Friday. Beez keeps that meaning, which needs two schedules, because the
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
            let (primary, alternate) = Fields::parse(text)?.split_days();
            (
                parse(&primary.to_string())?,
                alternate
                    .map(|fields| parse(&fields.to_string()))
                    .transpose()?,
            )
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
        let start = DateTime::<Utc>::from(time).with_timezone(&self.offset.fixed());
        std::iter::once(&self.primary)
            .chain(&self.alternate)
            .filter_map(|schedule| schedule.after(&start).next())
            .min()
            .map(SystemTime::from)
    }
}

/// Two schedules are the same when they are written the same way.
impl PartialEq for CronSchedule {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.offset == other.offset
    }
}

impl Eq for CronSchedule {}

impl fmt::Display for CronSchedule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)?;
        if self.offset != UtcOffset::UTC {
            write!(formatter, " {}", self.offset)?;
        }
        Ok(())
    }
}

/// The fields of an expression in the form the parser reads.
struct Fields<'a> {
    /// Always one fixed value.
    seconds: &'a str,
    minute: &'a str,
    hour: &'a str,
    day_of_month: &'a str,
    month: &'a str,
    /// Counts from Sunday as 1, as the parser does.
    day_of_week: String,
    year: Option<&'a str>,
}

impl<'a> Fields<'a> {
    /// Puts an expression in the form the parser reads.
    ///
    /// A crontab expression has no seconds field, so it gains one that holds
    /// the start of the minute, and its days of week move to the numbers the
    /// parser counts in.
    fn parse(text: &'a str) -> Result<Self, ScheduleError> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        let (seconds, [minute, hour, day_of_month, month, day_of_week], year) = match fields[..] {
            [minute, hour, day_of_month, month, day_of_week] => {
                ("0", [minute, hour, day_of_month, month, day_of_week], None)
            }
            [seconds, minute, hour, day_of_month, month, day_of_week] => (
                seconds,
                [minute, hour, day_of_month, month, day_of_week],
                None,
            ),
            [
                seconds,
                minute,
                hour,
                day_of_month,
                month,
                day_of_week,
                year,
            ] => (
                seconds,
                [minute, hour, day_of_month, month, day_of_week],
                Some(year),
            ),
            _ => return Err(ScheduleError::FieldCount(fields.len())),
        };
        // A workflow run must not start more than once a minute.
        if !is_ascii_digits(seconds) {
            return Err(ScheduleError::SubMinute);
        }

        Ok(Self {
            seconds,
            minute,
            hour,
            day_of_month,
            month,
            day_of_week: shift_days_of_week(day_of_week)?,
            year,
        })
    }

    /// Splits a restricted day of month and day of week into two schedules.
    ///
    /// A crontab reads both as "either day", but the parser keeps only the
    /// days both fields hold. The first half keeps the day of month and the
    /// second, when there is one, keeps the day of week.
    fn split_days(self) -> (Self, Option<Self>) {
        if !(restricts(self.day_of_month) && restricts(&self.day_of_week)) {
            return (self, None);
        }
        // Every field the update copies is borrowed, so `self` stays whole.
        let day_of_month = Self {
            day_of_week: "*".to_owned(),
            ..self
        };
        let day_of_week = Self {
            day_of_month: "*",
            ..self
        };
        (day_of_month, Some(day_of_week))
    }
}

impl fmt::Display for Fields<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} {} {} {} {}",
            self.seconds, self.minute, self.hour, self.day_of_month, self.month, self.day_of_week
        )?;
        if let Some(year) = &self.year {
            write!(formatter, " {year}")?;
        }
        Ok(())
    }
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
        .map(shift_term)
        .collect::<Result<Vec<_>, ScheduleError>>()?;

    Ok(terms.join(","))
}

/// Moves one comma-separated term of a day-of-week field.
fn shift_term(term: &str) -> Result<String, ScheduleError> {
    // A step divides a range, so only the range holds days.
    let (days, step) = match term.split_once('/') {
        Some((days, step)) => (days, Some(step)),
        None => (term, None),
    };
    if let Some(days) = list_range_ending_on_sunday(days, step)? {
        return Ok(days);
    }
    let days = days
        .split('-')
        .map(shift_day_of_week)
        .collect::<Result<Vec<_>, _>>()?
        .join("-");
    Ok(match step {
        Some(step) => format!("{days}/{step}"),
        None => days,
    })
}

/// Lists the days of a numbered range that ends on 7, such as `5-7`.
///
/// Sunday is 7 in a crontab but 1 in the Quartz form, so the range would end
/// before it starts once shifted. Listing its days, after any step, keeps its
/// meaning. Returns nothing for every other term.
fn list_range_ending_on_sunday(
    days: &str,
    step: Option<&str>,
) -> Result<Option<String>, ScheduleError> {
    let Some((start, end)) = days.split_once('-') else {
        return Ok(None);
    };
    if !is_ascii_digits(start) || !is_ascii_digits(end) || day_number(end)? != 7 {
        return Ok(None);
    }
    let start = day_number(start)?;
    let step = match step {
        None => 1,
        // The parser reports a step that is not a positive number.
        Some(step) => match step.parse::<u32>() {
            Ok(step) if step > 0 => step,
            _ => return Ok(None),
        },
    };

    Ok(Some(
        (start..=7)
            .filter(|day| (day - start) % step == 0)
            .map(|day| quartz_day(day).to_string())
            .collect::<Vec<_>>()
            .join(","),
    ))
}

fn shift_day_of_week(day: &str) -> Result<String, ScheduleError> {
    if !is_ascii_digits(day) {
        return Ok(day.to_owned());
    }
    Ok(quartz_day(day_number(day)?).to_string())
}

/// Reads a crontab day number, which runs from 0 to 7.
fn day_number(day: &str) -> Result<u32, ScheduleError> {
    day.parse::<u32>()
        .ok()
        .filter(|number| *number <= 7)
        .ok_or_else(|| ScheduleError::DayOfWeek(day.to_owned()))
}

/// Both 0 and 7 are Sunday, which the parser counts as 1.
const fn quartz_day(day: u32) -> u32 {
    day % 7 + 1
}

/// True when a field names days rather than every day.
///
/// As in a crontab, a field that starts with `*`, such as `*/2`, counts as
/// every day, so it narrows the other day field instead of adding to it.
fn restricts(field: &str) -> bool {
    !field.starts_with('*') && field != "?"
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
    use crate::instant::{Utc, parse_instant};
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
            fires.push(Utc(next).to_string());
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

    /// A crontab reads a day field that starts with `*` as every day, so it
    /// narrows the other day field instead of adding to it.
    #[test]
    fn reads_a_starred_day_field_as_every_day() {
        // 2026-10-01 is a Thursday, the first 1st on an even day of week.
        assert_eq!(fires("0 3 1 * */2", 1), ["2026-10-01T03:00:00Z"]);
        // Only Fridays on odd days of the month.
        assert_eq!(
            fires("0 3 */2 * Fri", 2),
            ["2026-09-11T03:00:00Z", "2026-09-25T03:00:00Z"]
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

    /// A range may end on 7, the second name for Sunday.
    #[test]
    fn reads_a_day_of_week_range_that_ends_on_seven() {
        // 2026-09-09 is a Wednesday.
        assert_eq!(fires("0 3 * * 1-7", 7), fires("0 3 * * *", 7));
        assert_eq!(fires("0 3 * * 0-7", 7), fires("0 3 * * *", 7));
        assert_eq!(fires("0 3 * * 5-7", 4), fires("0 3 * * Fri,Sat,Sun", 4));
        assert_eq!(
            fires("0 3 * * 5-7", 4),
            [
                "2026-09-11T03:00:00Z",
                "2026-09-12T03:00:00Z",
                "2026-09-13T03:00:00Z",
                "2026-09-18T03:00:00Z"
            ]
        );
        assert_eq!(fires("0 3 * * 7-7", 2), fires("0 3 * * Sun", 2));
    }

    #[test]
    fn reads_a_stepped_day_of_week_range_that_ends_on_seven() {
        assert_eq!(
            fires("0 3 * * 1-7/2", 8),
            fires("0 3 * * Mon,Wed,Fri,Sun", 8)
        );
        assert_eq!(fires("0 3 * * 0-7/7", 4), fires("0 3 * * Sun", 4));
        assert_eq!(fires("0 3 * * 2-7/3,1", 6), fires("0 3 * * Mon,Tue,Fri", 6));
    }

    #[test]
    fn reads_a_day_of_week_range_that_starts_on_zero() {
        assert_eq!(fires("0 3 * * 0-6", 7), fires("0 3 * * *", 7));
        assert_eq!(fires("0 3 * * 0-2", 6), fires("0 3 * * Sun,Mon,Tue", 6));
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
            schedule.next_after(fire).map(|time| Utc(time).to_string()),
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

    /// The seconds check runs before the day-of-week check.
    #[test]
    fn reports_sub_minute_before_a_bad_day_of_week() {
        assert_eq!(
            CronSchedule::new("* * * * * 8", UtcOffset::UTC),
            Err(ScheduleError::SubMinute)
        );
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
