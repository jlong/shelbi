//! Best-effort lookup of another process's command, for forensics.
//!
//! A session host records who signalled it in `exit.json` (see
//! [`crate::session`]). The kernel hands the handler only a pid; this resolves
//! it to something a human can act on while the sender may still be alive. It
//! never fails loudly: a sender that already exited (a `pkill` is gone within
//! milliseconds) or a platform without a lookup just yields `None`, and the pid
//! alone is still recorded.

/// Upper bound on the recorded command, so a sender with a huge argv (an agent
/// launched with its whole prompt inline) can't bloat `exit.json`.
const MAX_COMMAND_LEN: usize = 300;

/// The command of process `pid`: its argv joined by spaces on Linux, its
/// executable path on macOS. `None` when the process is gone or unreadable.
pub fn process_command(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let cmd = lookup(pid)?;
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return None;
    }
    Some(truncate(cmd, MAX_COMMAND_LEN))
}

#[cfg(target_os = "linux")]
fn lookup(pid: i32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some(args.join(" "))
}

#[cfg(target_os = "macos")]
fn lookup(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `proc_pidpath` writes at most `buf.len()` bytes into a buffer we
    // own and returns the length written (<= 0 on failure).
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn lookup(_pid: i32) -> Option<String> {
    None
}

/// Cut `s` to at most `max` bytes on a char boundary, marking the cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_our_own_command() {
        let cmd = process_command(std::process::id() as i32).expect("own command");
        assert!(!cmd.is_empty());
    }

    #[test]
    fn unknown_and_sentinel_pids_resolve_to_none() {
        assert_eq!(process_command(0), None);
        assert_eq!(process_command(-1), None);
    }

    #[test]
    fn truncate_cuts_on_a_char_boundary() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc…");
        // 'é' is two bytes; a cut inside it backs off to the boundary.
        assert_eq!(truncate("aé", 2), "a…");
    }
}
