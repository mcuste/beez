//! Where the daemon keeps its socket, its record and its state.

use std::io;
use std::path::{Path, PathBuf};

/// Directory of the daemon's own files, inside the Loom root.
const DIRECTORY: &str = "daemon";
const SOCKET: &str = "daemon.sock";
const RECORD: &str = "daemon.json";
const LOG: &str = "daemon.log";
const MANIFESTS: &str = "manifests.json";
const STATE: &str = "state.json";

/// A Unix socket path holds 104 bytes at most on macOS, with room for the NUL.
const SOCKET_LIMIT: usize = 100;

/// The files of one daemon.
///
/// A daemon belongs to one Loom root. The root holds the daemon's own files
/// and the runs it starts, and one root is enough for every repository,
/// because each watched manifest carries the directory its tasks run in.
#[derive(Clone, Debug)]
pub struct DaemonPaths {
    root: PathBuf,
    socket: PathBuf,
}

impl DaemonPaths {
    /// The one daemon of the person running Loom, in `~/.loom`.
    ///
    /// The daemon does not look at the working directory, so a command finds
    /// the same daemon from anywhere, and a service manager starting it in a
    /// directory of its own finds the same jobs.
    pub fn user() -> io::Result<Self> {
        let home = std::env::home_dir()
            .ok_or_else(|| io::Error::other("the daemon needs the home directory"))?;

        Ok(Self::new(home.join(".loom")))
    }

    /// Uses `root` as the Loom root, in place of the one Loom would find.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let socket = socket_path(&root.join(DIRECTORY));

        Self { root, socket }
    }

    /// The Loom root, which also holds the runs.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of the daemon's own files.
    #[must_use]
    pub fn directory(&self) -> PathBuf {
        self.root.join(DIRECTORY)
    }

    /// The socket that carries the commands.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The file that names the running daemon.
    #[must_use]
    pub fn record(&self) -> PathBuf {
        self.directory().join(RECORD)
    }

    /// The file a detached daemon writes its own lines to.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.directory().join(LOG)
    }

    /// The file that lists the manifests the daemon watches.
    #[must_use]
    pub fn manifests(&self) -> PathBuf {
        self.directory().join(MANIFESTS)
    }

    /// The file that holds what happened to each job.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.directory().join(STATE)
    }

    /// Creates the daemon directory, and keeps the Loom root out of Git.
    pub fn create(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.directory())?;

        loom_record::ignore_everything(&self.root)
    }
}

/// Keeps the socket beside the other daemon files, unless the path is too long
/// for a Unix socket. A deep repository falls back to the temporary directory.
fn socket_path(directory: &Path) -> PathBuf {
    let inside = directory.join(SOCKET);
    if inside.as_os_str().len() <= SOCKET_LIMIT {
        return inside;
    }

    std::env::temp_dir().join(format!("loom-{:016x}.sock", fingerprint(directory)))
}

/// FNV-1a, which names one directory without a hash dependency.
fn fingerprint(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    hash
}

#[cfg(test)]
mod tests {
    use super::{DaemonPaths, SOCKET_LIMIT};
    use std::path::Path;

    #[test]
    fn keeps_the_daemon_of_a_person_in_their_home_directory() {
        let paths = DaemonPaths::user().unwrap();
        let home = std::env::home_dir().unwrap();

        assert_eq!(paths.root(), home.join(".loom"));
    }

    #[test]
    fn keeps_every_file_inside_the_loom_root() {
        let paths = DaemonPaths::new("/work/.loom");

        assert_eq!(paths.directory(), Path::new("/work/.loom/daemon"));
        assert_eq!(paths.socket(), Path::new("/work/.loom/daemon/daemon.sock"));
        assert_eq!(paths.record(), Path::new("/work/.loom/daemon/daemon.json"));
        assert_eq!(
            paths.manifests(),
            Path::new("/work/.loom/daemon/manifests.json")
        );
    }

    /// A Unix socket path is short, so a deep root keeps its socket elsewhere.
    #[test]
    fn moves_a_socket_that_does_not_fit_out_of_a_deep_root() {
        let deep = format!("/{}/.loom", "directory/".repeat(20));
        let paths = DaemonPaths::new(&deep);

        assert!(paths.socket().as_os_str().len() <= SOCKET_LIMIT);
        assert!(!paths.socket().starts_with(&deep));
        assert_eq!(paths.socket(), DaemonPaths::new(&deep).socket());
    }
}
