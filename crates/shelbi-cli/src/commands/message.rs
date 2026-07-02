//! `shelbi message <task-id> <kind> "<body>"` — the hub → workspace push
//! channel.
//!
//! This is the file-based, robust half of orchestrator↔workspace
//! communication: it appends one JSON message per line to an append-only log
//! in the assigned workspace's worktree at
//! `<worktree>/.shelbi/messages/<task-id>.log`. The workspace tails that file
//! (Phases 7/8) and acks by `msg_id` (Phase 9). The log persists in the
//! worktree, so it survives workspace pane restarts.
//!
//! This is deliberately *not* `shelbi send`. `send` injects keystrokes into a
//! tmux pane (send-keys-style UI injection) and inherits all the fragility of
//! racing the agent's own terminal I/O. `message` writes a file: nothing the
//! agent's UI does can clobber it, and concurrent writers don't interleave
//! (POSIX `O_APPEND` makes single writes ≤ PIPE_BUF atomic).

use anyhow::{anyhow, bail, Result};
use chrono::{SecondsFormat, Utc};
use clap::ValueEnum;
use serde::Serialize;
use shelbi_core::{Column, Host};

use super::require_project;

/// Recognized message kinds. Extensible — add a variant here and it's
/// accepted on the wire and validated by clap automatically. clap rejects
/// anything outside this set at parse time with a helpful error, which
/// satisfies "unknown kinds rejected".
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum MessageKind {
    /// Response to a workspace's `request-clarification`. Pair with
    /// `--in-response-to <question-id>`.
    Reply,
    /// Course correction — "stop what you're doing, the spec changed".
    Directive,
    /// Additional background info the workspace should fold in.
    Context,
}

impl MessageKind {
    fn as_str(self) -> &'static str {
        match self {
            MessageKind::Reply => "reply",
            MessageKind::Directive => "directive",
            MessageKind::Context => "context",
        }
    }
}

/// One line of the per-task message log. Serializes to a single-line JSON
/// object; field order here is the on-disk field order.
#[derive(Debug, Serialize)]
struct Message<'a> {
    msg_id: &'a str,
    ts: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_response_to: Option<&'a str>,
    body: &'a str,
}

