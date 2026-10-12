//! Raise this process's open-file-descriptor soft limit at startup.
//!
//! Every long-lived shelbi process that fans out over Unix sockets — the hub
//! daemon and each `shelbi __session` — opens a descriptor per client
//! connection, per serving thread, and per throwaway liveness/`info`/`snapshot`
//! probe. launchd hands a GUI-session process a soft `RLIMIT_NOFILE` of only
//! 256, and a session that briefly leaks connections (or simply serves many
//! clients) can walk right into it: `accept(2)` starts returning `EMFILE`, the
//! listener stops taking new connections, and the session becomes an "alive but
//! not listening" zombie that supervision then reaps and relaunches in a loop
//! (`rt-review-slot-relaunch-loop`).
//!
//! Raising the *soft* limit toward the *hard* limit at startup gives these
//! processes the headroom the default denies them, without needing elevated
//! privileges (lowering the hard limit needs root; raising the soft limit up to
//! the hard limit never does). It is a best-effort nicety: a platform without
//! `getrlimit`/`setrlimit`, or a hard limit already at or below the soft one,
//! simply leaves the limit untouched.

/// The soft `RLIMIT_NOFILE` we aim for when the hard limit allows it. Chosen to
/// be comfortably above the launchd default (256) while staying modest enough
/// that it is almost always ≤ the hard limit (macOS hard default is typically
/// `unlimited`/large; Linux is commonly 524288 or `unlimited`). The soft limit
/// is clamped to the hard limit regardless, so this is only a target.
pub const TARGET_SOFT_NOFILE: u64 = 8192;

/// Raise this process's soft open-file limit toward [`TARGET_SOFT_NOFILE`]
/// (clamped to the current hard limit), returning the `(previous_soft,
/// new_soft)` pair actually in effect when the call succeeds.
///
/// Best-effort and idempotent: if the soft limit already meets the target (or
/// the hard limit caps it below the target and the soft limit is already at the
/// hard limit) nothing is changed and `previous == new`. A `getrlimit` /
/// `setrlimit` failure returns the OS error rather than panicking — the caller
/// logs it and carries on, since a too-low limit degrades gracefully (the accept
/// loop backs off on `EMFILE`) rather than being fatal.
#[cfg(unix)]
pub fn raise_file_limit() -> std::io::Result<(u64, u64)> {
    // SAFETY: `getrlimit`/`setrlimit` with a stack-allocated `rlimit` we fully
    // initialize; the pointers are valid for the duration of each call.
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // `rlim_t` is `u64` on every platform shelbi targets (macOS, Linux), so
        // the limits are read as `u64` directly — no cross-platform cast dance.
        let prev_soft: u64 = lim.rlim_cur;
        let hard: u64 = lim.rlim_max;
        // Never lower the soft limit; aim for the target but never exceed the
        // hard limit (raising the soft limit above it would be rejected).
        let target = TARGET_SOFT_NOFILE.min(hard).max(prev_soft);
        if target == prev_soft {
            return Ok((prev_soft, prev_soft));
        }
        lim.rlim_cur = target;
        if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((prev_soft, target))
    }
}

/// Non-unix stub: there is no `RLIMIT_NOFILE` to raise, so report no change.
#[cfg(not(unix))]
pub fn raise_file_limit() -> std::io::Result<(u64, u64)> {
    Ok((0, 0))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn raising_never_lowers_the_soft_limit() {
        // Whatever the environment's starting limit, the call must never return
        // a new soft limit below the previous one.
        let (prev, new) = raise_file_limit().expect("getrlimit/setrlimit on a unix test host");
        assert!(new >= prev, "soft limit must never be lowered: {prev} -> {new}");
    }

    #[test]
    fn raising_is_idempotent() {
        // A second call after the first has raised the limit is a no-op: the
        // soft limit already meets (or is capped at) the target.
        let (_p1, n1) = raise_file_limit().expect("first raise");
        let (p2, n2) = raise_file_limit().expect("second raise");
        assert_eq!(p2, n1, "the second call sees the first call's new soft limit");
        assert_eq!(n2, n1, "a second raise changes nothing");
    }
}
