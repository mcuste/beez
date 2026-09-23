use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Reports an invalid sandbox path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SandboxPathError {
    /// The path is empty.
    Empty,
    /// The path contains a NUL byte.
    Nul,
}

impl fmt::Display for SandboxPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("sandbox path must not be empty"),
            Self::Nul => formatter.write_str("sandbox path must not contain NUL"),
        }
    }
}

impl std::error::Error for SandboxPathError {}

/// A filesystem rule path before it is resolved on a host.
///
/// `~` or `~/...` refers to the home directory. A relative path refers to the
/// task's working directory. A bare program name in an executable rule refers
/// to a lookup on `PATH`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SandboxPath(String);

impl SandboxPath {
    /// The path text as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the path is a bare name without a directory separator.
    #[must_use]
    pub fn is_bare_name(&self) -> bool {
        !self.0.contains('/') && self.0 != "~"
    }

    /// The absolute path this rule names on a host.
    #[must_use]
    pub fn resolve(&self, home: &Path, working_directory: &Path) -> PathBuf {
        if self.0 == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = self.0.strip_prefix("~/") {
            return home.join(rest);
        }
        // An absolute path replaces the working directory when joined.
        working_directory.join(&self.0)
    }
}

impl FromStr for SandboxPath {
    type Err = SandboxPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(SandboxPathError::Empty);
        }
        if value.contains('\0') {
            return Err(SandboxPathError::Nul);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for SandboxPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{SandboxPath, SandboxPathError};

    fn resolve(text: &str) -> PathBuf {
        text.parse::<SandboxPath>()
            .unwrap()
            .resolve(Path::new("/home/user"), Path::new("/work"))
    }

    #[test]
    fn resolves_home_working_directory_and_absolute_paths() {
        assert_eq!(resolve("~"), Path::new("/home/user"));
        assert_eq!(resolve("~/.ssh"), Path::new("/home/user/.ssh"));
        assert_eq!(resolve("."), Path::new("/work/."));
        assert_eq!(resolve(".git/hooks"), Path::new("/work/.git/hooks"));
        assert_eq!(resolve("/tmp"), Path::new("/tmp"));
    }

    #[test]
    fn detects_bare_program_names() {
        assert!("git".parse::<SandboxPath>().unwrap().is_bare_name());
        assert!(!"~".parse::<SandboxPath>().unwrap().is_bare_name());
        assert!(!"./git".parse::<SandboxPath>().unwrap().is_bare_name());
    }

    #[test]
    fn rejects_empty_and_nul_paths() {
        assert_eq!("".parse::<SandboxPath>(), Err(SandboxPathError::Empty));
        assert_eq!("a\0b".parse::<SandboxPath>(), Err(SandboxPathError::Nul));
    }
}
