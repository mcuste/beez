//! Prints what the daemon holds.

use std::io::{self, Write};

use beez_daemon::{JobReport, Response, Status};
use beez_record::status_text;

/// Columns of the job table, in the order they are printed.
const HEADINGS: [&str; 5] = ["JOB", "CONDITION", "SCHEDULE", "NEXT FIRE", "LAST RUN"];

/// Prints the daemon's own line, then its jobs.
pub(crate) fn status(status: &Status) -> io::Result<()> {
    let jobs = match status {
        Status::Running(Response::Status {
            pid,
            started,
            running,
            jobs,
        }) => {
            println!("Daemon running, pid {pid}, started {started}, {running} running");
            jobs
        }
        Status::Running(Response::Done { message } | Response::Error { message }) => {
            return Err(io::Error::other(message.clone()));
        }
        Status::Stopped(jobs) => {
            println!("Daemon not running");
            jobs
        }
    };

    table(jobs)
}

/// Prints one row for each job, and the reason under a broken one.
pub(crate) fn table(jobs: &[JobReport]) -> io::Result<()> {
    if jobs.is_empty() {
        println!("No manifest is watched. Add one with: beez schedule add <manifest>");
        return Ok(());
    }
    let rows: Vec<[String; 5]> = jobs.iter().map(row).collect();
    let widths = widths(&rows);
    let mut out = io::stdout().lock();
    write_row(&mut out, &HEADINGS.map(str::to_owned), &widths)?;
    for (row, job) in rows.iter().zip(jobs) {
        write_row(&mut out, row, &widths)?;
        if let Some(error) = &job.error {
            writeln!(out, "  {error}")?;
        }
    }

    out.flush()
}

fn row(job: &JobReport) -> [String; 5] {
    [
        job.id.clone(),
        job.condition.clone(),
        or_dash(job.schedule.as_deref()),
        or_dash(job.next_fire.as_deref()),
        last_run(job),
    ]
}

fn last_run(job: &JobReport) -> String {
    let Some(directory) = &job.last_run else {
        return or_dash(None);
    };
    match job.last_status {
        // No status means the run never reached its end, so there is none to name.
        None => format!("{directory} (unfinished)"),
        status => format!("{directory} ({})", status_text(status)),
    }
}

/// The value of a column, or a dash when the job has none.
fn or_dash(value: Option<&str>) -> String {
    value.unwrap_or("-").to_owned()
}

/// As wide as the longest value of each column, the heading included.
fn widths(rows: &[[String; 5]]) -> [usize; 5] {
    let mut widths = HEADINGS.map(str::len);
    for row in rows {
        for (width, value) in widths.iter_mut().zip(row) {
            *width = (*width).max(value.len());
        }
    }

    widths
}

/// Writes one row, with no padding after the last column.
fn write_row(out: &mut impl Write, row: &[String; 5], widths: &[usize; 5]) -> io::Result<()> {
    let last = row.len().saturating_sub(1);
    for (index, (value, width)) in row.iter().zip(widths).enumerate() {
        if index == last {
            writeln!(out, "{value}")?;
        } else {
            write!(out, "{value:<width$}  ")?;
        }
    }

    Ok(())
}
