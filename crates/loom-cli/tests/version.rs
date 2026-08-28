//! Tests the compiled Loom CLI.

use std::process::Command;

#[test]
fn reports_release_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--version")
        .output()
        .expect("Loom binary starts");

    assert!(output.status.success());
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        std::str::from_utf8(&output.stdout).expect("version output is UTF-8"),
        format!("loom {version}\n")
    );
}
