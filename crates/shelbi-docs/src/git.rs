//! The git CLI, run in one directory, for the docs' sync.
//!
//! Shelbi's own code (not copied from ContextStore, whose `cstore-git` wraps
//! libgit2): the docs sync with the user's `git`, so their SSH setup,
//! credential helpers and commit signing apply as they do everywhere else.
//!
//! Every run is non-interactive (no terminal prompt, no editor, stdin closed)
//! and bounded by a deadline, so a remote that hangs can't wedge the hub's
//! sync thread. Output is read with `LC_ALL=C` so the few messages the sync
//! classifies (offline, rejected push, missing remote branch) read the same
//! on every machine.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one local git command may take.
pub const LOCAL_DEADLINE: Duration = Duration::from_secs(60);
/// How long one fetch or push may take.
pub const NETWORK_DEADLINE: Duration = Duration::from_secs(120);

/// Why a git command didn't succeed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitError {
    /// `git` couldn't be started.
    #[error("couldn't run git: {0}")]
    Spawn(String),
    /// It ran past its deadline and was killed.
    #[error("git {command} took longer than {}s", .after.as_secs())]
    Timeout { command: String, after: Duration },
    /// It exited unsuccessfully.
    #[error("git {command} failed: {}", detail(.stderr, .stdout))]
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
        stdout: String,
    },
}

fn detail<'a>(stderr: &'a str, stdout: &'a str) -> &'a str {
    let s = stderr.trim();
    if s.is_empty() {
        stdout.trim()
    } else {
        s
    }
}

impl GitError {
    /// Whether this reads as the remote being out of reach (no network, a
    /// host that doesn't resolve, a remote that refuses or times out), as
    /// opposed to a real failure.
    pub fn is_offline(&self) -> bool {
        match self {
            GitError::Timeout { .. } => true,
            GitError::Spawn(_) => false,
            GitError::Failed { stderr, .. } => {
                let m = stderr.to_ascii_lowercase();
                [
                    "could not resolve host",
                    "could not resolve hostname",
                    "could not read from remote repository",
                    "unable to access",
                    "connection refused",
                    "connection timed out",
                    "operation timed out",
                    "network is unreachable",
                    "no route to host",
                    "failed to connect",
                    "connection reset",
                    "does not appear to be a git repository",
                ]
                .iter()
                .any(|k| m.contains(k))
            }
        }
    }

    /// Whether a push was refused because the remote moved on (it needs a
    /// pull first).
    pub fn is_rejected_push(&self) -> bool {
        match self {
            GitError::Failed { stderr, .. } => {
                let m = stderr.to_ascii_lowercase();
                m.contains("[rejected]") || m.contains("non-fast-forward") || m.contains("fetch first")
            }
            _ => false,
        }
    }

    /// The command's stderr, for the callers that classify it.
    pub fn stderr(&self) -> &str {
        match self {
            GitError::Failed { stderr, .. } => stderr,
            _ => "",
        }
    }
}

/// What a finished git command printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// A finished command's exit code, stdout and stderr, as bytes.
type Raw = (Option<i32>, Vec<u8>, Vec<u8>);

/// Runs git in one directory.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
    env: Vec<(String, String)>,
    /// `-c` settings put before every command (an identity fallback).
    config: Vec<String>,
}