pub fn run(
    project_opt: Option<String>,
    id: String,
    kind: MessageKind,
    body: String,
    in_response_to: Option<String>,
) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let tf = shelbi_state::load_task(&project_name, &id).map_err(|e| anyhow!(e))?;

    // Resolve the assigned workspace → its worktree + host. The worktree is a
    // per-workspace, per-machine path; without an assignment there is no
    // worktree to push into.
    let workspace_name = tf.task.assigned_to.as_deref().ok_or_else(|| {
        anyhow!(
            "task `{id}` is unassigned — assign it to a workspace first \
             (`shelbi task assign {id} --to <workspace>`) so its worktree can be resolved"
        )
    })?;
    let workspace = project.workspace(workspace_name).ok_or_else(|| {
        anyhow!(
            "workspace `{workspace_name}` (assigned to `{id}`) is no longer declared in the project"
        )
    })?;
    let machine = project
        .machine(&workspace.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", workspace.machine))?;
    let host = machine.host();
    let worktree = shelbi_orchestrator::workspace::workspace_worktree(machine, workspace);

    // A `done` task still has a worktree (the workspace keeps it across tasks),
    // so a push is harmless and useful for archival/replay — just warn so the
    // operator knows the workspace has likely moved on.
    if tf.task.column == Column::Done {
        eprintln!("warning: task `{id}` is in `done` — pushing message anyway");
    }

    // Worktree must actually exist. A missing worktree is a hard error, never
    // a silent no-op — otherwise the message would vanish and the operator
    // would think it landed.
    let worktree_str = worktree.to_string_lossy().into_owned();
    if !dir_exists(&host, &worktree_str)? {
        bail!(
            "worktree for task `{id}` does not exist at {worktree_str} \
             (workspace `{workspace_name}` may not have been started yet)"
        );
    }

    // Fresh, opaque, per-task-unique msg_id. Each `shelbi message` is its own
    // process, so the pid disambiguates two invocations that land in the same
    // millisecond; a single process only ever emits one id.
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let msg_id = format!("m-{}-{}", Utc::now().timestamp_millis(), std::process::id());

    let message = Message {
        msg_id: &msg_id,
        ts: &ts,
        kind: kind.as_str(),
        in_response_to: in_response_to.as_deref(),
        body: &body,
    };
    // Single line, no embedded newlines: serde_json::to_string never emits
    // raw newlines and escapes any in `body`, so the whole record is one line
    // — a precondition for O_APPEND line atomicity.
    let mut line = serde_json::to_string(&message)?;
    line.push('\n');

    let messages_dir = worktree.join(".shelbi").join("messages");
    let log_path = messages_dir.join(format!("{id}.log"));
    append_line(&host, &messages_dir, &log_path, &line)?;

    // Audit the push on the shared events stream.
    shelbi_state::append_message_event(&msg_id, &id).map_err(|e| anyhow!(e))?;

    // Best-effort: tell the hub daemon so it can arm the
    // unacked-message timer. The file append above is the durable record;
    // the daemon-side timer is only the safety net that turns into an
    // `ack=timeout` event if the worker never confirms. A down or
    // missing daemon silently skips this — `events.log` still has the
    // `push=ok` line and an operator watching the stream will notice
    // the missing ack themselves.
    notify_daemon_message_pushed(&project_name, &id, &msg_id);

    // Verify a worker tail is actually running for this task: the
    // SessionStart hook writes its pid to `<msgs>/<id>.tail.d/pid` and
    // clears the dir on exit. If it's missing after our write, the
    // message is durable but nobody is reading it — surface that loudly
    // with a non-zero exit so callers (orchestrator scripts, humans)
    // don't silently trust an undelivered push. The file itself has
    // already been written, so a follow-up SessionStart will still find
    // and drain it on the next worker restart.
    if !tail_pid_alive(&host, &messages_dir, &id)? {
        bail!(
            "message written to {} but worker tail is not running for task `{id}` \
             (no live pid at .shelbi/messages/{id}.tail.d/pid) — \
             the record is durable and will be picked up when the worker's \
             SessionStart hook next fires, but nothing is reading right now",
            log_path.display(),
        );
    }

    println!("✓ {msg_id} → {id} ({})", kind.as_str());
    Ok(())
}

/// Check whether the SessionStart hook's `tail -f` pid file exists and
/// names a live process on `host`. `.tail.d/pid` is the durable
/// beacon the hook drops when it starts and clears when the pane exits
/// (see `crates/shelbi-cli/src/commands/open/pane.rs::kill_task_tail`).
/// Absence means no live worker tailing the log — the caller treats that
/// as a delivery failure so the message doesn't silently vanish.
fn tail_pid_alive(host: &Host, messages_dir: &std::path::Path, task_id: &str) -> Result<bool> {
    let pid_path = messages_dir
        .join(format!("{task_id}.tail.d"))
        .join("pid");
    match host {
        Host::Local => {
            let pid_text = match std::fs::read_to_string(&pid_path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(anyhow!("reading {}: {e}", pid_path.display())),
            };
            let pid: libc::pid_t = match pid_text.trim().parse() {
                Ok(p) => p,
                Err(_) => return Ok(false),
            };
            // `kill(pid, 0)` is the standard "is this pid alive?" probe on
            // POSIX. Returns 0 when the process exists; ESRCH otherwise.
            // SAFETY: no memory dereference, just a syscall.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            Ok(alive)
        }
        Host::Ssh { .. } => {
            // Remote: `test -f pid && kill -0 $(cat pid)` collapses both
            // presence and liveness into one probe. Any failure (missing
            // file, dead pid, unreadable pid text) => absent.
            let script = format!(
                "test -f '{p}' && kill -0 \"$(cat '{p}')\" 2>/dev/null",
                p = pid_path.to_string_lossy(),
            );
            let out = shelbi_ssh::run(host, ["sh", "-c", &script]).map_err(|e| anyhow!(e))?;
            Ok(out.status.success())
        }
    }
}

