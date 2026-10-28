//! Keeping stray text off the shell's screen (`tui-stderr-corrupts-screen`).
//!
//! The shell draws on the terminal its stderr shares. Anything written there
//! while the alternate screen is up (a `gh` retry notice, a stray `eprintln!`
//! in a library, a child process's inherited stderr) lands at the cursor,
//! wraps across the sidebar and can scroll the screen; ratatui only repaints
//! cells its own buffer thinks changed, so the debris stays. Two guards:
//!
//! - GitHub retry notices, the known writer, go to the shell's status band
//!   and `tui.log` through [`install_retry_notices`] instead of stderr.
//! - For everything else, fd 2 points at `~/.shelbi/logs/tui.log` for the life
//!   of the alternate screen ([`redirect_stderr`]) and is put back by
//!   [`restore_stderr`] whenever the terminal is restored, so an editor, a
//!   panic message or a detach note still reaches the user.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

/// The terminal's own stderr, saved while fd 2 points at the log; -1 when
/// stderr is not redirected.
static SAVED_STDERR: AtomicI32 = AtomicI32::new(-1);

/// Point fd 2 at the TUI log (or `/dev/null` when the log can't be opened:
/// silence beats text over the frame). Child processes started meanwhile
/// inherit it too. Safe to call again while redirected.
pub(super) fn redirect_stderr() {
    let target = open_log().or_else(|| OpenOptions::new().write(true).open("/dev/null").ok());
    if let Some(file) = target {
        redirect_stderr_to(&file);
    }
}

/// `~/.shelbi/logs/tui.log`, the file the TUI's `tracing` output goes to.
fn open_log() -> Option<File> {
    let dir = shelbi_state::shelbi_home().ok()?.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("tui.log"))
        .ok()
}

/// Point fd 2 at `file`, saving the terminal's stderr the first time. Returns
/// whether fd 2 now points at `file`.
fn redirect_stderr_to(file: &File) -> bool {
    if SAVED_STDERR.load(Ordering::SeqCst) < 0 {
        // Close-on-exec, so a child never holds the terminal's stderr open.
        // SAFETY: plain fd syscalls on descriptors this process owns.
        let saved = unsafe { libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 0) };
        if saved < 0 {
            return false;
        }
        SAVED_STDERR.store(saved, Ordering::SeqCst);
    }
    // SAFETY: as above; `file` stays open for the call.
    unsafe { libc::dup2(file.as_raw_fd(), 2) >= 0 }
}

/// Put the terminal's stderr back on fd 2. A no-op when not redirected, so the
/// panic hook, the RAII guard and an explicit restore can all call it.
pub(super) fn restore_stderr() {
    let saved = SAVED_STDERR.swap(-1, Ordering::SeqCst);
    if saved >= 0 {
        // SAFETY: `saved` is the descriptor `redirect_stderr_to` duplicated.
        unsafe {
            libc::dup2(saved, 2);
            libc::close(saved);
        }
    }
}

/// One GitHub retry notice for the status band: the line, and how long the
/// policy waits before its next attempt.
pub(super) type RetryNoticeMsg = (String, Duration);

/// Send this process's GitHub retry notices to the returned channel (the
/// shell's status band) and `tui.log` instead of stderr. Replaces any sink
/// installed before; [`uninstall_retry_notices`] goes back to stderr.
pub(super) fn install_retry_notices() -> Receiver<RetryNoticeMsg> {
    let (tx, rx) = mpsc::channel();
    shelbi_state::set_retry_notice_sink(Arc::new(move |n: &shelbi_state::RetryNotice| {
        let line = n.message();
        tracing::warn!("{line}");
        let _ = tx.send((line, n.wait));
    }));
    rx
}

/// Notices go back to stderr: the shell has given the terminal back.
pub(super) fn uninstall_retry_notices() {
    shelbi_state::clear_retry_notice_sink();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn a_redirect_sends_fd_2_to_the_file_until_restored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stderr.log");
        let file = File::create(&path).unwrap();
        // Raw writes to fd 2: what a library's `eprintln!` or a child process
        // does outside the test harness's capture.
        let write2 = |bytes: &[u8]| unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };

        assert!(redirect_stderr_to(&file));
        // A second redirect keeps the terminal's stderr as the one to restore.
        assert!(redirect_stderr_to(&file));
        write2(b"stray line\n");
        restore_stderr();
        restore_stderr();
        assert_eq!(SAVED_STDERR.load(Ordering::SeqCst), -1);

        let mut text = String::new();
        File::open(&path).unwrap().read_to_string(&mut text).unwrap();
        // `contains`: another test's child may write to fd 2 in the window.
        assert!(text.contains("stray line\n"), "{text:?}");
    }
}
