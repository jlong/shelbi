//! The runner of a `shelbi job run` command: `shelbi __job-exec <id> -- <cmd...>`.
//!
//! `job run` starts the runner as the child of a Shelbi session named
//! [`job_session_name`], so the job has a terminal a client can attach to and
//! watch (the Jobs screen's side sheet). The runner then runs the command in a
//! pseudo-terminal of its own, under the user's login shell, and copies what
//! the command prints to two places:
//!
//! - its own stdout, the session's PTY, raw, so an attached client sees the
//!   live terminal (colors, progress bars, cursor movement);
//! - the job's `output.log`, as plain text ([`PlainText`]), so `shelbi job
//!   log`, the history, and the `job` event's last line read the same text
//!   they always have, after the session is gone.
//!
//! Owning that second PTY is what lets the runner see every byte and drain
//! the output before it writes `result.json`, so the result's tail is never
//! short of the output's end.
//!
//! The runner leads the session's process group; the command leads its own
//! (a PTY's child is a session leader), so the runner passes on the signals
//! that stop a job (`job cancel`'s SIGTERM to its group, the session's own
//! kill, a hangup) to the command's group and still writes the result once
//! the command has gone.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use shelbi_core::{Error, Result};
use shelbi_state::awareness::{self, JobResult};

pub use shelbi_state::awareness::{is_job_session, job_of_session, job_session_name};

/// The size the command's terminal starts at when the runner has no terminal
/// of its own to copy (the session's default).
const DEFAULT_SIZE: (u16, u16) = (120, 40);

/// After the command exits, how long the runner keeps reading output a
/// background process it left behind may still be writing. The PTY stays
/// open while anything holds it, so this bounds the wait.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

/// The file `job run` leaves the caller's environment in, for the runner to
/// read and delete. The session hands its child a scrubbed login
/// environment, but a job runs with the environment of whoever started it, as
/// it always has.
const ENV_FILE: &str = "env.json";

/// Leave the caller's environment for job `id`'s runner (owner-only, deleted
/// by the runner as soon as it reads it).
pub(crate) fn write_job_env(project: &str, id: &str) -> Result<()> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let body = serde_json::to_vec(&env).map_err(|e| Error::Other(format!("job env: {e}")))?;
    let path = awareness::job_dir(project, id)?.join(ENV_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(Error::Io)?;
    file.write_all(&body).map_err(Error::Io)
}

/// Read and delete the environment `job run` left for job `id`. `None` when
/// there is none (a runner started some other way).
fn take_job_env(project: &str, id: &str) -> Option<BTreeMap<String, String>> {
    let path = awareness::job_dir(project, id).ok()?.join(ENV_FILE);
    let text = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_slice(&text).ok()
}

/// The terminal variables the session sets for its child, which the
/// command's terminal (a copy of the session's) should carry rather than the
/// caller's.
const TERMINAL_VARS: [&str; 3] = ["TERM", "COLORTERM", "TERM_PROGRAM"];

/// The command line a job runs: `argv` (shell-quoted, as `job list` shows
/// it) under `shell` as a login shell, so `PATH` is what the user would have
/// typing it into a terminal. The shell exits when the command does, with its
/// status.
pub fn login_shell_argv(shell: &str, argv: &[String]) -> Vec<String> {
    let line = argv
        .iter()
        .map(|a| shelbi_core::shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ");
    vec![shell.to_string(), "-l".into(), "-c".into(), line]
}

/// The body of `shelbi __job-exec <id> -- <cmd...>`: record this runner's pid
/// on the job, then run the command under the caller's login shell
/// ([`run_job_command`]).
pub fn run_job_exec(project: &str, id: &str, argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        return Err(Error::Other("no command given".into()));
    }
    // The runner leads its process group, so its pid is what `job cancel`
    // signals and the tick watches. A job canceled before this line runs is
    // gone from the store, and this is a no-op.
    if let Err(e) = awareness::set_job_runner_pid(project, id, std::process::id()) {
        tracing::warn!(project, job = %id, error = %e, "job runner pid not recorded");
    }
    let mut env = take_job_env(project, id);
    if let Some(env) = env.as_mut() {
        for key in TERMINAL_VARS {
            match std::env::var(key) {
                Ok(value) => env.insert(key.to_string(), value),
                Err(_) => env.remove(key),
            };
        }
    }
    let shell = env
        .as_ref()
        .and_then(|e| e.get("SHELL").cloned())
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string());
    run_command(project, id, &login_shell_argv(&shell, argv), env.as_ref(), true)
}

