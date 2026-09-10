//! Runs real programs inside the platform sandbox and checks the boundary.
//!
//! The Linux sandbox re-executes the `loom` binary as the first process inside
//! its namespace, so only tests that drive the real binary can reach it. These
//! tests also need the platform sandbox to work on the host: `sandbox-exec` on
//! macOS, bubblewrap plus Landlock on Linux, and `bash` and `curl` on both.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

use loom_test_support::TemporaryDirectory;

/// A temporary directory with an empty working directory inside it.
fn working_directory(name: &str) -> io::Result<(TemporaryDirectory, PathBuf)> {
    let directory = TemporaryDirectory::new(name)?;
    let work = directory.join("work");
    fs::create_dir(&work)?;
    Ok((directory, work))
}

/// Runs `manifest` with `work` as the working directory of its tasks.
fn run_workflow(directory: &TemporaryDirectory, work: &Path, manifest: &str) -> io::Result<Output> {
    let path = directory.join("workflow.yaml");
    fs::write(&path, manifest)?;

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(&path)
        .current_dir(work)
        .env("LOOM_LOG_DIR", directory.join("logs"))
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.starts_with("bwrap:") || stderr.contains("sandbox-exec:") {
        return Err(io::Error::other(format!(
            "the sandbox itself did not start: {stderr}"
        )));
    }
    Ok(output)
}

/// A one-task workflow that runs `script` under `sandbox`.
fn manifest(sandbox: &str, script: &str) -> String {
    format!("sandbox:\n{sandbox}tasks:\n  - id: task\n    command: [bash, -c, '{script}']\n")
}

/// A policy that may only write the working directory.
const WORK_ONLY: &str = "  filesystem:\n    defaults: false\n    write_allow: [\".\"]\n";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A path outside every writable directory of the test policies.
fn outside_path(name: &str) -> PathBuf {
    let path =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_file(&path);
    path
}

/// Runs `loom run command` with `arguments` in front of the program.
fn run_command(
    directory: &TemporaryDirectory,
    work: &Path,
    arguments: &[&str],
) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "command"])
        .args(arguments)
        .current_dir(work)
        .env("LOOM_LOG_DIR", directory.join("logs"))
        .output()
}

#[test]
fn sandboxes_a_command_unless_it_asks_for_no_sandbox() {
    let (directory, work) = working_directory("cli-sandbox-command").unwrap();
    let outside = outside_path("loom-sandbox-command.txt");
    let script = format!("printf leak > {}", outside.display());

    let denied = run_command(&directory, &work, &["bash", "-c", &script]).unwrap();
    let allowed = run_command(&directory, &work, &["--no-sandbox", "bash", "-c", &script]).unwrap();

    assert!(!denied.status.success(), "{denied:?}");
    assert!(allowed.status.success(), "{allowed:?}");
    assert!(outside.exists());
    let _ = fs::remove_file(&outside);
}

#[test]
fn refuses_no_sandbox_together_with_an_allowance() {
    let (directory, work) = working_directory("cli-sandbox-conflict").unwrap();

    let output = run_command(
        &directory,
        &work,
        &["--no-sandbox", "--allow-domain", "github.com", "true"],
    )
    .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot be used with"),
        "{output:?}"
    );
}

#[test]
fn sandboxes_a_workflow_that_names_no_sandbox() {
    let (directory, work) = working_directory("cli-sandbox-default").unwrap();
    let outside = outside_path("loom-sandbox-default-leak.txt");
    let source = format!(
        "tasks:\n  - id: task\n    command: [bash, -c, 'printf leak > {}']\n",
        outside.display()
    );

    let denied = run_workflow(&directory, &work, &source).unwrap();

    assert!(!denied.status.success(), "{denied:?}");
    assert!(!outside.exists());
}

#[test]
fn runs_a_task_that_opts_out_without_the_sandbox() {
    let (directory, work) = working_directory("cli-sandbox-opt-out").unwrap();
    let outside = outside_path("loom-sandbox-opt-out.txt");
    let source = format!(
        "tasks:\n  - id: task\n    sandbox: false\n    command: [bash, -c, 'printf ok > {}']\n",
        outside.display()
    );

    let allowed = run_workflow(&directory, &work, &source).unwrap();

    assert!(allowed.status.success(), "{allowed:?}");
    assert!(outside.exists());
    let _ = fs::remove_file(&outside);
}

