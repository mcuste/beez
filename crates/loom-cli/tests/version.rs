//! Tests the compiled Loom CLI.

use std::process::Command;

#[test]
fn reports_release_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--version")
        .output()
        .expect("Loom binary starts");

    assert!(output.status.success());
    let reported = std::str::from_utf8(&output.stdout).expect("version output is UTF-8");
    let version = reported
        .strip_prefix("loom ")
        .expect("version output names the program")
        .trim_end();
    let parts = version.split('.').collect::<Vec<_>>();
    assert_eq!(parts.len(), 3, "{version} is not a three-part version");
    assert!(
        parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(char::is_numeric)),
        "{version} has a non-numeric part"
    );
}
