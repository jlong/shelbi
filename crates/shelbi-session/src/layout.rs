//! On-disk layout for a session: `~/.shelbi/sessions/<short-id>/`.
//!
//! The directory holds `sock` (the Unix socket), `lock` (held for the session's
//! lifetime — an unheld lock means the session is dead), `meta.json`, and after
//! exit `exit.json` and `final.txt`. The optional raw output log is `raw.log`.
//!
//! The directory **name is a short hash**, not the readable session name,
//! because the socket path has to fit in a `sockaddr_un.sun_path`, which is only
//! 104 bytes on macOS. The readable name lives in `meta.json`.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// Hard limit on a Unix-domain socket path (`sun_path`), the tightest common
/// value (macOS/BSD). Linux allows 108. We keep every session socket under the
/// smaller bound so the same id scheme works everywhere.
pub const MAX_SOCKET_PATH: usize = 104;

/// The resolved set of paths for one session directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPaths {
    /// The short directory id.
    pub id: String,
    /// `~/.shelbi/sessions/<id>/`.
    pub dir: PathBuf,
}

impl SessionPaths {
    /// Build the paths for `id` under the given sessions root
    /// (`~/.shelbi/sessions`).
    pub fn new(sessions_root: &Path, id: impl Into<String>) -> Self {
        let id = id.into();
        let dir = sessions_root.join(&id);
        Self { id, dir }
    }

    /// Resolve against the live `~/.shelbi/sessions` root.
    pub fn resolve(id: impl Into<String>) -> Result<Self> {
        let root = shelbi_state::sessions_dir().map_err(|e| anyhow::anyhow!(e))?;
        Ok(Self::new(&root, id))
    }

    /// The Unix socket path clients connect to.
    pub fn sock(&self) -> PathBuf {
        self.dir.join("sock")
    }

    /// The lifetime lock file. Held while the session is alive; an unheld lock
    /// means the session is dead and the directory is stale.
    pub fn lock(&self) -> PathBuf {
        self.dir.join("lock")
    }

    /// `meta.json` — the readable session description.
    pub fn meta(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    /// `exit.json` — written when the child exits.
    pub fn exit(&self) -> PathBuf {
        self.dir.join("exit.json")
    }

    /// `final.txt` — the last screen plus recent history as text, on exit.
    pub fn final_txt(&self) -> PathBuf {
        self.dir.join("final.txt")
    }

    /// `raw.log` — the optional full raw output log (off unless the project
    /// enables it).
    pub fn raw_log(&self) -> PathBuf {
        self.dir.join("raw.log")
    }

    /// Fail if the socket path would not fit in `sun_path`. Called before bind
    /// so a too-long path is a clear error, not a truncated socket.
    pub fn check_socket_fits(&self) -> Result<()> {
        let len = self.sock().as_os_str().len();
        // The +1 is the NUL terminator the kernel appends inside sun_path.
        if len + 1 > MAX_SOCKET_PATH {
            bail!(
                "session socket path is {len} bytes, over the {} limit: {}",
                MAX_SOCKET_PATH,
                self.sock().display()
            );
        }
        Ok(())
    }
}

/// Derive a short, collision-resistant directory id for a new session.
///
/// The id is a 16-hex-digit hash of the readable name plus the spawn-time
/// entropy (`pid` and a nanosecond timestamp), so two sessions with the same
/// name launched at different moments do not collide, while the id stays short
/// enough to keep the socket path well under [`MAX_SOCKET_PATH`]. The hash is
/// computed once by the spawner and carried to the session process, so it never
/// needs to be stable across processes or releases.
pub fn derive_id(name: &str, pid: u32, nanos: u128) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    pid.hash(&mut hasher);
    nanos.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Convenience: derive an id from `name` using this process's pid and the
/// current time.
pub fn derive_id_now(name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    derive_id(name, std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_sixteen_hex_digits() {
        let id = derive_id("demo/ws/alpha", 4242, 1_700_000_000_000_000_000);
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn id_varies_with_name_and_entropy() {
        let a = derive_id("demo/ws/alpha", 1, 1);
        let b = derive_id("demo/ws/beta", 1, 1);
        let c = derive_id("demo/ws/alpha", 2, 1);
        let d = derive_id("demo/ws/alpha", 1, 2);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn socket_path_fits_under_the_limit() {
        // A realistic deep home plus the longest readable name shape.
        let root = Path::new("/Users/some-long-username/.shelbi/sessions");
        let id = derive_id("proj/review/slot-3/adversarial", 99999, 123);
        let paths = SessionPaths::new(root, id);
        paths
            .check_socket_fits()
            .expect("socket path should fit under 104 bytes");
        assert!(paths.sock().as_os_str().len() < MAX_SOCKET_PATH);
    }

    #[test]
    fn paths_hang_off_the_id_directory() {
        let paths = SessionPaths::new(Path::new("/root/sessions"), "deadbeefdeadbeef");
        assert_eq!(
            paths.sock(),
            Path::new("/root/sessions/deadbeefdeadbeef/sock")
        );
        assert_eq!(
            paths.meta(),
            Path::new("/root/sessions/deadbeefdeadbeef/meta.json")
        );
        assert_eq!(
            paths.final_txt(),
            Path::new("/root/sessions/deadbeefdeadbeef/final.txt")
        );
    }
}
