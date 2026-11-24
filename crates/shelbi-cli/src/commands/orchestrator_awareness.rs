//! Awareness (v2) delivery into an orchestrator through a plugin in its
//! runner: the `shelbi-awareness` system plugin for Claude
//! (`plugins/shelbi-awareness/`) and Shelbi's OpenCode plugin
//! (`opencode_plugin.js` in `shelbi-orchestrator`). Both drive the same
//! commands below; nothing here knows which runner is on the other end.
//!
//! Three consumers drain the project's durable delivery queue (the same
//! `event-cursor` the v1 feed acks):
//!
//! * `shelbi orchestrator events stream`, the plugin's **monitor**. The
//!   runner runs it for the whole session. Claude Code turns each stdout line
//!   into a notification; the OpenCode plugin reads `--json` batches and
//!   admits each into the session (an urgent one steered into a running
//!   turn, a digest stored without starting one). Either way the
//!   orchestrator never arms a Monitor itself.
//! * `shelbi orchestrator hook user-prompt-submit`: whatever the monitor
//!   hasn't delivered yet rides along with the user's message as context, so
//!   the orchestrator never answers from a stale board.
//! * `shelbi orchestrator hook stop`: an urgent event (reminder, finished job,
//!   overdue task, review ready) still queued when the orchestrator tries to
//!   stop blocks the stop once, with the events as the reason.
//!
//! Every consumer reads, hands over and acks one batch while holding the
//! project's delivery lock ([`shelbi_state::acquire_event_delivery_lock`]),
//! so each event reaches the orchestrator exactly once across all three.
//! Delivery is print-then-ack: a process killed between the two re-delivers
//! that batch, never drops it.
//!
//! `shelbi orchestrator hook session-start` injects the Zen policy summary
//! (the first line of `zenmode.md`) when Zen Mode is on.

use std::io::{IsTerminal, Read};

use clap::ValueEnum;
use serde_json::{json, Value};

use super::*;

/// How often the stream polls the queue. Matches the v1 feed.
const STREAM_POLL: Duration = Duration::from_millis(250);
/// How long a hook waits for the delivery lock before answering with
/// nothing. The stream holds it only for one read-print-ack, so this is
/// generous; it stays far below Claude Code's 30s prompt-hook timeout.
const HOOK_LOCK_WAIT: Duration = Duration::from_secs(5);
/// How long a SIGTERM'd stream waits for the registry to name the follower
/// that superseded it, before treating the signal as a real stop.
const SUPERSEDE_GRACE: Duration = Duration::from_secs(1);
/// At most this many event lines per delivery; the rest are summarized in
/// one closing line. A backlog from a long-closed project shouldn't flood the
/// conversation, and Claude Code moves hook context over 10,000 characters
/// to a file the model is not asked to read.
const MAX_LINES: usize = 40;
const MAX_CHARS: usize = 8_000;
/// One event line is capped at this many characters.
const MAX_LINE_CHARS: usize = 600;

/// The hook events a plugin wires to `shelbi orchestrator hook`: Claude
/// Code's own, and the OpenCode plugin's equivalents (a turn starting, a
/// turn ending, the plugin loading), which it calls with the same input and
/// reads the same JSON answer from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookEvent {
    /// `UserPromptSubmit`: attach undelivered events to the user's message.
    UserPromptSubmit,
    /// `Stop`: block the stop once when an urgent event is still queued.
    Stop,
    /// `SessionStart`: the Zen policy summary, when Zen Mode is on.
    SessionStart,
}

/// Which pending events a delivery takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// Everything pending (the stream and the prompt hook).
    All,
    /// Everything pending, but only when at least one event is urgent;
    /// otherwise nothing is taken and the stream delivers it (the stop hook).
    IfUrgent,
}

#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    /// The lock or the log was busy past the deadline; nothing was taken.
    Busy,
    /// Nothing for the orchestrator. Bytes it doesn't need (other projects,
    /// kinds it doesn't react to) may have been acked.
    Quiet,
    /// These lines were handed over and acked.
    Delivered(Vec<String>),
}

/// One delivery's rendered lines, with what a runner needs to decide how it
/// enters the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Batch {
    lines: Vec<String>,
    /// At least one event must not wait for the next turn ([`is_urgent`]).
    urgent: bool,
    /// Every event is the board digest, which never needs a turn of its own.
    digest: bool,
}