#[test]
fn allows_writes_inside_the_working_directory_only() {
    let (directory, work) = working_directory("cli-sandbox-writes").unwrap();
    let outside = outside_path("loom-sandbox-leak.txt");

    let inside = run_workflow(
        &directory,
        &work,
        &manifest(WORK_ONLY, "printf ok > inside.txt && cat inside.txt"),
    )
    .unwrap();
    let denied = run_workflow(
        &directory,
        &work,
        &manifest(WORK_ONLY, &format!("printf leak > {}", outside.display())),
    )
    .unwrap();

    assert!(inside.status.success(), "{inside:?}");
    assert_eq!(inside.stdout, b"ok");
    assert!(!denied.status.success(), "{denied:?}");
    assert!(!outside.exists());
}

#[test]
fn keeps_a_denied_file_unreadable() {
    let (directory, work) = working_directory("cli-sandbox-denied-file").unwrap();
    fs::write(work.join("secret"), "s3cret").unwrap();
    fs::write(work.join("visible"), "public").unwrap();
    let sandbox = "  filesystem:\n    defaults: false\n    read_deny: [\"secret\"]\n    write_allow: [\".\"]\n";

    let output = run_workflow(
        &directory,
        &work,
        &manifest(sandbox, "cat secret; cat visible"),
    )
    .unwrap();

    assert!(contains(&output.stdout, b"public"), "{output:?}");
    assert!(!contains(&output.stdout, b"s3cret"), "{output:?}");
}

#[test]
fn keeps_a_denied_directory_unreadable() {
    let (directory, work) = working_directory("cli-sandbox-denied-directory").unwrap();
    fs::create_dir(work.join("keys")).unwrap();
    fs::write(work.join("keys/id"), "private-key").unwrap();
    fs::write(work.join("visible"), "public").unwrap();
    let sandbox =
        "  filesystem:\n    defaults: false\n    read_deny: [\"keys\"]\n    write_allow: [\".\"]\n";

    let output = run_workflow(
        &directory,
        &work,
        &manifest(sandbox, "cat keys/id; cat visible"),
    )
    .unwrap();

    assert!(contains(&output.stdout, b"public"), "{output:?}");
    assert!(!contains(&output.stdout, b"private-key"), "{output:?}");
}

/// A policy that may write the working directory but never `.git/hooks`.
const PROTECTED_HOOKS: &str = "  filesystem:\n    defaults: false\n    write_allow: [\".\"]\n    write_deny: [\".git/hooks\"]\n";

#[test]
fn refuses_writes_to_a_denied_directory() {
    let (directory, work) = working_directory("cli-sandbox-denied-write").unwrap();
    fs::create_dir_all(work.join(".git/hooks")).unwrap();

    let output = run_workflow(
        &directory,
        &work,
        &manifest(PROTECTED_HOOKS, "printf x > .git/hooks/pre-commit"),
    )
    .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(!work.join(".git/hooks/pre-commit").exists());
}

#[test]
fn refuses_to_move_a_denied_directory_out_of_the_way() {
    let (directory, work) = working_directory("cli-sandbox-move-denied").unwrap();
    fs::create_dir_all(work.join(".git/hooks")).unwrap();

    let output = run_workflow(
        &directory,
        &work,
        &manifest(PROTECTED_HOOKS, "mv .git away"),
    )
    .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(work.join(".git").is_dir());
}

#[test]
fn refuses_to_create_a_denied_directory_that_does_not_exist_yet() {
    let (directory, work) = working_directory("cli-sandbox-missing-deny-directory").unwrap();

    let output = run_workflow(
        &directory,
        &work,
        &manifest(
            PROTECTED_HOOKS,
            "mkdir -p .git/hooks; printf x > .git/hooks/pre-commit",
        ),
    )
    .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(!work.join(".git/hooks/pre-commit").exists());
}

#[test]
fn refuses_to_create_a_denied_file_that_does_not_exist_yet() {
    let (directory, work) = working_directory("cli-sandbox-missing-deny-file").unwrap();
    let sandbox = "  filesystem:\n    defaults: false\n    write_allow: [\".\"]\n    write_deny: [\".mcp.json\"]\n";

    let output = run_workflow(
        &directory,
        &work,
        &manifest(sandbox, "printf x > .mcp.json"),
    )
    .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(!fs::read_to_string(work.join(".mcp.json")).is_ok_and(|text| text == "x"));
}

/// A policy that may write the working directory and run nothing but builtins.
const NO_EXECUTABLES: &str = "  filesystem:\n    defaults: false\n    write_allow: [\".\"]\n  executables:\n    defaults: false\n";

#[test]
fn refuses_a_program_outside_the_enabled_executable_groups() {
    let (directory, work) = working_directory("cli-sandbox-denied-program").unwrap();

    let builtin =
        run_workflow(&directory, &work, &manifest(NO_EXECUTABLES, "echo builtin")).unwrap();
    let denied = run_workflow(
        &directory,
        &work,
        &manifest(NO_EXECUTABLES, "command -v uname >/dev/null && uname"),
    )
    .unwrap();

    assert!(builtin.status.success(), "{builtin:?}");
    assert!(!denied.status.success(), "{denied:?}");
}

