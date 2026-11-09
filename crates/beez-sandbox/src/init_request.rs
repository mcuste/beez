//! The arguments of the `sandbox-init` command, which Beez writes for its own
//! Linux sandbox and reads back inside it.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::relay::Relay;

/// Option that relays one loopback port, followed by a [`Relay`] argument.
pub const RELAY_FLAG: &str = "relay";
/// Option that allows one file or directory to execute, followed by its path.
pub const EXEC_FLAG: &str = "exec";
/// Option that lets every program execute, in place of [`EXEC_FLAG`].
pub const UNRESTRICTED_EXEC_FLAG: &str = "unrestricted-exec";

/// Which programs may execute inside the sandbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecRestriction {
    /// Any program.
    Unrestricted,
    /// Only these files and the files inside these directories.
    Only(Vec<PathBuf>),
}

/// What `sandbox-init` sets up before it becomes the sandboxed program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitRequest {
    /// Loopback ports to bridge to the proxies outside the sandbox.
    pub relays: Vec<Relay>,
    /// Which programs may execute.
    pub execution: ExecRestriction,
    /// The program to replace `sandbox-init` with.
    pub program: PathBuf,
    /// The arguments of the program.
    pub arguments: Vec<OsString>,
}

impl InitRequest {
    /// The `sandbox-init` arguments that ask for this request, ending with the
    /// program after a `--` separator.
    #[must_use]
    pub fn to_arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        for relay in &self.relays {
            arguments.push(flag(RELAY_FLAG));
            arguments.push(OsString::from(relay));
        }
        match &self.execution {
            ExecRestriction::Unrestricted => arguments.push(flag(UNRESTRICTED_EXEC_FLAG)),
            ExecRestriction::Only(paths) => {
                for path in paths {
                    arguments.push(flag(EXEC_FLAG));
                    arguments.push(path.into());
                }
            }
        }
        arguments.push(OsString::from("--"));
        arguments.push(self.program.clone().into());
        arguments.extend(self.arguments.iter().cloned());
        arguments
    }
}

fn flag(name: &str) -> OsString {
    OsString::from(format!("--{name}"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::num::NonZeroU16;
    use std::path::PathBuf;

    use super::{ExecRestriction, InitRequest};
    use crate::relay::Relay;

    fn request(execution: ExecRestriction) -> InitRequest {
        InitRequest {
            relays: vec![Relay::new(
                NonZeroU16::new(3128).unwrap(),
                PathBuf::from("/tmp/http.sock"),
            )],
            execution,
            program: PathBuf::from("/bin/bash"),
            arguments: vec![OsString::from("-c"), OsString::from("exit 0")],
        }
    }

    #[test]
    fn writes_relays_then_the_execute_rules_then_the_program() {
        let sut = request(ExecRestriction::Only(vec![
            PathBuf::from("/bin/bash"),
            PathBuf::from("/usr/bin"),
        ]));

        assert_eq!(
            sut.to_arguments(),
            [
                "--relay",
                "3128=/tmp/http.sock",
                "--exec",
                "/bin/bash",
                "--exec",
                "/usr/bin",
                "--",
                "/bin/bash",
                "-c",
                "exit 0",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn writes_the_unrestricted_option_in_place_of_execute_rules() {
        let sut = request(ExecRestriction::Unrestricted);

        assert_eq!(
            sut.to_arguments(),
            [
                "--relay",
                "3128=/tmp/http.sock",
                "--unrestricted-exec",
                "--",
                "/bin/bash",
                "-c",
                "exit 0",
            ]
            .map(OsString::from)
        );
    }
}
