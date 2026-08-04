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
//!
//! Consequently, `message` is intentionally outside the pane verified-submit
//! primitive. It sends neither text nor Enter to tmux. The workspace's runner
//! hooks consume the durable file record and acknowledge its `msg_id`; routing
//! the same body through pane injection would duplicate delivery and weaken the
//! restart-safe contract of this channel.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use chrono::{SecondsFormat, Utc};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use shelbi_core::{Column, Host};
use shelbi_state::MessageDelivery;

use super::require_project;

/// Subcommands of `shelbi message` that are *not* a push. Today just
/// `status`, which reads a pushed message's delivery outcome back off the
/// events stream so a non-interactive caller can tell "the worker read it"
/// from "the file was written and nobody has looked".
#[derive(Debug, Subcommand)]
pub enum MessageStatusCmd {
    /// Report a pushed message's delivery status (`queued` / `delivered` /
    /// `unconfirmed`) by scanning `events.log` for its `ack=` line. Exits 0
    /// only when the worker has confirmed delivery.
    Status {
        /// The `msg-id` printed by `shelbi message` (e.g.
        /// `m-1785764991921-86377`).
        msg_id: String,
    },
}

/// How long the `--wait` poll loop sleeps between `events.log` reads. Short
/// enough that a `--wait` returns within ~1 poll of the ack landing; long
/// enough not to spin on the log file.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(500);

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

/// Entry point for `shelbi message`. Dispatches the `status` query subcommand
/// or the positional push form. The push form's `id`/`kind`/`body` are optional
/// at the clap layer only so `shelbi message status <msg-id>` parses without
/// tripping the positional requirements; a bare push missing any of them is a
/// hard error here with a usage hint.
pub fn run(
    project_opt: Option<String>,
    status: Option<MessageStatusCmd>,
    id: Option<String>,
    kind: Option<MessageKind>,
    body: Option<String>,
    in_response_to: Option<String>,
    wait: Option<u64>,
) -> Result<()> {
    if let Some(MessageStatusCmd::Status { msg_id }) = status {
        return run_status(&msg_id);
    }
    let id = id.ok_or_else(|| {
        anyhow!(
            "missing <ID>: usage `shelbi message <task-id> <kind> <body>` \
             (or `shelbi message status <msg-id>` to query delivery)"
        )
    })?;
    let kind = kind.ok_or_else(|| {
        anyhow!("missing <KIND> for `shelbi message {id} <kind> <body>` (one of: reply, directive, context)")
    })?;
    let body = body.ok_or_else(|| {
        anyhow!(
            "missing <BODY> for `shelbi message {id} {} <body>`",
            kind.as_str()
        )
    })?;
    run_send(project_opt, id, kind, body, in_response_to, wait)
}

/// Query and report a pushed message's delivery outcome from the events
/// stream. Exit 0 only when the worker has confirmed delivery (`ack=worker`);
/// every other state is a non-zero exit so a caller can gate on
/// `shelbi message status <id>` in a script.
fn run_status(msg_id: &str) -> Result<()> {
    match shelbi_state::message_delivery_status(msg_id).map_err(|e| anyhow!(e))? {
        MessageDelivery::Delivered => {
            println!("delivered {msg_id} — worker acked (read into its context)");
            Ok(())
        }
        MessageDelivery::TimedOut => bail!(
            "unconfirmed {msg_id} — the ack window elapsed with no worker confirmation. \
             The message is durable and may still be read when the worker's turn ends; \
             re-check with `shelbi message status {msg_id}`."
        ),
        MessageDelivery::Queued => bail!(
            "queued {msg_id} — durable on disk, no worker ack yet. \
             Re-check with `shelbi message status {msg_id}`."
        ),
        MessageDelivery::Unknown => bail!(
            "unknown message id `{msg_id}` — no `push=ok` for it in the events log \
             (never pushed, or the log has rotated the record out)"
        ),
    }
}

/// Push a message onto a task's file-based message log, then report an honest
/// delivery state: `queued` (durable, awaiting the worker's ack),
/// `undeliverable` (no live reader will ever pick it up), or — with `--wait` —
/// `delivered`/timed-out once the events stream resolves it.
fn run_send(
    project_opt: Option<String>,
    id: String,
    kind: MessageKind,
    body: String,
    in_response_to: Option<String>,
    wait: Option<u64>,
) -> Result<()> {
    let project_name = require_project(project_opt)?;
    // Version gate: the push writes the message log and arms the daemon's
    // ack timer — a stale daemon mishandles both.
    super::hub_version::ensure_daemon_matches_for_mutation()?;
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
    // so the append below is harmless and useful for archival/replay. But the
    // workspace's next session is a *different* task, so nothing will ever
    // drain this task's log — the push is undeliverable. We record it anyway
    // (archival) and report that truth after the write (see below), rather than
    // claiming a future SessionStart will pick it up.
    let is_done = tf.task.column == Column::done();

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

    // Undeliverable #1 — the task is `done`. The record above is durable
    // (archival/replay), but the assigned workspace has moved on to a
    // different task and will never tail *this* task's log again. Report that
    // honestly with a non-zero exit instead of implying a future pickup.
    if is_done {
        bail!(
            "message {msg_id} written to {} but task `{id}` is in `done` — UNDELIVERABLE: \
             the workspace has moved on to a different task and will not read this task's \
             log. The record is kept for archival/replay only.",
            log_path.display(),
        );
    }

    // Undeliverable #2 — no live reader right now. The SessionStart hook
    // writes its `tail -f` pid to `<msgs>/<id>.tail.d/pid` and clears the dir
    // on exit; its absence means nothing is draining the log. The record is
    // durable and *this* task's next session (a resume) would drain it, but as
    // of now it is not delivered — surface that with a non-zero exit so no
    // caller trusts an undelivered push.
    if !tail_pid_alive(&host, &messages_dir, &id)? {
        bail!(
            "message {msg_id} queued to {} but no worker is reading task `{id}` right now \
             (no live tail pid at .shelbi/messages/{id}.tail.d/pid). It is durable and would \
             be drained if this task's worker session restarts, but it is NOT delivered.",
            log_path.display(),
        );
    }

    // A live tail is consuming the log, so the message is genuinely queued.
    // That is still not "delivered": the worker only drains + acks at its next
    // turn boundary (its `Stop` hook), which for a busy worker can be well
    // past the daemon's fixed ack window. Confirmation is therefore async.
    if let Some(secs) = wait {
        return wait_for_delivery(&msg_id, &id, kind, secs);
    }

    // No `✓`: the push succeeded but delivery is unconfirmed. Say exactly that
    // so the success signal can't be misread as "the worker read it".
    println!(
        "queued {msg_id} → {id} ({}) — durable and being tailed by the worker, but NOT yet \
         confirmed delivered. Delivery lands when the worker next ends a turn; confirm with \
         `shelbi message status {msg_id}` or re-send with `--wait`.",
        kind.as_str(),
    );
    Ok(())
}

