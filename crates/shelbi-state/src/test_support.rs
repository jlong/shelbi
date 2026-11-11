//! Helpers for tests in this crate and its consumers (`test-support`).

use std::path::{Path, PathBuf};

/// Write `body` to `path` as an executable (0755) stub a test is about to
/// run, without ever holding the stub open for writing in this process.
///
/// Every test that writes a script and then executes it (a `gh` stub, a
/// fake `ssh`, a git hook) goes through here. The naive
/// `fs::write` + `set_permissions` races `cargo test`'s parallel threads on
/// Linux: while this thread has the file open for writing, another test's
/// `Command::spawn` can fork, and the child inherits that write fd until its
/// own `exec`. Executing the stub in that window fails with ETXTBSY ("Text
/// file busy"). `O_CLOEXEC` doesn't help (the fd is live between fork and
/// exec), and neither does writing a temp file and renaming it into place:
/// the busy check is on the inode, and a rename keeps the inode.
///
/// So the bytes the stub is executed from are written by a child process
/// (`cp` from a scratch copy). This process never opens that inode for
/// writing, so no fork of ours can inherit a writer on it. The copy lands
/// under a temp name and is renamed over `path` after the chmod, which also
/// makes replacing a stub mid-test atomic (a run of the old stub keeps its
/// own inode).
///
/// Panics on any failure: it's test scaffolding.
pub fn write_executable(path: impl AsRef<Path>, body: impl AsRef<[u8]>) {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let path = path.as_ref();
    let dir = path
        .parent()
        .unwrap_or_else(|| panic!("stub path {} has no parent dir", path.display()));
    let name = path
        .file_name()
        .unwrap_or_else(|| panic!("stub path {} has no file name", path.display()))
        .to_string_lossy();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tag = format!("{}.{seq}", std::process::id());
    let scratch = dir.join(format!(".{name}.stub-src.{tag}"));
    let staged = dir.join(format!(".{name}.stub.{tag}"));

    // The scratch copy is held open for writing here, but it is never
    // executed, so a fork that inherits that fd is harmless.
    std::fs::write(&scratch, body)
        .unwrap_or_else(|e| panic!("write stub source {}: {e}", scratch.display()));
    let copied = std::process::Command::new(cp_binary())
        .arg(&scratch)
        .arg(&staged)
        .stdin(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_file(&scratch);
    match copied {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("cp {} {}: {status}", scratch.display(), staged.display()),
        Err(e) => panic!("spawn cp for stub {}: {e}", path.display()),
    }
    // `set_permissions` is chmod(2) on the path: no fd is opened.
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod stub {}: {e}", staged.display()));
    std::fs::rename(&staged, path)
        .unwrap_or_else(|e| panic!("rename stub {} -> {}: {e}", staged.display(), path.display()));
}

/// `cp` by absolute path: tests point `PATH` at stub-only directories, so a
/// bare `cp` lookup isn't reliable while they run.
fn cp_binary() -> PathBuf {
    ["/bin/cp", "/usr/bin/cp"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("cp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch dir, removed on drop (`tempfile` isn't a dependency
    /// of this crate).
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("shb-stub-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stub_is_executable_and_runs() {
        let dir = Scratch::new("rewrite");
        let stub = dir.path().join("gh");
        write_executable(&stub, "#!/bin/sh\necho first\n");
        let mode = std::fs::metadata(&stub).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o755);
        let out = std::process::Command::new(&stub).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "first\n");

        // Rewriting replaces the stub, and leaves no scratch files behind.
        write_executable(&stub, "#!/bin/sh\necho second\n");
        let out = std::process::Command::new(&stub).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "second\n");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["gh"]);
    }

    /// Many threads writing and executing stubs while others fork: the
    /// shape that produced ETXTBSY with `fs::write` + chmod on Linux.
    #[test]
    fn concurrent_write_then_exec_never_hits_text_file_busy() {
        let dir = Scratch::new("concurrent");
        std::thread::scope(|s| {
            for t in 0..6 {
                let dir = dir.path();
                s.spawn(move || {
                    for i in 0..10 {
                        let stub = dir.join(format!("stub-{t}-{i}"));
                        write_executable(&stub, format!("#!/bin/sh\necho {t}-{i}\n"));
                        let out = std::process::Command::new(&stub)
                            .output()
                            .unwrap_or_else(|e| panic!("exec {}: {e}", stub.display()));
                        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{t}-{i}\n"));
                    }
                });
            }
        });
    }
}
