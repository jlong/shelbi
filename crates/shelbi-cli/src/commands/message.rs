//! `shelbi message <task-id> <kind> "<body>"` — the hub → workspace push
//! channel.
//!
//! This is the file-based, robust half of orchestrator↔workspace
//! communication: it appends one JSON message per line to an append-only log
//! in the assigned workspace's worktree at
//! `<worktree>/.shelbi/messages/<task-id>.log`. The worker reads that file from
//! a persisted cursor at the end of each turn and acks by `msg_id`. The log and
//! the cursor persist in the worktree, so an unread message survives a pane
//! restart.
//!
//! A message nothing would read is refused rather than queued: a done task or a
//! dead session gets `ack=undeliverable` on the events stream and no other side
//! effect, so the log only ever holds messages that had a reader.
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
use shelbi_state::{MessageDelivery, UndeliverableReason};

use super::require_project;

/// Subcommands of `shelbi message` that are *not* a push. Today just
/// `status`, which reads a pushed message's delivery outcome back off the
/// events stream so a non-interactive caller can tell "the worker read it"
/// from "the file was written and nobody has looked".
#[derive(Debug, Subcommand)]
pub enum MessageStatusCmd {
    /// Report a pushed message's delivery status (`queued` / `delivered` /
    /// `unconfirmed` / `undeliverable`) by scanning `events.log` for its `ack=` line. Exits 0
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
             The message is durable and may still be read when the worker next ends a turn, \
             including after a resume; re-check with `shelbi message status {msg_id}`."
        ),
        MessageDelivery::Undeliverable(reason) => bail!(
            "undeliverable {msg_id} ({}) — {} It was never written to the task's log, so \
             send it again if it still applies.",
            reason.as_str(),
            undeliverable_why(reason),
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

/// Why a message with this verdict will never be read, for the error a caller
/// sees both when sending and from `message status`.
fn undeliverable_why(reason: UndeliverableReason) -> &'static str {
    match reason {
        UndeliverableReason::TaskDone => {
            "the task is done, so no worker session will read its messages."
        }
        UndeliverableReason::NoSession => "no worker session was running for the task.",
    }
}

/// Record the refusal as the message's one and only side effect, then fail the
/// send. The record is what lets `message status` answer later, so a failure to
/// write it is reported rather than swallowed.
fn refuse(msg_id: &str, id: &str, reason: UndeliverableReason) -> Result<()> {
    shelbi_state::append_message_undeliverable_event(msg_id, id, reason)
        .map_err(|e| anyhow!(e))?;
    bail!(
        "message {msg_id} → {id} is UNDELIVERABLE ({}): {} It was not written to the task's \
         log, so send it again if it still applies.",
        reason.as_str(),
        undeliverable_why(reason),
    )
}

