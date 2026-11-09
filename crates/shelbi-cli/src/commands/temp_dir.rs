//! A temp dir that removes itself on drop, unless kept.
//!
//! The binary needs one of these (the issue editor's draft dir), so it carries
//! this small guard rather than the `tempfile` crate, which stays a
//! dev-dependency for the tests.

use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A fresh directory under the system temp dir, removed (with everything in
/// it) when dropped, unless [`TempDir::keep`] took it first.
pub(crate) struct TempDir {
    path: PathBuf,
    keep: bool,
}

impl TempDir {
    /// Create `<temp>/<prefix><pid>-<nanos>-<n>`, owner-only (0700, as
    /// `tempfile` does): on Linux the temp dir is the shared `/tmp`, and the
    /// draft inside must not be readable by other users. `mkdir` is atomic and
    /// fails on an existing path, so a name another process already holds is
    /// skipped rather than shared.
    pub(crate) fn create(prefix: &str) -> io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir();
        let pid = std::process::id();
        loop {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("{prefix}{pid}-{nanos}-{n}"));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self { path, keep: false }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Leave the directory on disk and hand back its path.
    pub(crate) fn keep(mut self) -> PathBuf {
        self.keep = true;
        std::mem::take(&mut self.path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TempDir;

    #[test]
    fn drop_removes_the_dir_and_its_contents() {
        let dir = TempDir::create("shelbi-tempdir-test-").unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("draft.md"), "x").unwrap();
        assert!(path.is_dir());
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn keep_leaves_the_dir_on_disk() {
        let dir = TempDir::create("shelbi-tempdir-test-").unwrap();
        std::fs::write(dir.path().join("draft.md"), "x").unwrap();
        let kept = dir.keep();
        assert!(kept.join("draft.md").is_file());
        std::fs::remove_dir_all(&kept).unwrap();
    }

    #[test]
    fn name_starts_with_the_prefix_under_the_temp_dir() {
        let dir = TempDir::create("shelbi-tempdir-prefix-").unwrap();
        let path = dir.path();
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("shelbi-tempdir-prefix-"), "{name}");
        assert_eq!(path.parent().unwrap(), std::env::temp_dir().as_path());
    }

    #[test]
    fn dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::create("shelbi-tempdir-test-").unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "mode {:o}", mode & 0o777);
    }

    #[test]
    fn each_create_gets_its_own_dir() {
        let a = TempDir::create("shelbi-tempdir-test-").unwrap();
        let b = TempDir::create("shelbi-tempdir-test-").unwrap();
        assert_ne!(a.path(), b.path());
    }
}