impl Batch {
    fn notice(lines: Vec<String>) -> Batch {
        Batch { lines, urgent: false, digest: false }
    }

    /// The `--json` form: one object per batch, on one line.
    fn to_json_line(&self) -> String {
        json!({ "lines": self.lines, "urgent": self.urgent, "digest": self.digest }).to_string()
    }
}

/// The compact label an event is delivered under, or `None` for a kind the
/// orchestrator doesn't react to (dispatch/send bookkeeping, messages it sent
/// itself, ...), which is acked without being shown.
fn label(event: &NormalizedEvent) -> Option<&'static str> {
    Some(match event.kind.as_str() {
        "task_transition" => "task",
        "workspace_transition" => "workspace",
        "reminder" => "reminder",
        "job_done" => "job done",
        "job_overdue" => "job overdue",
        "task_overdue" => "task overdue",
        "pane_death" => "pane death",
        "review_ready" => "review ready",
        "clarification" => "question",
        "zen_mode_transition" => "zen",
        // In v2 a heartbeat line is the board digest the poller writes after
        // a transition; a quiet board writes none.
        "heartbeat" => "digest",
        "supervision" => match event.metadata.get("supervision").map(String::as_str) {
            Some("gave-up" | "restart-failed" | "agent-exited") => "supervision",
            _ => return None,
        },
        // A send whose text never submitted needs a retry; the rest is
        // bookkeeping for the send the orchestrator just made.
        "send" if event.metadata.get("status").is_some_and(|s| s == "stuck") => "send stuck",
        _ => return None,
    })
}

/// Events that must not wait for the next turn: the Stop hook keeps the
/// orchestrator going for these.
fn is_urgent(event: &NormalizedEvent) -> bool {
    matches!(
        event.kind.as_str(),
        "reminder" | "job_done" | "job_overdue" | "task_overdue" | "review_ready"
    )
}

/// One compact line for `event`: `shelbi <label>: <body>`, where the body is
/// the log line without its timestamp and `project=` scope.
fn render(event: &NormalizedEvent) -> Option<String> {
    let label = label(event)?;
    let body = shelbi_state::split_event_words(&event.raw)
        .into_iter()
        .skip(1)
        .filter(|word| !is_redundant_word(word))
        .collect::<Vec<_>>()
        .join(" ");
    let mut line = format!("shelbi {label}: {body}");
    if let Some(question) = &event.question {
        let text = question.text.split_whitespace().collect::<Vec<_>>().join(" ");
        line.push_str(&format!(" — asks: {text}"));
    }
    Some(truncate_chars(&line, MAX_LINE_CHARS))
}

/// Words the label already says (the bare verb, the project scope) or that
/// only restate the column ids next to them.
fn is_redundant_word(word: &str) -> bool {
    const PREFIXES: [&str; 3] = ["project=", "from_category=", "to_category="];
    const VERBS: [&str; 6] = ["reminder", "job", "overdue", "heartbeat", "review-ready", "send"];
    PREFIXES.iter().any(|p| word.starts_with(p)) || VERBS.contains(&word)
}

fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// Cap a delivery at [`MAX_LINES`] lines and [`MAX_CHARS`] characters,
/// summarizing what was cut in a closing line.
fn cap(lines: Vec<String>) -> Vec<String> {
    let total = lines.len();
    let mut out = Vec::new();
    let mut chars = 0;
    for line in lines {
        if out.len() + 1 >= MAX_LINES && total > MAX_LINES
            || chars + line.len() > MAX_CHARS
        {
            break;
        }
        chars += line.len() + 1;
        out.push(line);
    }
    let cut = total - out.len();
    if cut > 0 {
        out.push(format!(
            "shelbi: {cut} more board event(s) not shown; run `shelbi task list` and \
             `shelbi workspace list` for the current board"
        ));
    }
    out
}

