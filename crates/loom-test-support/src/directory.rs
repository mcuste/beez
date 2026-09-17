use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A unique directory that is removed when the test drops it.
#[derive(Debug)]
pub struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    /// Creates an empty directory named after the test.
    pub fn new(name: &str) -> io::Result<Self> {
        // The counter separates parallel tests; the timestamp separates runs that reuse a PID.
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "loom-{name}-{}-{timestamp}-{sequence}",
            process::id()
        ));

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

    /// Writes `contents` to `name` inside the directory and returns its path.
    pub fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> io::Result<PathBuf> {
        let path = self.0.join(name);
        fs::write(&path, contents)?;
        Ok(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