/// Block until the worker confirms delivery (`ack=worker` on the events
/// stream) or `secs` elapses. Polls the durable events log rather than the
/// in-memory daemon map so it works across a daemon restart and needs no live
/// socket. A `TimedOut` seen mid-wait (the daemon's shorter 60s reaper fired
/// first) is not terminal — a late worker ack still upgrades it, so we keep
/// polling until *our* deadline. Exits non-zero if the window elapses without
/// an `ack=worker`, so a non-interactive caller can distinguish
/// delivered from queued-but-never-read.
fn wait_for_delivery(msg_id: &str, id: &str, kind: MessageKind, secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match shelbi_state::message_delivery_status(msg_id).map_err(|e| anyhow!(e))? {
            MessageDelivery::Delivered => {
                println!(
                    "delivered {msg_id} → {id} ({}) — worker acked (read into its context)",
                    kind.as_str()
                );
                return Ok(());
            }
            MessageDelivery::TimedOut | MessageDelivery::Queued | MessageDelivery::Unknown => {
                if Instant::now() >= deadline {
                    bail!(
                        "message {msg_id} → {id} ({}) was queued but NOT confirmed delivered \
                         within {secs}s (no `ack=worker`). The worker may simply not have ended \
                         a turn yet; the record stays durable — re-check with \
                         `shelbi message status {msg_id}`.",
                        kind.as_str(),
                    );
                }
                std::thread::sleep(WAIT_POLL_INTERVAL);
            }
        }
    }
}

/// Check whether the SessionStart hook's `tail -f` pid file exists and
/// names a live process on `host`. `.tail.d/pid` is the durable
/// beacon the hook drops when it starts and clears when the pane exits
/// (see `crates/shelbi-cli/src/commands/open/pane.rs::kill_task_tail`).
/// Absence means no live worker tailing the log — the caller treats that
/// as a delivery failure so the message doesn't silently vanish.
fn tail_pid_alive(host: &Host, messages_dir: &std::path::Path, task_id: &str) -> Result<bool> {
    let pid_path = messages_dir.join(format!("{task_id}.tail.d")).join("pid");
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
    use std::io::Write;
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
    // push itself is already durable on disk. The shared reader also skips
    // the briefly-shipped server-first hello for rolling compatibility.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = shelbi_state::read_daemon_ack(&stream);
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

    /// The push form requires id/kind/body; they are `Option` only so the
    /// clap layer can accept `message status <id>`. A bare push missing an id
    /// is a usage error here, not a panic or a silent no-op.
    #[test]
    fn run_without_id_or_status_is_a_usage_error() {
        let err = run(None, None, None, None, None, None, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing <ID>"), "got: {msg}");
        assert!(msg.contains("status"), "usage hint should mention status: {msg}");
    }

    /// `message status` reports the real, durable delivery outcome and exits
    /// non-zero unless the worker actually acked — the whole point of the
    /// feature. This guards the success signal from regressing to "the file was
    /// written": a queued-but-unacked message must NOT read as success.
    #[test]
    fn run_status_reflects_delivery_and_gates_exit_code() {
        let _g = crate::commands::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-msg-status-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // Unknown id → error (never pushed).
        assert!(run_status("m-nope").is_err());

        // Queued (push, no ack) → non-zero exit, and the wording is honest.
        shelbi_state::append_message_event("m-q", "task-a").unwrap();
        let queued = run_status("m-q").unwrap_err().to_string();
        assert!(queued.contains("queued"), "got: {queued}");

        // Timed out (daemon reaper) → still non-zero, distinct wording.
        shelbi_state::append_message_event("m-t", "task-a").unwrap();
        shelbi_state::append_message_ack_event("m-t", "task-a", "timeout").unwrap();
        let timed = run_status("m-t").unwrap_err().to_string();
        assert!(timed.contains("unconfirmed"), "got: {timed}");

        // Worker acked → success (exit 0). This is the ONLY delivered state.
        shelbi_state::append_message_event("m-ok", "task-a").unwrap();
        shelbi_state::append_message_ack_event("m-ok", "task-a", "worker").unwrap();
        assert!(run_status("m-ok").is_ok());

        std::env::remove_var("SHELBI_HOME");
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
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(":")
            .spawn()
            .unwrap();
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