/// Read, hand over and ack the pending batch under the delivery lock.
///
/// `emit` receives the rendered batch while the lock is held; the cursor is
/// advanced only after it returns `Ok`, so a failed hand-over (a closed pipe)
/// leaves the batch queued.
fn deliver(
    project: &str,
    deadline: Option<Instant>,
    cancelled: &dyn Fn() -> bool,
    policy: Policy,
    emit: &mut dyn FnMut(&Batch) -> Result<()>,
) -> Result<Delivery> {
    let Some(_lock) = shelbi_state::acquire_event_delivery_lock(project, deadline, cancelled)
        .map_err(|e| anyhow!(e))?
    else {
        return Ok(Delivery::Busy);
    };
    let Some(cursor) =
        shelbi_state::read_or_initialize_event_cursor_deadline(project, deadline, cancelled)
            .map_err(|e| anyhow!(e))?
    else {
        return Ok(Delivery::Busy);
    };
    let read = match shelbi_state::read_event_log_from_deadline(cursor, deadline, cancelled)
        .map_err(|e| anyhow!(e))?
    {
        shelbi_state::FeedRead::Read(read) => read,
        shelbi_state::FeedRead::LockUnavailable => return Ok(Delivery::Busy),
        shelbi_state::FeedRead::CursorExpired { earliest, .. } => {
            if policy == Policy::IfUrgent {
                return Ok(Delivery::Quiet);
            }
            // The cursor fell behind log rotation: those events are gone.
            // Say so once and resume from the earliest retained byte.
            let lines = vec![format!(
                "shelbi: {} bytes of board events rotated out of the log before they could be \
                 delivered; run `shelbi task list` and `shelbi workspace list` to resync",
                earliest.saturating_sub(cursor)
            )];
            emit(&Batch::notice(lines.clone()))?;
            write_persisted_cursor(project, earliest)?;
            return Ok(Delivery::Delivered(lines));
        }
    };
    // Nothing complete past the cursor: skip the board read entirely.
    if !read.bytes.contains(&b'\n') {
        return Ok(Delivery::Quiet);
    }
    let response = parse_drain(project, read)?;
    let through = response.cursor_offset;
    if through <= cursor {
        return Ok(Delivery::Quiet);
    }
    if policy == Policy::IfUrgent && !response.events.iter().any(is_urgent) {
        return Ok(Delivery::Quiet);
    }
    let shown: Vec<&NormalizedEvent> =
        response.events.iter().filter(|event| label(event).is_some()).collect();
    let lines = cap(shown.iter().filter_map(|event| render(event)).collect());
    if !lines.is_empty() {
        emit(&Batch {
            lines: lines.clone(),
            urgent: shown.iter().any(|event| is_urgent(event)),
            digest: shown.iter().all(|event| event.kind == "heartbeat"),
        })?;
    }
    write_persisted_cursor(project, through)?;
    Ok(if lines.is_empty() {
        Delivery::Quiet
    } else {
        Delivery::Delivered(lines)
    })
}

// ---------------------------------------------------------------------------
// events stream (the plugin monitor)

