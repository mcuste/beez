//! macOS backend: a Seatbelt profile applied through `sandbox-exec`.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use crate::resolve::ResolvedSandbox;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Wraps the program in `sandbox-exec` with a generated profile.
pub(crate) fn command(
    resolved: &ResolvedSandbox,
    http_port: u16,
    socks_port: u16,
    arguments: &[OsString],
) -> Command {
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(profile(resolved, http_port, socks_port))
        .arg(resolved.program())
        .args(arguments);
    command
}

/// The Seatbelt profile for a resolved policy. Later rules win over earlier ones.
pub(crate) fn profile(resolved: &ResolvedSandbox, http_port: u16, socks_port: u16) -> String {
    let mut profile = String::from(concat!(
        "(version 1)\n",
        "(deny default)\n",
        "(allow process-fork)\n",
        "(allow process-info* (target same-sandbox))\n",
        "(allow signal (target same-sandbox))\n",
        "(allow sysctl-read)\n",
        "(allow mach-lookup)\n",
        "(allow ipc-posix*)\n",
        "(allow pseudo-tty)\n",
        "(allow user-preference-read)\n",
        "(allow file-ioctl)\n",
        "(allow file-read*)\n",
    ));

    write_rule(
        &mut profile,
        "deny file-read*",
        &resolved.read_deny,
        "subpath",
    );
    // Path lookups through a denied directory still need its metadata.
    profile.push_str("(allow file-read-metadata)\n");

    write_rule(
        &mut profile,
        "allow file-write*",
        &resolved.write_allow,
        "subpath",
    );
    profile.push_str(concat!(
        "(allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\")",
        " (literal \"/dev/ptmx\") (literal \"/dev/dtracehelper\")",
        " (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/fd/[0-9]+$\"))\n",
    ));
    write_rule(
        &mut profile,
        "deny file-write*",
        &resolved.write_deny,
        "subpath",
    );
    // A denied directory must not be moved away and recreated writable.
    let ancestors = protected_ancestors(resolved);
    write_rule(
        &mut profile,
        "deny file-write-unlink",
        &ancestors,
        "literal",
    );

    profile.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
    profile.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
    let _ = writeln!(
        profile,
        "(allow network-outbound (remote ip \"localhost:{http_port}\") (remote ip \"localhost:{socks_port}\"))"
    );
    if resolved.localhost {
        profile.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    }

    match &resolved.executables {
        None => profile.push_str("(allow process-exec*)\n"),
        Some(executables) => {
            profile.push_str("(allow process-exec*");
            for path in executables {
                let filter = if path.is_dir() { "subpath" } else { "literal" };
                let _ = write!(profile, " ({filter} {})", quote(path));
            }
            profile.push_str(")\n");
        }
    }
    profile
}

fn write_rule(profile: &mut String, action: &str, paths: &[impl AsRef<Path>], filter: &str) {
    if paths.is_empty() {
        return;
    }
    let _ = write!(profile, "({action}");
    for path in paths {
        let _ = write!(profile, " ({filter} {})", quote(path.as_ref()));
    }
    profile.push_str(")\n");
}

/// Parents of write-denied paths that sit inside a writable directory.
fn protected_ancestors(resolved: &ResolvedSandbox) -> Vec<std::path::PathBuf> {
    let mut ancestors = resolved
        .write_deny
        .iter()
        .flat_map(|denied| denied.ancestors().skip(1))
        .filter(|ancestor| {
            resolved
                .write_allow
                .iter()
                .any(|allowed| ancestor.starts_with(allowed) && *ancestor != allowed.as_path())
        })
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    ancestors.sort();
    ancestors.dedup();
    ancestors
}

/// Quotes a path as an SBPL string.
fn quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{profile, quote};
    use crate::resolve::ResolvedSandbox;

    fn resolved() -> ResolvedSandbox {
        ResolvedSandbox {
            allowed_domains: Vec::new(),
            localhost: false,
            read_deny: vec![PathBuf::from("/Users/me/.ssh")],
            write_allow: vec![
                PathBuf::from("/Users/me/work"),
                PathBuf::from("/private/tmp"),
            ],
            write_deny: vec![PathBuf::from("/Users/me/work/.git/hooks")],
            executables: None,
            environment: Vec::new(),
            working_directory: PathBuf::from("/Users/me/work"),
            program: PathBuf::from("/bin/bash"),
        }
    }

    #[test]
    fn writes_filesystem_rules_in_precedence_order() {
        let sut = profile(&resolved(), 3128, 1080);

        let read_deny = sut
            .find("(deny file-read* (subpath \"/Users/me/.ssh\"))")
            .unwrap();
        let metadata = sut.find("(allow file-read-metadata)").unwrap();
        let write_allow = sut
            .find("(allow file-write* (subpath \"/Users/me/work\") (subpath \"/private/tmp\"))")
            .unwrap();
        let write_deny = sut
            .find("(deny file-write* (subpath \"/Users/me/work/.git/hooks\"))")
            .unwrap();
        assert!(read_deny < metadata);
        assert!(write_allow < write_deny);
        assert!(sut.contains("(deny file-write-unlink (literal \"/Users/me/work/.git\"))"));
        assert!(!sut.contains("(literal \"/Users/me/work\"))"));
    }

    #[test]
    fn allows_only_the_proxy_ports_outbound() {
        let sut = profile(&resolved(), 3128, 1080);

        assert!(sut.contains(
            "(allow network-outbound (remote ip \"localhost:3128\") (remote ip \"localhost:1080\"))"
        ));
        assert!(!sut.contains("localhost:*\"))\n(allow process-exec"));
        assert!(sut.contains("(allow process-exec*)\n"));
    }

    #[test]
    fn opens_localhost_and_restricts_executables_on_request() {
        let mut resolved = resolved();
        resolved.localhost = true;
        resolved.executables = Some(vec![PathBuf::from("/bin/bash"), PathBuf::from("/usr")]);

        let sut = profile(&resolved, 3128, 1080);

        assert!(sut.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
        assert!(sut.contains("(allow process-exec* (literal \"/bin/bash\") (subpath \"/usr\"))"));
    }

    #[test]
    fn escapes_quotes_and_backslashes_in_paths() {
        assert_eq!(
            quote(std::path::Path::new("/a \"b\"/c\\d")),
            "\"/a \\\"b\\\"/c\\\\d\""
        );
    }
}
