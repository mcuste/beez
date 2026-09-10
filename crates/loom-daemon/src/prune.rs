//! Removes the oldest runs.
//!
//! A daemon writes a run every time a job fires, so a directory that is never
//! swept grows without end. Run names carry the time they started and sort by
//! it, so the newest are the last ones.

use std::fs;
use std::io;
use std::path::Path;

/// Directory that holds the runs, inside the Loom root.
const RUNS: &str = "runs";

/// Removes every run but the newest `keep`, and reports how many went.
///
/// `keep` of zero keeps every run. A run directory that cannot be removed is
/// left alone, because a sweep must never stop the daemon.
pub(crate) fn prune(root: &Path, keep: usize) -> io::Result<usize> {
    if keep == 0 {
        return Ok(0);
    }
    let runs = match fs::read_dir(root.join(RUNS)) {
        Ok(runs) => runs,
        // A root that never held a run has nothing to sweep.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in runs {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // A symbolic link such as `latest` is not a run, and never goes.
        if is_run_name(&name) && entry.file_type()?.is_dir() {
            names.push(name);
        }
    }
    if names.len() <= keep {
        return Ok(0);
    }
    names.sort();
    let removing = names.len() - keep;
    let mut removed = 0;
    for name in names.into_iter().take(removing) {
        if fs::remove_dir_all(root.join(RUNS).join(name)).is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

/// True for a name Loom gives a run, such as `20260910T030000004Z-4123`.
fn is_run_name(name: &str) -> bool {
    let Some((stamp, pid)) = name.split_once('-') else {
        return false;
    };

    stamp.len() == 19
        && stamp.ends_with('Z')
        && stamp.get(8..9) == Some("T")
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{is_run_name, prune};
    use loom_test_support::TemporaryDirectory;
    use std::fs;

    fn runs(root: &TemporaryDirectory, names: &[&str]) {
        for name in names {
            fs::create_dir_all(root.join(&format!("runs/{name}"))).unwrap();
        }
    }

    fn remaining(root: &TemporaryDirectory) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root.join("runs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();

        names
    }

    #[test]
    fn keeps_the_newest_runs_and_removes_the_rest() {
        let root = TemporaryDirectory::new("prune-oldest").unwrap();
        runs(
            &root,
            &[
                "20260908T030000000Z-1",
                "20260909T030000000Z-2",
                "20260910T030000000Z-3",
            ],
        );

        assert_eq!(prune(root.path(), 2).unwrap(), 1);
        assert_eq!(
            remaining(&root),
            ["20260909T030000000Z-2", "20260910T030000000Z-3"]
        );
    }

    #[test]
    fn sweeps_a_root_that_never_held_a_run() {
        let root = TemporaryDirectory::new("prune-empty").unwrap();

        assert_eq!(prune(root.path(), 2).unwrap(), 0);
    }

    #[test]
    fn keeps_every_run_when_there_are_few_enough_or_the_count_is_zero() {
        let root = TemporaryDirectory::new("prune-few").unwrap();
        runs(&root, &["20260910T030000000Z-3"]);

        assert_eq!(prune(root.path(), 2).unwrap(), 0);
        assert_eq!(prune(root.path(), 0).unwrap(), 0);
        assert_eq!(remaining(&root), ["20260910T030000000Z-3"]);
    }

    /// Only a run goes. Anything else in the directory stays.
    #[test]
    fn leaves_what_is_not_a_run_alone() {
        let root = TemporaryDirectory::new("prune-other").unwrap();
        runs(
            &root,
            &["20260908T030000000Z-1", "notes", "20260909T030000000Z-2"],
        );

        assert_eq!(prune(root.path(), 1).unwrap(), 1);
        assert_eq!(remaining(&root), ["20260909T030000000Z-2", "notes"]);
    }

    #[test]
    fn reads_the_name_loom_gives_a_run() {
        assert!(is_run_name("20260910T030000004Z-4123"));
        assert!(!is_run_name("latest"));
        assert!(!is_run_name("20260910T030000004Z"));
        assert!(!is_run_name("20260910T030000004Z-"));
        assert!(!is_run_name("20260910T030000004Z-abc"));
        assert!(!is_run_name("2026-09-10"));
    }
}