/// `shelbi orchestrator events stream`: the plugin monitor's command.
///
/// Claims the project's consuming-follower slot (tearing down a leaked prior
/// follower), then delivers each batch as one line per event on stdout and
/// acks it. A quiet board prints nothing. It runs until the session ends:
///
/// * A SIGTERM from a follower that took the slot (the v1 per-reply drain an
///   older `instructions.md` still asks for) is not a stop: the stream yields
///   until that follower exits, then reclaims the slot. Any other
///   SIGTERM/SIGHUP/SIGINT ends it quietly (exit 0); a session ending is not
///   news for the orchestrator.
/// * A write to a closed stdout (the session is gone) ends it without acking
///   the batch it couldn't print.
/// * `--max-lifetime`, when set, ends it with a line saying so.
///
/// With `json`, each batch is one JSON object on one line instead
/// (`{"lines": [...], "urgent": bool, "digest": bool}`), for a plugin that
/// admits a batch as a whole and needs to know whether it can wait.
pub(super) fn run_stream(project: &str, max_lifetime: Option<Duration>, json: bool) -> Result<()> {
    ensure_registered(project)?;
    let signal = Arc::new(AtomicUsize::new(0));
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        signal_hook::flag::register_usize(sig, Arc::clone(&signal), sig as usize)
            .map_err(|e| anyhow!("failed to install signal handler {sig}: {e}"))?;
    }
    let self_pid = shelbi_state::claim_event_follower(project).map_err(|e| anyhow!(e))?;
    let mut print = |batch: &Batch| print_batch(batch, json);
    let parent = std::os::unix::process::parent_id();
    let reader_alive = || std::os::unix::process::parent_id() == parent;
    let outcome = stream_loop(project, max_lifetime, &signal, STREAM_POLL, &reader_alive, &mut print);
    let _ = shelbi_state::release_event_follower(project, self_pid);
    match outcome {
        Ok(StreamEnd::Terminated(sig)) => {
            eprintln!("shelbi orchestrator events stream: stopped by signal {sig}");
            Ok(())
        }
        Ok(StreamEnd::Orphaned) => {
            eprintln!("shelbi orchestrator events stream: its parent exited; stopping");
            Ok(())
        }
        Ok(StreamEnd::Expired(after)) => {
            let _ = print_batch(
                &Batch::notice(vec![format!(
                    "shelbi: the event stream reached its --max-lifetime of {}; board events \
                     still arrive with each user message",
                    format_duration(after)
                )]),
                json,
            );
            Ok(())
        }
        Err(error) => {
            // Best-effort: stdout may be the very thing that failed.
            let _ = print_batch(
                &Batch::notice(vec![format!(
                    "shelbi: the awareness event stream stopped ({error}); board events still \
                     arrive with each user message. Restart the session to resume live \
                     notifications."
                )]),
                json,
            );
            Err(error)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StreamEnd {
    Terminated(usize),
    Expired(Duration),
    /// The process that started the stream (the runner reading its output)
    /// is gone. A runner killed outright can't stop its child, and an
    /// orphan would otherwise sit yielded to its successor, holding nothing
    /// but taking the slot back the moment that one exits.
    Orphaned,
}

fn print_batch(batch: &Batch, json: bool) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    if json {
        stdout.write_all(batch.to_json_line().as_bytes())?;
        stdout.write_all(b"\n")?;
    } else {
        for line in &batch.lines {
            stdout.write_all(line.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
    }
    // stdout is a pipe: without the flush a line would wait for the next one.
    stdout.flush()?;
    Ok(())
}

fn stream_loop(
    project: &str,
    max_lifetime: Option<Duration>,
    signal: &AtomicUsize,
    poll: Duration,
    reader_alive: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(&Batch) -> Result<()>,
) -> Result<StreamEnd> {
    let start = Instant::now();
    let self_pid = std::process::id();
    let cancelled = || signal.load(Ordering::Relaxed) != 0;
    let mut yielded = false;
    loop {
        if !reader_alive() {
            return Ok(StreamEnd::Orphaned);
        }
        let sig = signal.load(Ordering::Relaxed);
        if sig != 0 {
            if sig == SIGTERM as usize && superseded_within(project, self_pid, SUPERSEDE_GRACE) {
                signal.store(0, Ordering::Relaxed);
                yielded = true;
            } else {
                return Ok(StreamEnd::Terminated(sig));
            }
        }
        if let Some(limit) = max_lifetime {
            if start.elapsed() >= limit {
                return Ok(StreamEnd::Expired(limit));
            }
        }
        if yielded || superseded(project, self_pid) {
            // Another consumer holds the slot: let it drain, and take the slot
            // back once it has gone.
            match shelbi_state::live_event_follower(project) {
                Ok(Some(owner)) if owner != self_pid => {
                    yielded = true;
                    thread::sleep(poll);
                    continue;
                }
                _ => {
                    shelbi_state::claim_event_follower(project).map_err(|e| anyhow!(e))?;
                    yielded = false;
                }
            }
        }
        let deadline = max_lifetime.map(|limit| start + limit);
        deliver(project, deadline, &cancelled, Policy::All, emit)?;
        thread::sleep(poll);
    }
}

/// Whether a live process other than us now owns the follower slot.
fn superseded(project: &str, self_pid: u32) -> bool {
    matches!(shelbi_state::live_event_follower(project), Ok(Some(owner)) if owner != self_pid)
}

/// A claimer signals the prior owner before it records itself, so give the
/// registry a moment to name the new owner.
fn superseded_within(project: &str, self_pid: u32, grace: Duration) -> bool {
    let until = Instant::now() + grace;
    loop {
        if superseded(project, self_pid) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// hooks

/// `shelbi orchestrator hook <event>`: answer one hook call. Never
/// fails the hook: any error answers with nothing (exit 0) and a note on
/// stderr, so a Shelbi hiccup can't block the user's prompt.
pub(super) fn run_hook(project_opt: Option<String>, event: HookEvent) -> Result<()> {
    let input = read_hook_input();
    let answer = require_project(project_opt).and_then(|project| {
        ensure_registered(&project)?;
        let deadline = Some(Instant::now() + HOOK_LOCK_WAIT);
        hook_answer(&project, event, &input, deadline)
    });
    match answer {
        Ok(Some(output)) => {
            println!("{output}");
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => {
            eprintln!("shelbi orchestrator hook: {error}");
            Ok(())
        }
    }
}

/// The hook's JSON input on stdin (`{}` when there is none or it's a tty).
fn read_hook_input() -> Value {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return json!({});
    }
    let mut text = String::new();
    let _ = stdin.lock().take(1 << 20).read_to_string(&mut text);
    serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
}

/// What the hook prints, or `None` for nothing. The output carries already
/// acked events, so it is printed whatever happens next.
fn hook_answer(
    project: &str,
    event: HookEvent,
    input: &Value,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    let never = || false;
    match event {
        HookEvent::UserPromptSubmit => {
            let mut taken = Vec::new();
            deliver(project, deadline, &never, Policy::All, &mut |batch| {
                taken = batch.lines.clone();
                Ok(())
            })?;
            if taken.is_empty() {
                return Ok(None);
            }
            Ok(Some(
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": format!(
                            "Shelbi board events not yet shown to you (already acknowledged; \
                             apply them through your reaction rules before answering):\n{}",
                            taken.join("\n")
                        ),
                    }
                })
                .to_string(),
            ))
        }
        HookEvent::Stop => {
            // Already continuing because of a stop hook: never block again, so
            // the orchestrator can't be held in a loop.
            if input.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
                return Ok(None);
            }
            let mut taken = Vec::new();
            deliver(project, deadline, &never, Policy::IfUrgent, &mut |batch| {
                taken = batch.lines.clone();
                Ok(())
            })?;
            if taken.is_empty() {
                return Ok(None);
            }
            Ok(Some(
                json!({
                    "decision": "block",
                    "reason": format!(
                        "Shelbi events arrived during this turn (already acknowledged); handle \
                         them before you finish:\n{}",
                        taken.join("\n")
                    ),
                })
                .to_string(),
            ))
        }
        HookEvent::SessionStart => {
            let mut context = Vec::new();
            // A fresh session has the bootstrap prompt; after a compaction or
            // a clear, restate how events arrive so it isn't forgotten.
            if input.get("source").and_then(Value::as_str).is_some_and(|s| s != "startup") {
                context.push(DELIVERY_REMINDER.to_string());
            }
            let zen_on = shelbi_state::read_state(project)
                .map(|s| s.zen_mode == shelbi_state::ZenModeState::On)
                .unwrap_or(false);
            if zen_on {
                if let Some(summary) =
                    shelbi_state::read_zenmode_summary(project).map_err(|e| anyhow!(e))?
                {
                    context.push(format!("Zen Mode is on. {summary}"));
                }
            }
            if context.is_empty() {
                return Ok(None);
            }
            Ok(Some(
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": context.join("\n"),
                    }
                })
                .to_string(),
            ))
        }
    }
}

