//! Tests the daemon that runs workflows on a schedule.

#![cfg(unix)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::thread::sleep;
use std::time::{Duration, Instant};

use loom_test_support::TemporaryDirectory;

mod common;

/// Longest a test waits for the daemon to reach a state.
const LIMIT: Duration = Duration::from_secs(20);
const STEP: Duration = Duration::from_millis(50);
/// How far ahead a test puts a fire, so a busy machine still reaches it in time.
const LEAD: Duration = Duration::from_secs(3);

fn manifest(directory: &TemporaryDirectory, name: &str, source: &str) -> io::Result<PathBuf> {
    let path = directory.join(&format!("{name}.yaml"));
    fs::write(&path, source)?;

    Ok(path)
}

/// A workflow of one task that writes a file, with the given schedule.
///
/// The task opts out of the sandbox, so these tests need no sandbox on the host.
fn ticking(schedule: &str, marker: &Path) -> String {
    format!(
        "schedule:\n{schedule}  allow_unsandboxed: true\ntasks:\n  - id: mark\n    sandbox: false\n    command: [bash, -c, \"echo fired >> {}\"]\n",
        marker.display()
    )
}

fn loom(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    common::loom()
        .args(arguments)
        .arg("--root")
        .arg(root)
        .output()
}

/// Watches one manifest, whether or not a daemon runs.
fn add(root: &Path, path: &Path) -> io::Result<Output> {
    common::loom()
        .args(["schedule", "add"])
        .arg(path)
        .arg("--root")
        .arg(root)
        .output()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Waits for `condition`, and reports what the daemon logged if it never holds.
fn wait_for(root: &Path, what: &str, condition: impl Fn() -> bool) -> io::Result<()> {
    let started = Instant::now();
    while started.elapsed() < LIMIT {
        if condition() {
            return Ok(());
        }
        sleep(STEP);
    }
    let log = fs::read_to_string(root.join("daemon/daemon.log")).unwrap_or_default();

    Err(io::Error::other(format!("the daemon never {what}\n{log}")))
}

/// A running daemon that stops when the test ends, even when the test fails.
struct Running {
    root: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        // A test that leaves a daemon behind would outlive its own directory.
        let _ = stop(&self.root);
    }
}

fn start(root: &Path) -> io::Result<Running> {
    let output = loom(root, &["daemon", "start"])?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "the daemon did not start: {output:?}"
        )));
    }
    wait_for(root, "answered", || {
        loom(root, &["daemon", "status"])
            .is_ok_and(|output| stdout(&output).contains("Daemon running"))
    })?;

    Ok(Running {
        root: root.to_path_buf(),
    })
}

fn stop(root: &Path) -> io::Result<()> {
    loom(root, &["daemon", "stop"])?;

    wait_for(root, "stopped", || {
        !root.join("daemon/daemon.sock").exists()
    })
}

fn runs(root: &Path) -> usize {
    fs::read_dir(root.join("runs")).map_or(0, Iterator::count)
}

#[test]
fn watches_a_manifest_before_the_daemon_starts() {
    let directory = TemporaryDirectory::new("daemon-add").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &directory.join("marker")),
    )
    .unwrap();

    let output = add(&root, &path).unwrap();
    let listed = loom(&root, &["schedule", "list"]).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&listed).contains("Daemon not running"), "{listed:?}");
    assert!(stdout(&listed).contains("nightly"), "{listed:?}");
    assert!(stdout(&listed).contains("0 3 * * *"), "{listed:?}");
}

#[test]
fn pauses_and_removes_a_job_before_the_daemon_starts() {
    let directory = TemporaryDirectory::new("daemon-stopped-commands").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &directory.join("marker")),
    )
    .unwrap();
    let path = path.to_string_lossy();
    add(&root, Path::new(&*path)).unwrap();

    let paused = loom(&root, &["schedule", "pause", "nightly"]).unwrap();
    let held = loom(&root, &["schedule", "list"]).unwrap();
    let resumed = loom(&root, &["schedule", "resume", "nightly"]).unwrap();
    let released = loom(&root, &["schedule", "list"]).unwrap();
    let removed = loom(&root, &["schedule", "remove", &path]).unwrap();
    let again = loom(&root, &["schedule", "remove", &path]).unwrap();
    let listed = loom(&root, &["schedule", "list"]).unwrap();

    assert!(paused.status.success(), "{paused:?}");
    assert!(stdout(&held).contains("paused"), "{held:?}");
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(stdout(&released).contains("waiting"), "{released:?}");
    assert!(stdout(&removed).contains("stopped watching"), "{removed:?}");
    assert!(!again.status.success(), "{again:?}");
    assert!(!stdout(&listed).contains("nightly"), "{listed:?}");
}

#[test]
fn refuses_a_manifest_that_names_no_schedule() {
    let directory = TemporaryDirectory::new("daemon-no-schedule").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "plain",
        "tasks:\n  - id: mark\n    command: [bash, -c, \"true\"]\n",
    )
    .unwrap();

    let output = add(&root, &path).unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no schedule section"),
        "{output:?}"
    );
}