/// Send a `message-pushed` verb to the hub daemon over the Unix socket.
/// Mirrors the worker → hub one-liner pattern (single newline-terminated
/// JSON, write-only, half-close) so the daemon handler treats it like
/// every other inbound message. Best-effort: any error is swallowed —
/// the push is durable on disk regardless of whether the timer arms.
fn notify_daemon_message_pushed(project: &str, task_id: &str, msg_id: &str) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock = match shelbi_state::hub_socket_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    let Ok(mut stream) = UnixStream::connect(&sock) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let payload = serde_json::json!({
        "verb": "message-pushed",
        "project": project,
        "task_id": task_id,
        "msg_id": msg_id,
    });
    let Ok(mut bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    bytes.push(b'\n');
    let _ = stream.write_all(&bytes);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // Wait (briefly) for the daemon's ack so the connection isn't torn
    // down while the daemon is still dispatching. Still best-effort: a
    // missing ack just means the timeout timer may not be armed, and the
    // push itself is already durable on disk.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut ack = [0u8; shelbi_state::DAEMON_ACK.len()];
    let _ = stream.read(&mut ack);
}

/// Is `path` a directory on `host`? `test -d` is a real binary on both Linux
/// and macOS, so the same probe works locally and over SSH.
fn dir_exists(host: &Host, path: &str) -> Result<bool> {
    let out = shelbi_ssh::run(host, ["test", "-d", path]).map_err(|e| anyhow!(e))?;
    Ok(out.status.success())
}

