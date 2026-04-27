use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

fn main() {
    if let Err(error) = run(std::env::args_os(), &mut io::stdout()) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run(args: impl IntoIterator<Item = OsString>, output: &mut impl Write) -> Result<(), CliError> {
    let mut args = args.into_iter();
    let _program = args.next();
    let arguments: Vec<_> = args.collect();

    match arguments.as_slice() {
        [] => {
            writeln!(output, "Usage: loom --version")?;
            Ok(())
        }
        [argument] if argument == "--help" || argument == "-h" => {
            writeln!(output, "Usage: loom --version")?;
            Ok(())
        }
        [argument] if argument == "--version" || argument == "-V" => {
            writeln!(output, "loom {}", env!("CARGO_PKG_VERSION"))?;
            Ok(())
        }
        [argument] => Err(CliError::UnknownArgument(argument.clone())),
        _ => Err(CliError::UnexpectedArguments),
    }
}

#[derive(Debug)]
enum CliError {
    Io(io::Error),
    UnknownArgument(OsString),
    UnexpectedArguments,
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::UnknownArgument(argument) => {
                write!(
                    formatter,
                    "unknown argument {:?}",
                    argument.to_string_lossy()
                )
            }
            Self::UnexpectedArguments => formatter.write_str("expected at most one argument"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::UnknownArgument(_) | Self::UnexpectedArguments => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::run;

    #[test]
    fn prints_the_release_version() {
        let mut output = Vec::new();

        run(
            [OsString::from("loom"), OsString::from("--version")],
            &mut output,
        )
        .unwrap();

        assert_eq!(output, b"loom 0.1.0\n");
    }
}