const DELIVERY_REMINDER: &str = "Shelbi board events reach you automatically: Shelbi's \
    awareness plugin delivers each one as a `shelbi <kind>: ...` line the moment it lands, \
    and anything not yet shown is attached to the user's next message. They are already \
    acknowledged. Don't start a Monitor on `shelbi events tail` and don't run `shelbi \
    orchestrator events next`, `drain` or `ack` yourself.";

#[cfg(test)]
mod tests {
    use super::super::tests::setup_home;
    use super::*;
    use shelbi_core::Column;
    use shelbi_state::{
        append_heartbeat_event, append_job_event, append_reminder_event, append_send_event,
        append_task_event, JobEvent,
    };

    fn collect(project: &str, policy: Policy) -> Delivery {
        deliver(project, None, &|| false, policy, &mut |_| Ok(())).unwrap()
    }

    fn lines(delivery: Delivery) -> Vec<String> {
        match delivery {
            Delivery::Delivered(lines) => lines,
            other => panic!("expected a delivery, got {other:?}"),
        }
    }

    #[test]
    fn delivers_each_event_once_as_one_compact_line() {
        let (_g, _home) = setup_home();
        append_task_event("demo", "t-1", "default", Column::todo(), Column::in_progress(), "dispatch")
            .unwrap();
        append_reminder_event("demo", "r-1", "check the tap PR merged").unwrap();
        append_reminder_event("other", "r-9", "not ours").unwrap();

        let got = lines(collect("demo", Policy::All));
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("shelbi task: task=t-1"), "{got:?}");
        assert!(got[0].contains("-> in_progress"), "{got:?}");
        assert!(!got[0].contains("project="), "scope is stripped: {got:?}");
        assert_eq!(got[1], "shelbi reminder: id=r-1 text=\"check the tap PR merged\"");

