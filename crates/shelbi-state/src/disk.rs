//! Free-space probe for the volume a path lives on.
//!
//! A full data volume is the failure mode that looks like a wedged hub —
//! probes hang, heartbeats stop, CLI calls take minutes — so the heartbeat
//! line and `shelbi status` surface how much room is left on the volume
//! holding a project's `work_dir`. This is the one primitive both read.
//!
//! The probe is best-effort: it returns `None` rather than erroring when the
//! platform has no `statvfs` or the syscall fails, so a disk read can never
//! block a heartbeat or a status print.

use std::path::Path;

/// Bytes of free space available to an unprivileged process on the filesystem
/// that contains `path`, or `None` if it cannot be determined.
///
/// Uses `statvfs(2)` on Unix and reports `f_bavail * f_frsize` — the blocks
/// available to a non-root caller, which is the number that matches what a
/// worker's build can actually use (not the root-reserved `f_bfree`). Returns
/// `None` on non-Unix targets or when the syscall fails.
#[cfg(unix)]
pub fn free_space_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    // statvfs wants a NUL-terminated path. A path with an interior NUL can't
    // name a real file, so bail rather than truncate silently.
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    if bytes.contains(&0) {
        return None;
    }
    bytes.push(0);

    // SAFETY: `stat` is a plain POD struct we zero-initialize and hand to the
    // kernel to fill; `bytes` is a valid NUL-terminated C string that outlives
    // the call. We only read scalar fields from `stat` afterward.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(bytes.as_ptr() as *const libc::c_char, &mut stat) };
    if rc != 0 {
        return None;
    }

    // `f_frsize` is the fundamental block size the counts are expressed in.
    // Fall back to `f_bsize` on the rare platform that reports frsize as 0.
    let frag = if stat.f_frsize != 0 {
        stat.f_frsize as u64
    } else {
        stat.f_bsize as u64
    };
    (stat.f_bavail as u64).checked_mul(frag)
}

/// Non-Unix fallback: free space cannot be probed, so callers treat disk as
/// "unknown" and stay quiet rather than warn.
#[cfg(not(unix))]
pub fn free_space_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn free_space_of_an_existing_dir_is_some_and_positive() {
        // The temp dir always exists on a Unix test host; a live filesystem
        // reports a positive, plausible free-byte count.
        let free = free_space_bytes(std::env::temp_dir().as_path());
        assert!(free.is_some(), "statvfs on the temp dir should succeed");
        assert!(free.unwrap() > 0, "a live volume has some free space");
    }

    #[test]
    fn free_space_of_a_missing_path_is_none() {
        let missing = std::path::Path::new("/this/path/should/not/exist/anywhere-xyz");
        assert_eq!(free_space_bytes(missing), None);
    }

    #[test]
    fn free_space_of_a_path_with_interior_nul_is_none() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = std::path::PathBuf::from(OsStr::from_bytes(b"/tmp/\0bad"));
        assert_eq!(free_space_bytes(&bad), None);
    }
}