#[test]
fn runs_a_program_from_an_enabled_executable_group() {
    let (directory, work) = working_directory("cli-sandbox-allowed-program").unwrap();
    let sandbox = "  filesystem:\n    defaults: false\n    write_allow: [\".\"]\n  executables:\n    defaults: false\n    groups: [coreutils]\n";

    let output = run_workflow(&directory, &work, &manifest(sandbox, "uname")).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(!output.stdout.is_empty());
}

/// Accepts one connection and answers a fixed HTTP response.
fn http_origin() -> io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\norigin",
            );
        }
    });
    Ok(port)
}

/// A listener that never accepts, to check nothing reaches it.
fn unused_origin() -> io::Result<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    Ok((port, listener))
}

fn assert_never_reached(origin: &TcpListener) {
    let refused = origin.accept().err();
    assert!(
        refused.is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "the sandbox reached the origin"
    );
}

/// A policy that allows `localhost` and, with `localhost`, local addresses.
fn network(localhost: bool) -> String {
    format!(
        "  network:\n    defaults: false\n    allow: [\"localhost\"]\n    localhost: {localhost}\n{WORK_ONLY}"
    )
}

fn fetch(proxy: &str, port: u16) -> String {
    format!("curl -sf --max-time 10 --proxy \"${proxy}\" --noproxy \"\" http://localhost:{port}/")
}

#[test]
fn reaches_an_allowed_host_through_the_http_proxy() {
    let (directory, work) = working_directory("cli-sandbox-http-proxy").unwrap();
    let origin_port = http_origin().unwrap();

    let output = run_workflow(
        &directory,
        &work,
        &manifest(&network(true), &fetch("HTTP_PROXY", origin_port)),
    )
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"origin");
}

#[test]
fn reaches_an_allowed_host_through_the_socks_proxy() {
    let (directory, work) = working_directory("cli-sandbox-socks-proxy").unwrap();
    let origin_port = http_origin().unwrap();

    let output = run_workflow(
        &directory,
        &work,
        &manifest(&network(true), &fetch("ALL_PROXY", origin_port)),
    )
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"origin");
}

#[test]
fn refuses_local_connections_directly_and_through_the_proxy() {
    let (directory, work) = working_directory("cli-sandbox-local-network").unwrap();
    let (origin_port, origin) = unused_origin().unwrap();
    let sandbox = network(false);

    let direct = run_workflow(
        &directory,
        &work,
        &manifest(
            &sandbox,
            &format!("curl -sf --max-time 5 --noproxy \"*\" http://localhost:{origin_port}/"),
        ),
    )
    .unwrap();
    let proxied = run_workflow(
        &directory,
        &work,
        &manifest(&sandbox, &fetch("HTTP_PROXY", origin_port)),
    )
    .unwrap();

    assert!(!direct.status.success(), "{direct:?}");
    assert!(!proxied.status.success(), "{proxied:?}");
    assert_never_reached(&origin);
    // Loom's own notes carry a status word, so a log never confuses them with
    // the output of a task.
    let stderr = String::from_utf8_lossy(&proxied.stderr);
    assert!(
        stderr.contains("Sandbox   denied connection to"),
        "{stderr}"
    );
    assert!(!contains(&proxied.stdout, b"Sandbox"), "{proxied:?}");
}

#[test]
fn applies_the_harness_environment_inside_the_sandbox() {
    let directory = TemporaryDirectory::new("cli-sandbox-harness-environment").unwrap();
    let work = directory.join("work");
    fs::create_dir(&work).unwrap();
    write_harness_stub(&directory).unwrap();
    let manifest = directory.join("workflow.yaml");
    fs::write(
        &manifest,
        "tasks:\n  - id: environment\n    harness: claude\n    prompt: check\n    sandbox: true\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(&manifest)
        .current_dir(&work)
        .env("PATH", extended_path(directory.path()))
        .env("LOOM_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert_eq!(output.stdout, b"1-false", "{output:?}");
}

/// `directory` before the host `PATH`, which the sandbox needs for its own tools.
fn extended_path(directory: &Path) -> String {
    let host = std::env::var("PATH").unwrap_or_default();
    format!("{}:{host}", directory.display())
}

/// Writes a `claude` stub that prints the harness environment it received.
fn write_harness_stub(directory: &TemporaryDirectory) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let program = directory.join("claude");
    fs::write(
        &program,
        "#!/bin/sh\nprintf %s \"$CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC-$ENABLE_CLAUDEAI_MCP_SERVERS\"\n",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
}
