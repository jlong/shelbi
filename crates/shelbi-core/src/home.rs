//! The user's home directory, without the `dirs` crate: std's lookup, plus
//! the one case where `dirs` read it differently.

use std::path::PathBuf;

/// The user's home directory, as `dirs::home_dir` found it: `$HOME` when it
/// is set and non-empty, else the password database's entry for this user.
/// std reads a set `$HOME` verbatim, an empty one included, so only that case
/// goes to [`account_home_dir`] here; an unset `$HOME` is std's own fallback
/// to the same database.
#[allow(deprecated)] // `std::env::home_dir`, deprecated before Rust 1.87.
pub fn home_dir() -> Option<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if home.is_empty() => account_home_dir(),
        _ => std::env::home_dir(),
    }
}

/// The home directory the password database records for this user, whatever
/// `$HOME` says (a test that swaps `$HOME` can't move it). `None` when there
/// is no entry or it names no directory.
pub fn account_home_dir() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    loop {
        // SAFETY: `passwd` is plain data, zeroed until `getpwuid_r` fills it;
        // its string fields point into `buf`, which outlives every read below.
        let mut record: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe { libc::getpwuid_r(libc::getuid(), &mut record, buf.as_mut_ptr(), buf.len(), &mut found) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || record.pw_dir.is_null() {
            return None;
        }
        // SAFETY: a found record's `pw_dir` is a NUL-terminated string in `buf`.
        let dir = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes();
        return (!dir.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(dir)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_home_is_an_absolute_directory() {
        let home = account_home_dir().expect("the test user has a passwd entry");
        assert!(home.is_absolute(), "{}", home.display());
    }

    /// Reads `$HOME` without changing it (other tests in this binary read the
    /// environment concurrently).
    #[test]
    fn a_set_home_is_read_verbatim() {
        match std::env::var_os("HOME") {
            Some(h) if !h.is_empty() => assert_eq!(home_dir(), Some(PathBuf::from(h))),
            _ => assert_eq!(home_dir(), account_home_dir()),
        }
    }
}