/// Push a message onto a task's file-based message log, then report an honest
/// delivery state: `queued` (durable, awaiting the worker's ack) or, with
/// `--wait`, `delivered`/timed-out once the events stream resolves it.
///
/// Deliverability is decided first. A message no worker would read is refused
/// with `ack=undeliverable` before anything else happens: it isn't appended, no
/// ack timer is armed, and the question it answers stays open.
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
    let store = shelbi_state::resolve_issue_store(&project_name, &project.issue_tracker)
        .map_err(|e| anyhow!(e))?;
    let tf = store
        .get(&id)
        .map_err(|e| anyhow!(e))?
        .ok_or_else(|| anyhow!("issue `{id}` not found"))?;

    // Fresh, opaque, per-task-unique msg_id. Each `shelbi message` is its own
    // process, so the pid disambiguates two invocations that land in the same
    // millisecond; a single process only ever emits one id.
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let msg_id = format!("m-{}-{}", Utc::now().timestamp_millis(), std::process::id());

    // A `done` task's workspace has moved on (its worktree may even be gone),
    // so nothing will read this task's log again.
    if tf.task.column == Column::done() {
        return refuse(&msg_id, &id, UndeliverableReason::TaskDone);
    }

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

    let messages_dir = worktree.join(".shelbi").join("messages");
    let session_live = match probe_session_beacon(&host, &messages_dir, &id)? {
        SessionBeacon::Alive => true,
        SessionBeacon::Dead => false,
        // A polling runner has no hook to vouch for it. It reads the log itself
        // while its pane runs, so the pane is the session.
        SessionBeacon::Polling => {
            let target = shelbi_orchestrator::workspace::workspace_target(&project, workspace)
                .map_err(|e| anyhow!(e))?;
            shelbi_orchestrator::workspace::workspace_pane_alive(&host, &target)
                .map_err(|e| anyhow!(e))?
        }
    };
    if !session_live {
        return refuse(&msg_id, &id, UndeliverableReason::NoSession);
    }

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

    let log_path = messages_dir.join(format!("{id}.log"));
    append_line(&host, &messages_dir, &log_path, &line)?;

    // Audit the push on the shared events stream.
    shelbi_state::append_message_event(&msg_id, &id).map_err(|e| anyhow!(e))?;

    // A live session has the reply queued, and the cursor carries it across a
    // restart, so the question it answers is answered now. Stamping the stored
    // question is what drops it off `shelbi question list`; a failure here
    // never fails the push that already landed.
    if let Some(question_id) = in_response_to.as_deref() {
        match shelbi_state::questions::mark_question_answered(
            Some(&project_name),
            &id,
            question_id,
            &msg_id,
        ) {
            Ok(Some(_)) => {}
            Ok(None) => eprintln!(
                "note: no stored question `{question_id}` for task `{id}` — the reply was \
                 sent, but there is no question file to mark answered"
            ),
            Err(e) => eprintln!("warning: could not mark question `{question_id}` answered: {e}"),
        }
    }

    // Best-effort: tell the hub daemon so it can arm the
    // unacked-message timer. The file append above is the durable record;
    // the daemon-side timer is only the safety net that turns into an
    // `ack=timeout` event if the worker never confirms. A down or
    // missing daemon silently skips this — `events.log` still has the
    // `push=ok` line and an operator watching the stream will notice
    // the missing ack themselves.
    notify_daemon_message_pushed(&project_name, &id, &msg_id);

    // Queued is still not "delivered": the worker only reads + acks at its next
    // turn boundary (its `Stop` hook), which for a busy worker can be well past
    // the daemon's fixed ack window. Confirmation is therefore async.
    if let Some(secs) = wait {
        return wait_for_delivery(&msg_id, &id, kind, secs);
    }

    // No `✓`: the push succeeded but delivery is unconfirmed. Say exactly that
    // so the success signal can't be misread as "the worker read it".
    println!(
        "queued {msg_id} → {id} ({}) — durable, and a worker session is running, but NOT yet \
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
            MessageDelivery::Undeliverable(reason) => {
                bail!("message {msg_id} → {id} is undeliverable ({})", reason.as_str())
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

/// What a task's `<task>.session` beacon says about its worker session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionBeacon {
    /// The session that wrote it is still running.
    Alive,
    /// No beacon, or the session that wrote it is gone.
    Dead,
    /// Dispatch launched a polling runner, which has no hooks to write one.
    Polling,
}

/// Reads the beacon the SessionStart hook writes (see `MESSAGE_SESSION_START_SH`
/// in `shelbi_orchestrator::workspace`), with the same fixed locale and zone, so
/// a pid only counts while it's the process that started at the recorded time.
/// `$1` is the messages dir, `$2` the task id.
///
/// The `tail.d` branch covers sessions that started under the tail relay before
/// this binary was installed: they have no beacon but are alive, and keep their
/// old hooks until their next dispatch. It can go once none can be left running.
const SESSION_BEACON_PROBE_SH: &str = r#"BEACON=$1/$2.session
if [ -f "$BEACON" ]; then
  read -r PID STARTED < "$BEACON"
  case "$PID" in
    polling) echo polling; exit 0 ;;
    unverified) echo alive; exit 0 ;;
    ''|*[!0-9]*|0|1) echo dead; exit 0 ;;
  esac
  NOW=$(echo $(LC_ALL=C TZ=UTC ps -o lstart= -p "$PID" 2>/dev/null))
  if [ -n "$NOW" ] && [ "$NOW" = "$STARTED" ]; then echo alive; else echo dead; fi
  exit 0
fi
PID=$(cat "$1/$2.tail.d/pid" 2>/dev/null)
case "$PID" in ''|*[!0-9]*|0|1) echo dead; exit 0 ;; esac
case "$(ps -ww -o command= -p "$PID" 2>/dev/null)" in
  *tail*"$2.log") echo alive ;;
  *) echo dead ;;
esac
"#;

