//! Checks that the installed harnesses accept the flags Loom sends them.
#![cfg(unix)]

use std::process::Command;

#[test]
fn pi_accepts_the_flags_loom_sends() {
    assert_harness_accepts("pi", "--effort");
}

#[test]
fn omp_accepts_the_flags_loom_sends() {
    assert_harness_accepts("omp", "--thinking");
}

fn assert_harness_accepts(program: &str, effort_flag: &str) {
    // Machines without the harness skip the check.
    let Ok(output) = Command::new(program).arg("--help").output() else {
        return;
    };

    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--print", "--model", effort_flag] {
        assert!(help.contains(flag), "{program} --help does not list {flag}");
    }
}