#[test]
fn refuses_a_job_whose_tasks_no_sandbox_limits() {
    let directory = TemporaryDirectory::new("daemon-unsandboxed").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "open",
        "schedule:\n  cron: \"0 3 * * *\"\ntasks:\n  - id: mark\n    sandbox: false\n    command: [bash, -c, \"true\"]\n",
    )
    .unwrap();
    add(&root, &path).unwrap();

    let listed = loom(&root, &["schedule", "list"]).unwrap();

    assert!(stdout(&listed).contains("broken"), "{listed:?}");
    assert!(stdout(&listed).contains("allow_unsandboxed"), "{listed:?}");
}

#[test]
fn runs_a_job_on_request_and_writes_the_same_artifacts_as_a_run() {
    let directory = TemporaryDirectory::new("daemon-trigger").unwrap();
    let root = directory.join("root");
    let marker = directory.join("marker");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &marker),
    )
    .unwrap();
    add(&root, &path).unwrap();
    let daemon = start(&root).unwrap();

    let triggered = loom(&root, &["schedule", "trigger", "nightly"]).unwrap();
    wait_for(&root, "ran the job", || marker.exists()).unwrap();
    wait_for(&root, "recorded the run", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("(ok)"))
    })
    .unwrap();
    drop(daemon);

    assert!(triggered.status.success(), "{triggered:?}");
    let run = fs::read_dir(root.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let log = fs::read_to_string(run.join("run.log")).unwrap();
    // The run says which fire started it, and holds the lines of every run.
    assert!(log.contains("Trigger   nightly scheduled for"), "{log}");
    assert!(log.contains("Running   mark"), "{log}");
    assert!(log.contains("Finished  mark in"), "{log}");
    assert!(log.contains("Summary   1 passed in"), "{log}");
    assert!(run.join("run.json").exists());
    assert!(run.join("tasks/mark.stdout").exists());
}

#[test]
fn fires_a_one_time_schedule_once() {
    let directory = TemporaryDirectory::new("daemon-once").unwrap();
    let root = directory.join("root");
    let marker = directory.join("marker");
    let daemon = start(&root).unwrap();
    // The daemon runs already, so the whole lead time is its to wait.
    let at = loom_schedule::format_instant(std::time::SystemTime::now() + LEAD);
    let path = manifest(
        &directory,
        "once",
        &ticking(&format!("  at: \"{at}\"\n"), &marker),
    )
    .unwrap();
    add(&root, &path).unwrap();

    wait_for(&root, "fired the one-time schedule", || marker.exists()).unwrap();
    wait_for(&root, "finished the run", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("done"))
    })
    .unwrap();
    let listed = loom(&root, &["schedule", "list"]).unwrap();
    drop(daemon);

    // A one-time schedule has nothing left to fire.
    assert_eq!(runs(&root), 1);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "fired\n");
    assert!(stdout(&listed).contains("done"), "{listed:?}");
}

#[test]
fn runs_a_fire_it_missed_only_when_the_schedule_asks_for_it() {
    let directory = TemporaryDirectory::new("daemon-catch-up").unwrap();
    let root = directory.join("root");
    let marker = directory.join("marker");
    // The fire is already past, so it is missed as soon as the daemon starts.
    let at = loom_schedule::format_instant(std::time::SystemTime::now() - Duration::from_secs(60));
    let missed = manifest(
        &directory,
        "missed",
        &ticking(&format!("  at: \"{at}\"\n"), &marker),
    )
    .unwrap();
    let caught = manifest(
        &directory,
        "caught",
        &ticking(&format!("  at: \"{at}\"\n  catch_up: true\n"), &marker),
    )
    .unwrap();
    for path in [&missed, &caught] {
        add(&root, path).unwrap();
    }
    let daemon = start(&root).unwrap();

    wait_for(&root, "caught up", || marker.exists()).unwrap();
    wait_for(&root, "finished the run", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("(ok)"))
    })
    .unwrap();
    drop(daemon);
    let log = fs::read_to_string(root.join("daemon/daemon.log")).unwrap();

    // Only the schedule that asks for it runs late.
    assert_eq!(runs(&root), 1);
    assert!(log.contains("Catch-up  caught missed"), "{log}");
    assert!(log.contains("Missed    missed missed"), "{log}");
}

#[test]
fn reads_a_manifest_again_after_it_changes() {
    let directory = TemporaryDirectory::new("daemon-reload").unwrap();
    let root = directory.join("root");
    let marker = directory.join("marker");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &marker),
    )
    .unwrap();
    add(&root, &path).unwrap();
    let daemon = start(&root).unwrap();

    fs::write(&path, ticking("  cron: \"0 5 * * *\"\n", &marker)).unwrap();
    wait_for(&root, "read the manifest again", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("0 5 * * *"))
    })
    .unwrap();
    let listed = loom(&root, &["schedule", "list"]).unwrap();
    drop(daemon);

    assert!(stdout(&listed).contains("0 5 * * *"), "{listed:?}");
}

