use std::collections::BTreeSet;

use beez_schedule::{CronSchedule, Schedule, UtcOffset, parse_instant};
use serde::Deserialize;

use crate::message;

/// A manifest's `schedule` field: one entry, or a list of entries.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum ScheduleSetting {
    One(Box<ScheduleFields>),
    Many(Vec<ScheduleFields>),
}

/// One schedule as written in a manifest.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScheduleFields {
    name: Option<String>,
    cron: Option<String>,
    at: Option<String>,
    offset: Option<String>,
    #[serde(default)]
    on_overlap: Overlap,
    #[serde(default)]
    catch_up: bool,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    allow_unsandboxed: bool,
}

/// What the daemon does when a schedule fires while its own last run still runs.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    /// Drops the fire and reports it.
    #[default]
    Skip,
    /// Runs once more as soon as the running run ends.
    Queue,
    /// Starts another run beside the running one.
    Parallel,
}

/// One schedule of a manifest, with the policies for the runs it starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobSchedule {
    name: Option<String>,
    schedule: Schedule,
    on_overlap: Overlap,
    catch_up: bool,
    enabled: bool,
    allow_unsandboxed: bool,
}

impl JobSchedule {
    /// The name the manifest gives this schedule, when it names one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// When the workflow runs.
    #[must_use]
    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// What to do when a fire meets a run that is still going.
    #[must_use]
    pub fn on_overlap(&self) -> Overlap {
        self.on_overlap
    }

    /// True when a fire the daemon missed still runs once, late.
    #[must_use]
    pub fn catch_up(&self) -> bool {
        self.catch_up
    }

    /// False keeps the schedule in the manifest without firing it.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// True lets the daemon run tasks that no sandbox limits.
    ///
    /// A scheduled run starts without a person watching it, so the daemon
    /// refuses an unsandboxed workflow unless the schedule allows it.
    #[must_use]
    pub fn allow_unsandboxed(&self) -> bool {
        self.allow_unsandboxed
    }
}

impl TryFrom<ScheduleFields> for JobSchedule {
    type Error = String;

    fn try_from(fields: ScheduleFields) -> Result<Self, Self::Error> {
        let offset = fields
            .offset
            .as_deref()
            .map(str::parse::<UtcOffset>)
            .transpose()
            .map_err(message)?;
        let schedule = match (fields.cron, fields.at) {
            (Some(_), Some(_)) => {
                return Err("schedule must define either cron or at, not both".into());
            }
            (None, None) => return Err("schedule must define either cron or at".into()),
            (Some(cron), None) => Schedule::Cron(
                CronSchedule::new(&cron, offset.unwrap_or_default()).map_err(message)?,
            ),
            (None, Some(at)) => {
                if offset.is_some() {
                    return Err("at already names its own offset, so offset must be absent".into());
                }
                Schedule::Once(parse_instant(&at).map_err(message)?)
            }
        };

        Ok(Self {
            name: fields.name,
            schedule,
            on_overlap: fields.on_overlap,
            catch_up: fields.catch_up,
            enabled: fields.enabled,
            allow_unsandboxed: fields.allow_unsandboxed,
        })
    }
}

/// Reads every schedule of a manifest, in the order it wrote them.
pub(crate) fn resolve(setting: Option<ScheduleSetting>) -> Result<Vec<JobSchedule>, String> {
    let fields = match setting {
        None => return Ok(Vec::new()),
        Some(ScheduleSetting::One(fields)) => vec![*fields],
        Some(ScheduleSetting::Many(fields)) => fields,
    };
    let schedules = fields
        .into_iter()
        .map(JobSchedule::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    // A name tells one schedule of a manifest from another, so it must be there.
    if schedules.len() > 1 && schedules.iter().any(|schedule| schedule.name.is_none()) {
        return Err("a manifest with more than one schedule must name each of them".into());
    }
    if let Some(name) = repeated_name(&schedules) {
        return Err(format!("schedule name {name} is used twice"));
    }

    Ok(schedules)
}

fn repeated_name(schedules: &[JobSchedule]) -> Option<&str> {
    let mut seen = BTreeSet::new();
    schedules
        .iter()
        .filter_map(JobSchedule::name)
        .find(|name| !seen.insert(*name))
}

fn default_true() -> bool {
    true
}
