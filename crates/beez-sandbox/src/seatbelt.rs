//! macOS backend: a Seatbelt profile applied through `sandbox-exec`.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::proxy::ProxyPorts;
use crate::resolve::{Execution, PathSet, ResolvedSandbox};
use crate::sandbox::Wrapped;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// System services a sandboxed program may call.
///
/// A Mach service is a system process that does work for its caller, outside
/// the sandbox. Allowing every service lets a task have one act for it: Launch
/// Services opens a URL in the browser, and the pasteboard server hands over
/// the clipboard. These are the services command-line tools need: user and
/// group lookups, notifications, logging, preferences, the temporary
/// directory, certificate checks, file events, network settings, and sleep
/// assertions.
const MACH_SERVICES: [&str; 20] = [
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
    "com.apple.system.DirectoryService.libinfo_v1",
    "com.apple.system.DirectoryService.membership_v1",
    "com.apple.system.notification_center",
    "com.apple.system.logger",
    "com.apple.logd",
    "com.apple.logd.events",
    "com.apple.diagnosticd",
    "com.apple.cfprefsd.daemon",
    "com.apple.cfprefsd.agent",
    "com.apple.bsd.dirhelper",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.ocspd",
    "com.apple.SecurityServer",
    "com.apple.FSEvents",
    "com.apple.SystemConfiguration.configd",
    "com.apple.SystemConfiguration.DNSConfiguration",
    "com.apple.PowerManagement.control",
];

/// Seatbelt keeps nothing alive beside the command.
pub(crate) type Guard = ();

/// Wraps the program in `sandbox-exec` with a generated profile.
///
/// The sandbox shares the host's network, so the program reaches the proxies
/// on their own ports.
pub(crate) fn wrap(
    resolved: &ResolvedSandbox,
    ports: ProxyPorts,
    arguments: &[OsString],
) -> io::Result<Wrapped> {
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(profile(resolved, ports)?)
        .arg(resolved.program())
        .args(arguments);
    Ok(Wrapped {
        command,
        ports,
        guard: (),
    })
}

/// The Seatbelt profile for a resolved policy. Later rules win over earlier ones.
///
/// Fails for a path that is not UTF-8, which a profile cannot name.
pub(crate) fn profile(resolved: &ResolvedSandbox, ports: ProxyPorts) -> io::Result<String> {
    let mut profile = String::from(concat!(
        "(version 1)\n",
        "(deny default)\n",
        "(allow process-fork)\n",
        "(allow process-info* (target same-sandbox))\n",
        "(allow signal (target same-sandbox))\n",
        "(allow sysctl-read)\n",
        "(allow ipc-posix*)\n",
        "(allow pseudo-tty)\n",
        "(allow user-preference-read)\n",
        "(allow file-ioctl)\n",
        "(allow file-read*)\n",
    ));
    profile.push_str("(allow mach-lookup");
    for service in MACH_SERVICES {
        let _ = write!(profile, " (global-name \"{service}\")");
    }
    profile.push_str(")\n");

    write_rule(
        &mut profile,
        "deny file-read*",
        &resolved.read_deny,
        Filter::Subpath,
    )?;
    // Path lookups through a denied directory still need its metadata.
    profile.push_str("(allow file-read-metadata)\n");

    write_rule(
        &mut profile,
        "allow file-write*",
        &resolved.write_allow,
        Filter::Subpath,
    )?;
    profile.push_str(concat!(
        "(allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\")",
        " (literal \"/dev/ptmx\") (literal \"/dev/dtracehelper\")",
        " (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/fd/[0-9]+$\"))\n",
    ));
    write_rule(
        &mut profile,
        "deny file-write*",
        &resolved.write_deny,
        Filter::Subpath,
    )?;
    // A denied directory must not be moved away and recreated writable.
    let ancestors = resolved.protected_ancestors();
    write_rule(
        &mut profile,
        "deny file-write-unlink",
        &ancestors,
        Filter::Literal,
    )?;

    profile.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
    profile.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
    let _ = writeln!(
        profile,
        "(allow network-outbound (remote ip \"localhost:{}\") (remote ip \"localhost:{}\"))",
        ports.http, ports.socks
    );
    if resolved.localhost {
        profile.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    }

    match &resolved.execution {
        Execution::Unrestricted => profile.push_str("(allow process-exec*)\n"),
        Execution::Only(executables) => {
            profile.push_str("(allow process-exec*");
            for path in executables {
                let filter = if path.is_dir() {
                    Filter::Subpath
                } else {
                    Filter::Literal
                };
                write_filter(&mut profile, filter, path)?;
            }
            profile.push_str(")\n");
        }
    }
    Ok(profile)
}

