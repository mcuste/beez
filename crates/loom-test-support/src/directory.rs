use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

/// A unique directory that is removed when the test drops it.
#[derive(Debug)]
pub struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    /// Creates an empty directory named after the test.
    pub fn new(name: &str) -> io::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!("loom-{name}-{}-{timestamp}", process::id()));

        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    /// The directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The path of `name` inside the directory.
    #[must_use]
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