/// A mistake in a manifest must not lose what the job already did. A broken
/// manifest holds no named jobs for a moment, so their history is easy to drop.
#[test]
fn keeps_the_history_of_a_job_whose_manifest_stops_loading() {
    let directory = TemporaryDirectory::new("daemon-broken").unwrap();
    let root = directory.join("root");
    let marker = directory.join("marker");
    let source = format!(
        "schedule:\n  - name: weekday\n    cron: \"0 3 * * *\"\n    allow_unsandboxed: true\ntasks:\n  - id: mark\n    sandbox: false\n    command: [bash, -c, \"echo fired >> {}\"]\n",
        marker.display()
    );
    let path = manifest(&directory, "nightly", &source).unwrap();
    add(&root, &path).unwrap();
    let daemon = start(&root).unwrap();
    loom(&root, &["schedule", "trigger", "nightly:weekday"]).unwrap();
    wait_for(&root, "recorded the run", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("(ok)"))
    })
    .unwrap();

    fs::write(&path, "tasks: [{{{\n").unwrap();
    wait_for(&root, "reported the broken manifest", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("broken"))
    })
    .unwrap();
    fs::write(&path, &source).unwrap();
    wait_for(&root, "read the fixed manifest again", || {
        loom(&root, &["schedule", "list"]).is_ok_and(|output| stdout(&output).contains("waiting"))
    })
    .unwrap();
    let listed = loom(&root, &["schedule", "list"]).unwrap();
    drop(daemon);

    assert!(stdout(&listed).contains("nightly:weekday"), "{listed:?}");
    assert!(stdout(&listed).contains("(ok)"), "{listed:?}");
}

/// The list of watched manifests is a file, so a daemon that already runs must
/// read it again on request, not only the manifests it already knows.
#[test]
fn reads_the_list_of_watched_manifests_again_on_request() {
    let directory = TemporaryDirectory::new("daemon-reload-list").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &directory.join("marker")),
    )
    .unwrap();
    let daemon = start(&root).unwrap();
    let before = loom(&root, &["schedule", "list"]).unwrap();

    // A daemon of another root, or a hand, can write this file.
    fs::write(
        root.join("daemon/manifests.json"),
        format!(
            "{{\"schema\":1,\"manifests\":[{{\"path\":\"{}\",\"working_directory\":\"{}\"}}]}}\n",
            path.display(),
            directory.path().display()
        ),
    )
    .unwrap();
    let reloaded = loom(&root, &["daemon", "reload"]).unwrap();
    let after = loom(&root, &["schedule", "list"]).unwrap();
    drop(daemon);

    assert!(
        stdout(&before).contains("No manifest is watched"),
        "{before:?}"
    );
    assert!(reloaded.status.success(), "{reloaded:?}");
    assert!(stdout(&after).contains("nightly"), "{after:?}");
    assert!(stdout(&after).contains("0 3 * * *"), "{after:?}");
}

#[test]
fn holds_a_paused_job_back_and_lets_it_go_again() {
    let directory = TemporaryDirectory::new("daemon-pause").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &directory.join("marker")),
    )
    .unwrap();
    add(&root, &path).unwrap();
    let daemon = start(&root).unwrap();

    let paused = loom(&root, &["schedule", "pause", "nightly"]).unwrap();
    let held = loom(&root, &["schedule", "list"]).unwrap();
    let resumed = loom(&root, &["schedule", "resume", "nightly"]).unwrap();
    let released = loom(&root, &["schedule", "list"]).unwrap();
    let unknown = loom(&root, &["schedule", "pause", "nothing"]).unwrap();
    drop(daemon);

    assert!(paused.status.success(), "{paused:?}");
    assert!(stdout(&held).contains("paused"), "{held:?}");
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(stdout(&released).contains("waiting"), "{released:?}");
    assert!(!unknown.status.success());
}

#[test]
fn removes_the_oldest_runs_when_the_root_holds_too_many() {
    let directory = TemporaryDirectory::new("daemon-sweep").unwrap();
    let root = directory.join("root");
    let path = manifest(
        &directory,
        "nightly",
        &ticking("  cron: \"0 3 * * *\"\n", &directory.join("marker")),
    )
    .unwrap();
    add(&root, &path).unwrap();
    // Runs of an earlier daemon, named after the times they started.
    for name in ["20260908T030000000Z-1", "20260909T030000000Z-2"] {
        fs::create_dir_all(root.join("runs").join(name)).unwrap();
    }

    let daemon = common::loom()
        .args(["daemon", "start", "--keep-runs", "1", "--root"])
        .arg(&root)
        .output()
        .unwrap();
    wait_for(&root, "swept the runs", || runs(&root) == 1).unwrap();
    let _ = stop(&root);

    assert!(daemon.status.success(), "{daemon:?}");
    assert!(root.join("runs/20260909T030000000Z-2").exists());
    assert!(!root.join("runs/20260908T030000000Z-1").exists());
}

#[test]
fn keeps_a_second_daemon_out_of_one_root() {
    let directory = TemporaryDirectory::new("daemon-single").unwrap();
    let root = directory.join("root");
    let daemon = start(&root).unwrap();

    let second = loom(&root, &["daemon", "start"]).unwrap();
    drop(daemon);

    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already runs"),
        "{second:?}"
    );
}