/// How a rule matches a path.
#[derive(Clone, Copy, Debug)]
enum Filter {
    /// The path and everything below it.
    Subpath,
    /// The path itself only.
    Literal,
}

impl Filter {
    fn name(self) -> &'static str {
        match self {
            Self::Subpath => "subpath",
            Self::Literal => "literal",
        }
    }
}

fn write_rule(
    profile: &mut String,
    action: &str,
    paths: &PathSet,
    filter: Filter,
) -> io::Result<()> {
    if paths.iter().next().is_none() {
        return Ok(());
    }
    let _ = write!(profile, "({action}");
    for path in paths {
        write_filter(profile, filter, path)?;
    }
    profile.push_str(")\n");
    Ok(())
}

/// Appends one ` (filter "path")` clause.
fn write_filter(profile: &mut String, filter: Filter, path: &Path) -> io::Result<()> {
    let _ = write!(profile, " ({} {})", filter.name(), quote(path)?);
    Ok(())
}

/// Quotes a path as an SBPL string.
///
/// A path that is not UTF-8 is an error: a lossy copy would name another
/// path, and a deny rule for it would protect nothing.
fn quote(path: &Path) -> io::Result<String> {
    let text = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox cannot name the non-UTF-8 path {} in a Seatbelt profile",
                path.display()
            ),
        )
    })?;
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    Ok(quoted)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use super::{profile, quote};
    use crate::proxy::ProxyPorts;
    use crate::resolve::{ExecAllowlist, Execution, PathSet, ResolvedSandbox};

    const PORTS: ProxyPorts = ProxyPorts {
        http: 3128,
        socks: 1080,
    };

    fn resolved() -> ResolvedSandbox {
        ResolvedSandbox {
            allowed_domains: Vec::new(),
            localhost: false,
            read_deny: PathSet::from([PathBuf::from("/Users/me/.ssh")]),
            write_allow: PathSet::from([
                PathBuf::from("/Users/me/work"),
                PathBuf::from("/private/tmp"),
            ]),
            write_deny: PathSet::from([PathBuf::from("/Users/me/work/.git/hooks")]),
            execution: Execution::Unrestricted,
            environment: Vec::new(),
            working_directory: PathBuf::from("/Users/me/work"),
            program: PathBuf::from("/bin/bash"),
        }
    }

    #[test]
    fn writes_filesystem_rules_in_precedence_order() {
        let sut = profile(&resolved(), PORTS).unwrap();

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
        let sut = profile(&resolved(), PORTS).unwrap();

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
        resolved.execution = Execution::Only(ExecAllowlist::new(
            PathBuf::from("/bin/bash"),
            [PathBuf::from("/usr")],
        ));

        let sut = profile(&resolved, PORTS).unwrap();

        assert!(sut.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
        assert!(sut.contains("(allow process-exec* (literal \"/bin/bash\") (subpath \"/usr\"))"));
    }

    #[test]
    fn allows_only_the_listed_system_services() {
        let sut = profile(&resolved(), PORTS).unwrap();

        assert!(!sut.contains("(allow mach-lookup)"), "{sut}");
        assert!(sut.contains(
            "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\")"
        ));
        assert!(!sut.contains("launchservicesd"), "{sut}");
        assert!(!sut.contains("pasteboard"), "{sut}");
    }

    #[test]
    fn escapes_quotes_and_backslashes_in_paths() {
        assert_eq!(
            quote(std::path::Path::new("/a \"b\"/c\\d")).unwrap(),
            "\"/a \\\"b\\\"/c\\\\d\""
        );
    }

    #[test]
    fn refuses_a_path_that_is_not_utf8_instead_of_naming_another_one() {
        let mut resolved = resolved();
        resolved.write_deny = PathSet::from([PathBuf::from(OsString::from_vec(
            b"/Users/me/work/\xff".to_vec(),
        ))]);

        let error = profile(&resolved, PORTS).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