/// Run `argv` in a PTY of its own, its output as plain text in job `id`'s
/// `output.log`, then write `result.json`. `env` replaces the environment
/// when given. A command that can't be started at all still gets a result
/// (exit 127 and why), so the job never goes unreported. What the runner does
/// in a session, less the session: no copy to stdout, no signal or input
/// forwarding.
pub fn run_job_command(
    project: &str,
    id: &str,
    argv: &[String],
    env: Option<&BTreeMap<String, String>>,
) -> Result<()> {
    run_command(project, id, argv, env, false)
}

/// [`run_job_command`]; `live` is the runner in its session: the raw output
/// also goes to stdout (the session's terminal), input comes back from it,
/// and the signals that end a job are passed on to the command.
fn run_command(
    project: &str,
    id: &str,
    argv: &[String],
    env: Option<&BTreeMap<String, String>>,
    live: bool,
) -> Result<()> {
    let output_path = awareness::job_output_path(project, id)?;
    if let Some(parent) = output_path.parent() {
        shelbi_state::ensure_dir(parent)?;
    }
    let status = run_in_pty(&output_path, argv, env, live);
    let tail = awareness::tail_lines(&output_path, awareness::JOB_TAIL_LINES);
    let result = match status {
        Ok(Exit { code, signal }) => JobResult {
            exit_code: code,
            signal,
            finished_at: Utc::now(),
            tail,
            error: None,
        },
        Err(error) => JobResult {
            exit_code: Some(127),
            signal: None,
            finished_at: Utc::now(),
            tail,
            error: Some(error),
        },
    };
    awareness::write_job_result(project, id, &result)
}

/// How the command ended: an exit code, or the signal that ended it.
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
}

/// The runner's own terminal size, when its stdout is a terminal.
fn own_terminal_size() -> Option<(u16, u16)> {
    // SAFETY: TIOCGWINSZ fills a plain struct; a non-terminal fd just fails.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            Some((ws.ws_col, ws.ws_row))
        } else {
            None
        }
    }
}

fn pty_size((cols, rows): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn run_in_pty(
    output_path: &Path,
    argv: &[String],
    env: Option<&BTreeMap<String, String>>,
    live: bool,
) -> std::result::Result<Exit, String> {
    let program = argv.first().ok_or("no command given")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_path)
        .map_err(|e| format!("cannot open {}: {e}", output_path.display()))?;
    let pair = native_pty_system()
        .openpty(pty_size(own_terminal_size().unwrap_or(DEFAULT_SIZE)))
        .map_err(|e| format!("cannot open a terminal for the command: {e}"))?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(&argv[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    if let Some(env) = env {
        cmd.env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("could not start `{program}`: {e}"))?;
    // Only the command holds the terminal now, so it closes when the command
    // (and anything it left running on it) is gone.
    drop(pair.slave);
    let pid = child.process_id().and_then(|p| libc::pid_t::try_from(p).ok()).filter(|p| *p > 1);
    let Some(pid) = pid else {
        return Err(format!("could not start `{program}`: no process id"));
    };
    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));

    let reader = master
        .lock()
        .unwrap()
        .try_clone_reader()
        .map_err(|e| format!("cannot read the command's terminal: {e}"))?;
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        copy_output(reader, log, live);
        let _ = done_tx.send(());
    });
    // Input from the session's terminal (an attached client's, or the
    // session answering a terminal query) goes on to the command's. Only when
    // there is a terminal: a dropped PTY writer types an end-of-file.
    // SAFETY: isatty on a standard fd.
    if live && unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        if let Ok(writer) = master.lock().unwrap().take_writer() {
            std::thread::spawn(move || copy_input(writer));
        }
    }
    if live {
        forward_signals(pid, master.clone());
    }

    let exit = wait_for(pid)?;
    let _ = done_rx.recv_timeout(DRAIN_AFTER_EXIT);
    drop(child);
    Ok(exit)
}

