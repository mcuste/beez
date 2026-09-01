//! Checks that installed harnesses accept Loom's arguments.
#![cfg(unix)]

use std::process::{Command, Output, Stdio};

const UNKNOWN_PROVIDER: &str = "loom_contract_check";

#[test]
fn pi_accepts_the_flags_loom_sends() {
    assert_help_documents("pi", &[], &["--print", "--model", "--effort"]);
}

#[test]
fn omp_accepts_the_flags_loom_sends() {
    assert_help_documents("omp", &[], &["--print", "--model", "--thinking"]);
}

#[test]
fn claude_accepts_the_flags_loom_sends() {
    assert_help_documents("claude", &[], &["--print", "--model", "--effort"]);
}

#[test]
fn codex_accepts_the_flags_loom_sends() {
    assert_help_documents("codex", &["exec"], &["--model", "-c, --config"]);
}

#[test]
fn codex_accepts_the_effort_override_loom_sends() {
    let report = report(run_isolated_codex("model_reasoning_effort=high"));

    assert!(report.contains(&format!("Model provider `{UNKNOWN_PROVIDER}` not found")));
}

#[test]
fn codex_rejects_an_unknown_config_override() {
    let report = report(run_isolated_codex("loom_contract_unknown_key=1"));

    assert!(report.contains("unknown configuration field `loom_contract_unknown_key`"));
}

fn assert_help_documents(program: &str, help_command: &[&str], flags: &[&str]) {
    let mut arguments = help_command.to_vec();
    arguments.push("--help");
    let output = run(program, &arguments);

    assert!(
        output.status.success(),
        "{program} {} --help failed with {}",
        help_command.join(" "),
        output.status
    );
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in flags {
        assert!(help.contains(flag), "{program} --help does not list {flag}");
    }
}

fn run_isolated_codex(config_override: &str) -> Output {
    run(
        "codex",
        &[
            "exec",
            "--skip-git-repo-check",
            "--strict-config",
            "--ignore-user-config",
            "-c",
            "model_provider=loom_contract_check",
            "-c",
            config_override,
            "report the loaded configuration",
        ],
    )
}

fn run(program: &str, arguments: &[&str]) -> Output {
    Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run {program}: {error}"))
}

fn report(output: Output) -> String {
    let mut report = String::from_utf8_lossy(&output.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&output.stderr));
    report
}