/// Append one already-newline-terminated `line` to `log_path` on `host`,
/// creating `dir` first.
///
/// Local: open with `O_APPEND` and write the whole line in a single
/// `write_all`. Remote: `mkdir -p && cat >>` over SSH, with the payload fed
/// through stdin (not argv) so the body survives the SSH wire and the remote
/// shell verbatim. Both rely on POSIX `O_APPEND` for atomic, non-interleaved
/// line writes ≤ PIPE_BUF.
fn append_line(
    host: &Host,
    dir: &std::path::Path,
    log_path: &std::path::Path,
    line: &str,
) -> Result<()> {
    match host {
        Host::Local => {
            use std::io::Write;
            std::fs::create_dir_all(dir).map_err(|e| anyhow!("creating {}: {e}", dir.display()))?;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
                .map_err(|e| anyhow!("opening {}: {e}", log_path.display()))?;
            f.write_all(line.as_bytes())
                .map_err(|e| anyhow!("appending to {}: {e}", log_path.display()))?;
            Ok(())
        }
        Host::Ssh { .. } => {
            // `cat >>` opens the file with O_APPEND on the remote; the single
            // small write keeps the line atomic. Single-quote the paths for
            // the remote shell (worktree paths are shelbi-derived and contain
            // no single quotes).
            let script = format!(
                "mkdir -p '{}' && cat >> '{}'",
                dir.to_string_lossy(),
                log_path.to_string_lossy()
            );
            shelbi_ssh::run_with_stdin(host, ["sh", "-c", &script], line.as_bytes())
                .map_err(|e| anyhow!(e))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serializes_single_line_with_fields_in_order() {
        let m = Message {
            msg_id: "m-1",
            ts: "2026-06-30T01:55:00Z",
            kind: "reply",
            in_response_to: Some("q-001"),
            body: "hello",
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(
            s,
            r#"{"msg_id":"m-1","ts":"2026-06-30T01:55:00Z","kind":"reply","in_response_to":"q-001","body":"hello"}"#
        );
        assert!(!s.contains('\n'));
    }

    #[test]
    fn in_response_to_omitted_when_absent() {
        let m = Message {
            msg_id: "m-2",
            ts: "2026-06-30T02:10:00Z",
            kind: "directive",
            in_response_to: None,
            body: "stop",
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("in_response_to"));
        assert_eq!(
            s,
            r#"{"msg_id":"m-2","ts":"2026-06-30T02:10:00Z","kind":"directive","body":"stop"}"#
        );
    }

    #[test]
    fn body_with_newlines_and_quotes_stays_one_line() {
        let m = Message {
            msg_id: "m-3",
            ts: "2026-06-30T02:30:00Z",
            kind: "context",
            in_response_to: None,
            body: "line one\nline \"two\"",
        };
        let s = serde_json::to_string(&m).unwrap();
        // The raw newline is escaped, so the on-disk record is a single line.
        assert!(!s.contains('\n'));
        assert!(s.contains(r#"line one\nline \"two\""#));
    }

    #[test]
    fn kind_value_enum_accepts_known_and_rejects_unknown() {
        assert_eq!(
            MessageKind::from_str("reply", true).unwrap(),
            MessageKind::Reply
        );
        assert_eq!(
            MessageKind::from_str("directive", true).unwrap(),
            MessageKind::Directive
        );
        assert_eq!(
            MessageKind::from_str("context", true).unwrap(),
            MessageKind::Context
        );
        assert!(MessageKind::from_str("bogus", true).is_err());
    }

    #[test]
    fn append_line_local_appends_without_interleaving() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-msg-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir = tmp.join(".shelbi").join("messages");
        let log = dir.join("t.log");
        append_line(&Host::Local, &dir, &log, "{\"a\":1}\n").unwrap();
        append_line(&Host::Local, &dir, &log, "{\"b\":2}\n").unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        assert_eq!(body, "{\"a\":1}\n{\"b\":2}\n");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The tail-liveness probe returns false when no pid file exists —
    /// that's the "worker's SessionStart hook never ran" case. `shelbi
    /// message` uses this signal to fail loudly instead of silently
    /// writing to a log nobody is reading.
    #[test]
    fn tail_pid_alive_false_when_no_pid_file() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-tail-probe-none-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(!tail_pid_alive(&Host::Local, &tmp, "feat-x").unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The tail-liveness probe returns false when the pid file names a
    /// process that no longer exists — worker crashed or was killed
    /// after the previous pane exit didn't clean up. Uses a
    /// short-lived child so we get a definitely-dead pid without racing
    /// the OS reaper.
    #[test]
    fn tail_pid_alive_false_when_recorded_pid_is_dead() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-tail-probe-dead-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let lock_dir = tmp.join("feat-x.tail.d");
        std::fs::create_dir_all(&lock_dir).unwrap();

        // Spawn a short-lived child, wait for it, then reuse its pid.
        // On POSIX the pid may get recycled — probability is negligible
        // in a test that lasts milliseconds and doesn't fork thousands
        // of processes. `sh -c :` is portable across macOS (no /bin/true)
        // and Linux.
        let child = std::process::Command::new("sh").arg("-c").arg(":").spawn().unwrap();
        let pid = child.id();
        let _ = child.wait_with_output();
        std::fs::write(lock_dir.join("pid"), pid.to_string()).unwrap();

        assert!(!tail_pid_alive(&Host::Local, &tmp, "feat-x").unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The tail-liveness probe returns true when the pid file names a
    /// running process — that's the healthy "worker is tailing" state.
    #[test]
    fn tail_pid_alive_true_for_running_pid() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-tail-probe-live-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let lock_dir = tmp.join("feat-x.tail.d");
        std::fs::create_dir_all(&lock_dir).unwrap();

        // Sleep child stands in for the SessionStart hook's `tail -f`.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(lock_dir.join("pid"), child.id().to_string()).unwrap();

        assert!(tail_pid_alive(&Host::Local, &tmp, "feat-x").unwrap());

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
