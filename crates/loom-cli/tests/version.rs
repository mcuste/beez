//! Tests the compiled Loom CLI.

use std::process::Command;

#[test]
fn reports_release_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--version")
        .output()
        .expect("Loom binary starts");

    assert!(output.status.success());
    assert_eq!(
        std::str::from_utf8(&output.stdout).expect("version output is UTF-8"),
        "loom 0.1.0\n"
    );
}