/// Run [`SESSION_BEACON_PROBE_SH`] on `host`. An answer the script didn't give
/// (SSH down, say) is an error, not `Dead`: nothing is known about the session,
/// so nothing gets recorded against the message either.
fn probe_session_beacon(
    host: &Host,
    messages_dir: &std::path::Path,
    task_id: &str,
) -> Result<SessionBeacon> {
    let dir = messages_dir.to_string_lossy();
    let out = shelbi_ssh::run(
        host,
        ["sh", "-c", SESSION_BEACON_PROBE_SH, "sh", &dir, task_id],
    )
    .map_err(|e| anyhow!(e))?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "alive" => Ok(SessionBeacon::Alive),
        "dead" => Ok(SessionBeacon::Dead),
        "polling" => Ok(SessionBeacon::Polling),
        _ => bail!(
            "could not check for a worker session for task `{task_id}` ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
        ),
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
        let _g = crate::commands::test_support::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

        // Refused at send time → non-zero, named as such, and the reason survives.
        for (msg, reason, token) in [
            ("m-done", UndeliverableReason::TaskDone, "done"),
            ("m-dead", UndeliverableReason::NoSession, "no_session"),
        ] {
            shelbi_state::append_message_undeliverable_event(msg, "task-a", reason).unwrap();
            let refused = run_status(msg).unwrap_err().to_string();
            assert!(refused.contains("undeliverable"), "got: {refused}");
            assert!(refused.contains(token), "got: {refused}");
        }

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

    fn probe_dir(tag: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-beacon-probe-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    /// The beacon line the SessionStart hook writes for a running `pid`.
    fn beacon_for(pid: u32) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("echo {pid} $(LC_ALL=C TZ=UTC ps -o lstart= -p {pid})"))
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    /// No beacon is the "SessionStart hook never ran" case: `shelbi message`
    /// refuses instead of writing to a log nobody is reading.
    #[test]
    fn probe_reports_dead_without_a_beacon() {
        let tmp = probe_dir("none");
        assert_eq!(
            probe_session_beacon(&Host::Local, &tmp, "feat-x").unwrap(),
            SessionBeacon::Dead
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A running process that started when the beacon says it did is the
    /// session. A dead one isn't, and neither is a running process with a
    /// different start time: that's the OS reusing the pid, which `kill -0`
    /// alone would have called alive.
    #[test]
    fn probe_follows_the_session_not_the_pid() {
        let tmp = probe_dir("session");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let beacon = tmp.join("feat-x.session");
        let probe = || probe_session_beacon(&Host::Local, &tmp, "feat-x").unwrap();

        std::fs::write(&beacon, beacon_for(child.id())).unwrap();
        assert_eq!(probe(), SessionBeacon::Alive);

        std::fs::write(
            &beacon,
            format!("{} Mon Jan 1 00:00:00 2001\n", child.id()),
        )
        .unwrap();
        assert_eq!(probe(), SessionBeacon::Dead, "reused pid");

        std::fs::write(&beacon, beacon_for(child.id())).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(probe(), SessionBeacon::Dead, "session gone");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The tokens that aren't a pid: a polling runner's marker, a host with no
    /// `ps` to verify against, and a corrupt file.
    #[test]
    fn probe_reads_the_non_pid_beacons() {
        let tmp = probe_dir("tokens");
        let beacon = tmp.join("feat-x.session");
        let probe = || probe_session_beacon(&Host::Local, &tmp, "feat-x").unwrap();
        std::fs::write(&beacon, "polling\n").unwrap();
        assert_eq!(probe(), SessionBeacon::Polling);
        std::fs::write(&beacon, "unverified\n").unwrap();
        assert_eq!(probe(), SessionBeacon::Alive);
        std::fs::write(&beacon, "garbage here\n").unwrap();
        assert_eq!(probe(), SessionBeacon::Dead);
        std::fs::write(&beacon, "").unwrap();
        assert_eq!(probe(), SessionBeacon::Dead);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A session already running when this binary landed still has the old
    /// tail and no beacon. Its live tail counts; a stale lock doesn't, and
    /// neither does a pid the OS reused for something else.
    #[test]
    fn probe_honors_a_live_tail_from_before_the_cursor() {
        let tmp = probe_dir("legacy");
        let lock = tmp.join("feat-x.tail.d");
        std::fs::create_dir_all(&lock).unwrap();
        let log = tmp.join("feat-x.log");
        std::fs::write(&log, "").unwrap();
        let spawn = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let probe = || probe_session_beacon(&Host::Local, &tmp, "feat-x").unwrap();

        let mut tail = spawn(&["tail", "-f", "-c", "+1", log.to_str().unwrap()]);
        std::fs::write(lock.join("pid"), tail.id().to_string()).unwrap();
        assert_eq!(probe(), SessionBeacon::Alive);
        let _ = tail.kill();
        let _ = tail.wait();
        assert_eq!(probe(), SessionBeacon::Dead, "stale lock");

        let mut other = spawn(&["sleep", "30"]);
        std::fs::write(lock.join("pid"), other.id().to_string()).unwrap();
        assert_eq!(probe(), SessionBeacon::Dead, "reused pid");
        let _ = other.kill();
        let _ = other.wait();

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