/// Copy the command's output to the log (plain text), and to stdout (raw)
/// when `echo`, until its terminal closes.
fn copy_output(mut reader: Box<dyn Read + Send>, log: std::fs::File, echo: bool) {
    let mut log = std::io::BufWriter::new(log);
    let mut plain = PlainText::default();
    let mut text = Vec::new();
    let mut chunk = [0u8; 8192];
    let stdout = std::io::stdout();
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if echo {
            let mut out = stdout.lock();
            let _ = out.write_all(&chunk[..n]);
            let _ = out.flush();
        }
        text.clear();
        plain.push(&chunk[..n], &mut text);
        if !text.is_empty() {
            let _ = log.write_all(&text);
            let _ = log.flush();
        }
    }
    text.clear();
    plain.finish(&mut text);
    let _ = log.write_all(&text);
    let _ = log.flush();
}

fn copy_input(mut writer: Box<dyn Write + Send>) {
    let mut stdin = std::io::stdin();
    let mut chunk = [0u8; 1024];
    loop {
        match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if writer.write_all(&chunk[..n]).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        }
    }
    // Keep the writer: dropping it would type an end-of-file at the command.
    std::mem::forget(writer);
}

/// Pass the signals that end a job on to the command's process group, and
/// follow the session's terminal size. The runner itself stays up to record
/// how the command ended.
fn forward_signals(pid: libc::pid_t, master: Arc<Mutex<Box<dyn MasterPty + Send>>>) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
    let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP, SIGINT, SIGWINCH]) else {
        return;
    };
    std::thread::spawn(move || {
        for signal in signals.forever() {
            if signal == SIGWINCH {
                if let Some(size) = own_terminal_size() {
                    let _ = master.lock().unwrap().resize(pty_size(size));
                }
                continue;
            }
            // A hangup stays a hangup; an interrupt or terminate is a
            // terminate, what `job cancel` has always sent the command.
            let forward = if signal == SIGHUP { libc::SIGHUP } else { libc::SIGTERM };
            // SAFETY: kill(2) on the negated pgid of the command this runner
            // started (a PTY child leads its own group; pid > 1 checked).
            unsafe {
                libc::kill(-pid, forward);
            }
        }
    });
}

/// Wait for `pid` (this runner's child) and decode its status.
fn wait_for(pid: libc::pid_t) -> std::result::Result<Exit, String> {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waitpid on our own child into a local.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        let err = std::io::Error::last_os_error();
        if r == -1 && err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(format!("waiting for the command: {err}"));
    }
    if libc::WIFEXITED(status) {
        Ok(Exit {
            code: Some(libc::WEXITSTATUS(status)),
            signal: None,
        })
    } else if libc::WIFSIGNALED(status) {
        Ok(Exit {
            code: None,
            signal: Some(libc::WTERMSIG(status)),
        })
    } else {
        Ok(Exit { code: None, signal: None })
    }
}

/// Turns a terminal's output stream into the plain text a log holds: escape
/// sequences (colors, cursor movement, titles) dropped, `\r\n` as `\n`, a
/// carriage return that redraws a line (a progress bar) keeping only what the
/// line ends up as, a backspace erasing, other control characters dropped.
/// Text is written a line at a time, so a line being redrawn is written once,
/// finished; [`finish`](Self::finish) writes a last line that never ended.
#[derive(Debug, Default)]
pub struct PlainText {
    state: EscState,
    line: Vec<u8>,
    /// A carriage return was the last thing seen: the next text redraws the
    /// line, unless a newline comes first.
    returned: bool,
}

/// Where a [`PlainText`] is inside an escape sequence.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum EscState {
    #[default]
    Text,
    /// After ESC.
    Esc,
    /// After ESC and intermediate bytes (`ESC ( B`).
    EscIntermediate,
    /// Inside `ESC [ …` until its final byte.
    Csi,
    /// Inside a string (`ESC ]`, `ESC P`, `ESC _`, `ESC ^`, `ESC X`) until
    /// BEL or ST.
    Str,
    /// ESC inside a string: `\` ends it.
    StrEsc,
}