        // Acked: a second consumer (a hook, a relaunched stream) sees nothing.
        assert_eq!(collect("demo", Policy::All), Delivery::Quiet);
    }

    #[test]
    fn bookkeeping_kinds_are_acked_without_output() {
        let (_g, _home) = setup_home();
        append_send_event("demo", "alpha", "submitted", "ok").unwrap();
        assert_eq!(collect("demo", Policy::All), Delivery::Quiet);
        // ...and stay acked, so they never resurface next to a real event.
        append_reminder_event("demo", "r-1", "ping").unwrap();
        assert_eq!(lines(collect("demo", Policy::All)).len(), 1);
    }

    #[test]
    fn a_failed_hand_over_leaves_the_batch_queued() {
        let (_g, _home) = setup_home();
        append_reminder_event("demo", "r-1", "ping").unwrap();
        let err = deliver("demo", None, &|| false, Policy::All, &mut |_| {
            Err(anyhow!("broken pipe"))
        });
        assert!(err.is_err());
        assert_eq!(lines(collect("demo", Policy::All)).len(), 1, "re-delivered, not lost");
    }

    #[test]
    fn stop_policy_takes_the_batch_only_when_something_is_urgent() {
        let (_g, _home) = setup_home();
        append_task_event("demo", "t-1", "default", Column::todo(), Column::in_progress(), "x")
            .unwrap();
        assert_eq!(collect("demo", Policy::IfUrgent), Delivery::Quiet);
        // Left for the stream, not acked.
        append_job_event(
            "demo",
            &JobEvent { id: "j-1", kind: "command", label: "tests", status: "ok", exit: Some(0), ..Default::default() },
        )
        .unwrap();
        let got = lines(collect("demo", Policy::IfUrgent));
        assert_eq!(got.len(), 2, "the whole pending batch rides along: {got:?}");
        assert!(got[1].starts_with("shelbi job done: id=j-1"), "{got:?}");
    }

    #[test]
    fn stop_hook_blocks_once_and_never_while_already_continuing() {
        let (_g, _home) = setup_home();
        append_reminder_event("demo", "r-1", "ping").unwrap();
        let active = json!({"stop_hook_active": true});
        assert_eq!(hook_answer("demo", HookEvent::Stop, &active, None).unwrap(), None);

        let out = hook_answer("demo", HookEvent::Stop, &json!({}), None).unwrap().unwrap();
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("shelbi reminder: id=r-1"));
        // Delivered: the next stop is free.
        assert_eq!(hook_answer("demo", HookEvent::Stop, &json!({}), None).unwrap(), None);
    }

    #[test]
    fn prompt_hook_attaches_pending_events_as_context() {
        let (_g, _home) = setup_home();
        assert_eq!(
            hook_answer("demo", HookEvent::UserPromptSubmit, &json!({}), None).unwrap(),
            None,
            "nothing pending, nothing printed"
        );
        append_heartbeat_event("demo", 2, 1, None, None).unwrap();
        let out = hook_answer("demo", HookEvent::UserPromptSubmit, &json!({}), None)
            .unwrap()
            .unwrap();
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        let context = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(context.contains("shelbi digest: zen_eligible=2 idle_workspaces=1"), "{context}");
    }

    #[test]
    fn session_start_restates_delivery_only_after_startup() {
        let (_g, _home) = setup_home();
        let startup = json!({"source": "startup"});
        assert_eq!(hook_answer("demo", HookEvent::SessionStart, &startup, None).unwrap(), None);
        let compact = json!({"source": "compact"});
        let out = hook_answer("demo", HookEvent::SessionStart, &compact, None).unwrap().unwrap();
        assert!(out.contains("awareness plugin delivers"), "{out}");
    }

    #[test]
    fn a_busy_lock_gives_up_at_the_deadline() {
        let (_g, _home) = setup_home();
        append_reminder_event("demo", "r-1", "ping").unwrap();
        let _held = shelbi_state::acquire_event_delivery_lock("demo", None, &|| false)
            .unwrap()
            .unwrap();
        let deadline = Some(Instant::now() + Duration::from_millis(100));
        let got = deliver("demo", deadline, &|| false, Policy::All, &mut |_| Ok(())).unwrap();
        assert_eq!(got, Delivery::Busy);
    }

    #[test]
    fn a_large_backlog_is_capped_with_a_summary_line() {
        let many: Vec<String> = (0..100).map(|i| format!("shelbi task: {i}")).collect();
        let capped = cap(many);
        assert_eq!(capped.len(), MAX_LINES);
        assert!(capped.last().unwrap().starts_with("shelbi: 61 more board event(s)"), "{capped:?}");
        assert_eq!(cap(vec!["a".into(), "b".into()]), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn a_batch_says_whether_it_is_urgent_or_only_the_digest() {
        let (_g, _home) = setup_home();
        let take = || {
            let mut taken = None;
            deliver("demo", None, &|| false, Policy::All, &mut |batch| {
                taken = Some(batch.clone());
                Ok(())
            })
            .unwrap();
            taken.expect("a batch")
        };

        let mut batches = Vec::new();
        append_heartbeat_event("demo", 2, 1, None, None).unwrap();
        batches.push(take());
        append_task_event("demo", "t-1", "default", Column::todo(), Column::in_progress(), "x")
            .unwrap();
        append_heartbeat_event("demo", 1, 0, None, None).unwrap();
        batches.push(take());
        append_reminder_event("demo", "r-1", "ping").unwrap();
        batches.push(take());

        let flags: Vec<(bool, bool)> = batches.iter().map(|b| (b.urgent, b.digest)).collect();
        assert_eq!(flags, vec![(false, true), (false, false), (true, false)], "{batches:?}");
        // Bookkeeping that renders nothing doesn't make a digest batch look
        // like more than a digest.
        append_send_event("demo", "alpha", "submitted", "ok").unwrap();
        append_heartbeat_event("demo", 0, 0, None, None).unwrap();
        assert!(take().digest);

        let json: Value = serde_json::from_str(&batches[2].to_json_line()).unwrap();
        assert_eq!(
            json,
            json!({"lines": ["shelbi reminder: id=r-1 text=\"ping\""], "urgent": true, "digest": false})
        );
        assert!(!batches[2].to_json_line().contains('\n'), "one batch, one line");
    }

    #[test]
    fn stream_prints_then_stops_on_a_real_signal() {
        let (_g, _home) = setup_home();
        append_reminder_event("demo", "r-1", "ping").unwrap();
        let signal = AtomicUsize::new(0);
        let mut seen = Vec::new();
        let end = stream_loop(
            "demo",
            Some(Duration::from_millis(200)),
            &signal,
            Duration::from_millis(10),
            &|| true,
            &mut |batch| {
                seen.extend_from_slice(&batch.lines);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(end, StreamEnd::Expired(Duration::from_millis(200)));
        assert_eq!(seen, vec!["shelbi reminder: id=r-1 text=\"ping\"".to_string()]);

        // A signal while nobody else owns the slot is a real stop.
        signal.store(SIGHUP as usize, Ordering::Relaxed);
        let end = stream_loop("demo", None, &signal, Duration::from_millis(10), &|| true, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(end, StreamEnd::Terminated(SIGHUP as usize));

        // Its reader gone, it stops before delivering anything more.
        signal.store(0, Ordering::Relaxed);
        append_reminder_event("demo", "r-2", "pong").unwrap();
        let end = stream_loop("demo", None, &signal, Duration::from_millis(10), &|| false, &mut |_| {
            panic!("an orphaned stream delivers nothing")
        })
        .unwrap();
        assert_eq!(end, StreamEnd::Orphaned);
        assert_eq!(lines(collect("demo", Policy::All)).len(), 1, "the batch stays queued");
    }
}