impl Git {
    /// Git in `dir`, with the process's own environment.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            env: Vec::new(),
            config: Vec::new(),
        }
    }

    /// The same runner with `env` added to every command's environment.
    pub fn with_env(mut self, env: &[(String, String)]) -> Self {
        self.env.extend(env.iter().cloned());
        self
    }

    /// The same runner in `dir`.
    pub fn at(&self, dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            env: self.env.clone(),
            config: self.config.clone(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Commit as Shelbi when the user's git config names no one, so a hub on
    /// a machine without `user.email` still commits instead of failing every
    /// sync. A configured identity always wins.
    pub fn ensure_identity(&mut self) {
        let has = |key: &str| {
            self.run(&["config", "--get", key], LOCAL_DEADLINE)
                .map(|o| o.success() && !o.stdout.trim().is_empty())
                .unwrap_or(false)
        };
        let (email, name) = (has("user.email"), has("user.name"));
        if !email {
            self.config.push("user.email=docs@shelbi.invalid".into());
        }
        if !name {
            self.config.push("user.name=Shelbi".into());
        }
    }

    /// Run `git <args>` whatever its exit status.
    pub fn run(&self, args: &[&str], deadline: Duration) -> Result<GitOutput, GitError> {
        self.run_with_input(args, None, deadline)
    }

    /// Run `git <args>`, requiring success, and answer its stdout.
    pub fn ok(&self, args: &[&str]) -> Result<String, GitError> {
        self.ok_within(args, LOCAL_DEADLINE)
    }

    /// [`Self::ok`] with its own deadline (a fetch, a push).
    pub fn ok_within(&self, args: &[&str], deadline: Duration) -> Result<String, GitError> {
        let out = self.run(args, deadline)?;
        if out.success() {
            Ok(out.stdout)
        } else {
            Err(failed(args, out))
        }
    }

    /// Run `git <args>` with `input` on stdin, requiring success.
    pub fn ok_with_input(&self, args: &[&str], input: &[u8]) -> Result<String, GitError> {
        let out = self.run_with_input(args, Some(input), LOCAL_DEADLINE)?;
        if out.success() {
            Ok(out.stdout)
        } else {
            Err(failed(args, out))
        }
    }

    /// The raw bytes `git <args>` prints, requiring success (file contents).
    pub fn bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let (code, stdout, stderr) = self.spawn(args, None, LOCAL_DEADLINE)?;
        if code == Some(0) {
            Ok(stdout)
        } else {
            Err(failed(
                args,
                GitOutput {
                    code,
                    stdout: String::new(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                },
            ))
        }
    }

    fn run_with_input(&self, args: &[&str], input: Option<&[u8]>, deadline: Duration) -> Result<GitOutput, GitError> {
        let (code, stdout, stderr) = self.spawn(args, input, deadline)?;
        Ok(GitOutput {
            code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn spawn(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        deadline: Duration,
    ) -> Result<Raw, GitError> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.dir);
        cmd.args(["-c", "core.quotepath=off"]);
        for c in &self.config {
            cmd.arg("-c").arg(c);
        }
        cmd.args(args);
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C")
            .env("LANG", "C");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| GitError::Spawn(e.to_string()))?;
        if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
            let bytes = bytes.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            });
        }
        // Read both pipes on their own threads. A git that exits while a
        // child it started still holds a pipe (an SSH ControlPersist master
        // keeps stderr open) must not block the sync, so the reads are
        // collected with a short grace after the exit, not joined.
        let out_rx = read_pipe(child.stdout.take());
        let err_rx = read_pipe(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GitError::Timeout {
                        command: args.first().copied().unwrap_or("").to_string(),
                        after: deadline,
                    });
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(GitError::Spawn(e.to_string())),
            }
        };
        let stdout = out_rx.recv_timeout(PIPE_GRACE).unwrap_or_default();
        let stderr = err_rx.recv_timeout(PIPE_GRACE).unwrap_or_default();
        Ok((status.code(), stdout, stderr))
    }
}

/// How long a finished command's output may take to drain.
const PIPE_GRACE: Duration = Duration::from_secs(2);

fn read_pipe(pipe: Option<impl Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

fn failed(args: &[&str], out: GitOutput) -> GitError {
    GitError::Failed {
        command: args.first().copied().unwrap_or("").to_string(),
        code: out.code,
        stderr: out.stderr,
        stdout: out.stdout,
    }
}

/// Split `-z` output into its NUL-separated fields.
pub fn split_z(out: &str) -> Vec<&str> {
    out.split('\0').filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_with(stderr: &str) -> GitError {
        GitError::Failed {
            command: "fetch".into(),
            code: Some(128),
            stderr: stderr.into(),
            stdout: String::new(),
        }
    }

    #[test]
    fn network_failures_read_as_offline() {
        assert!(failed_with("ssh: Could not resolve hostname github.com: nodename nor servname").is_offline());
        assert!(failed_with("fatal: Could not read from remote repository.").is_offline());
        assert!(failed_with("fatal: unable to access 'https://x/': Failed to connect").is_offline());
        assert!(!failed_with("fatal: not a git repository").is_offline());
    }

    #[test]
    fn a_rejected_push_is_told_apart() {
        assert!(failed_with(" ! [rejected]        main -> main (fetch first)").is_rejected_push());
        assert!(!failed_with("fatal: Could not read from remote repository.").is_rejected_push());
    }

    #[test]
    fn a_command_past_its_deadline_is_killed() {
        let dir = std::env::temp_dir();
        // `git -c alias...` runs a shell alias that sleeps past the deadline.
        let git = Git::new(&dir);
        let err = git
            .run(&["-c", "alias.nap=!sleep 5", "nap"], Duration::from_millis(200))
            .unwrap_err();
        assert!(matches!(err, GitError::Timeout { .. }), "{err:?}");
    }
}