/// A line longer than this is written as it stands, so text that never
/// ends a line can't grow the buffer without bound.
const MAX_LINE: usize = 64 * 1024;

impl PlainText {
    /// Feed `bytes`, appending each line they finish to `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &b in bytes {
            match self.state {
                EscState::Text => self.text(b, out),
                EscState::Esc => {
                    self.state = match b {
                        b'[' => EscState::Csi,
                        b']' | b'P' | b'_' | b'^' | b'X' => EscState::Str,
                        0x20..=0x2f => EscState::EscIntermediate,
                        _ => EscState::Text,
                    }
                }
                EscState::EscIntermediate => {
                    if !(0x20..=0x2f).contains(&b) {
                        self.state = EscState::Text;
                    }
                }
                EscState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = EscState::Text;
                    }
                }
                EscState::Str => match b {
                    0x07 => self.state = EscState::Text,
                    0x1b => self.state = EscState::StrEsc,
                    _ => {}
                },
                EscState::StrEsc => {
                    self.state = if b == b'\\' { EscState::Text } else { EscState::Str };
                }
            }
        }
    }

    /// Write a last line the output never ended.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.line.is_empty() {
            out.append(&mut self.line);
            out.push(b'\n');
        }
        self.returned = false;
    }

    fn text(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            0x1b => self.state = EscState::Esc,
            b'\n' => {
                out.append(&mut self.line);
                out.push(b'\n');
                self.returned = false;
            }
            b'\r' => self.returned = true,
            0x08 => {
                self.redraw_if_returned();
                // Back over one character, a whole UTF-8 sequence.
                while let Some(last) = self.line.pop() {
                    if last & 0xc0 != 0x80 {
                        break;
                    }
                }
            }
            b'\t' => self.put(b, out),
            0x00..=0x1f | 0x7f => {}
            _ => self.put(b, out),
        }
    }

    fn redraw_if_returned(&mut self) {
        if self.returned {
            self.line.clear();
            self.returned = false;
        }
    }

    fn put(&mut self, b: u8, out: &mut Vec<u8>) {
        self.redraw_if_returned();
        self.line.push(b);
        if self.line.len() >= MAX_LINE {
            out.append(&mut self.line);
            out.push(b'\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(chunks: &[&[u8]]) -> String {
        let mut p = PlainText::default();
        let mut out = Vec::new();
        for chunk in chunks {
            p.push(chunk, &mut out);
        }
        p.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plain_text_drops_escapes_and_terminal_line_ends() {
        assert_eq!(plain(&[b"\x1b[1;31mred\x1b[0m done\r\n"]), "red done\n");
        assert_eq!(plain(&[b"\x1b]0;title\x07a\x1b]8;;http://x\x1b\\b\r\n"]), "ab\n");
        assert_eq!(plain(&[b"\x1b(Bplain\x1b=\r\n"]), "plain\n");
    }

    #[test]
    fn plain_text_keeps_what_a_redrawn_line_ends_as() {
        assert_eq!(plain(&[b"10%\r50%\r100%\r\nnext\r\n"]), "100%\nnext\n");
        assert_eq!(plain(&[b"abc\x08\x08x\r\n"]), "ax\n");
        assert_eq!(plain(&[b"caf\xc3\xa9\x08e\r\n"]), "cafe\n");
    }

    #[test]
    fn plain_text_survives_sequences_split_across_reads() {
        assert_eq!(plain(&[b"a\x1b[3", b"8;5;2", b"0mb\r", b"\nc"]), "ab\nc\n");
        assert_eq!(plain(&[b"x\x1b]0;t", b"\x1b", b"\\y\n"]), "xy\n");
    }

    #[test]
    fn a_job_runs_under_a_login_shell_with_its_argv_quoted() {
        let argv: Vec<String> = ["zsh", "-c", "for i in 1 2; do echo $i; done"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            login_shell_argv("/bin/zsh", &argv),
            ["/bin/zsh", "-l", "-c", "zsh -c 'for i in 1 2; do echo $i; done'"]
        );
    }
}
