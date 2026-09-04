//! Machine-readable orchestrator transport primitives.
//!
//! These commands expose the append-only hub event stream with durable
//! cursors. They intentionally do not dispatch work or mutate board state.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Subcommand;
use serde::Serialize;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum OrchestratorCmd {
    /// Read project-scoped events since a cursor.
    Events {
        #[command(subcommand)]
        cmd: OrchestratorEventsCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum OrchestratorEventsCmd {
    /// Drain all currently pending project events since the cursor.
    Drain {
        /// Explicit cursor override. Omit to resume from — and persist back
        /// to — the durable cursor in the project config dir. Use `0` to
        /// replay the whole log from the start.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Wait up to `--timeout` for the next non-empty project event batch, or
    /// with `--follow` stream durable event batches continuously (claim/ack).
    Next {
        /// Durable cursor returned by a prior drain. Use `0` for the first read.
        /// Ignored under `--follow`, which always resumes from the persisted
        /// durable cursor.
        #[arg(long, default_value = "0")]
        cursor: String,
        /// Maximum wait, e.g. `10s`, `2m`, `1h`. Required unless `--follow` is
        /// set; ignored when it is.
        #[arg(long)]
        timeout: Option<String>,
        /// Stream normalized event batches as they arrive, each tagged with a
        /// stable `shelbi-event/<project>/<from>-<through>` delivery id.
        /// Reading does NOT advance the durable cursor — `events ack` does — so
        /// an unacknowledged batch is re-delivered verbatim on restart
        /// (at-least-once). This is the Claude orchestrator's durable
        /// replacement for a raw `shelbi events tail --follow` watch.
        ///
        /// The feed runs indefinitely. Liveness rides the unified event stream
        /// itself: the hub always writes at least a heartbeat on its idle
        /// cadence (and real events when busy), so a consumer that has seen no
        /// line — batch or heartbeat — within the heartbeat interval plus a
        /// grace margin can conclude the follower died and restart it. There is
        /// no separate keepalive ping. A delivered-but-unacked batch does not
        /// wedge the feed: it is held in lock-step only up to a bounded
        /// visibility timeout, after which the feed redelivers it (folding in
        /// anything appended since) rather than parking forever — so a dropped
        /// ack or a consumer that hangs mid-batch can never silence a follower
        /// while the stream is still flowing. When the feed stops on its own terms — a
        /// catchable termination signal (SIGTERM/SIGHUP/SIGINT) or the optional
        /// `--max-lifetime` cap below — it prints a terminal
        /// `{"feed":"expired"|"terminated", ...}` notice on stdout and exits 0.
        /// Any batch left unacked at exit re-delivers to the next follower. This
        /// feed is a latency accelerator; the source of truth for never missing
        /// an event is a consumer's own periodic claim/ack drain.
        #[arg(long)]
        follow: bool,
        /// Optional self-imposed wall-clock lifetime for `--follow`, e.g.
        /// `4h`. When set, the feed exits cleanly with a `feed=expired` stdout
        /// notice after this long instead of running forever — a deterministic
        /// recycle point that beats an environment reaper (even an uncatchable
        /// SIGKILL) so the death is never silent. Ignored without `--follow`;
        /// omit to run indefinitely.
        #[arg(long)]
        max_lifetime: Option<String>,
    },
    /// Advance the durable cursor past an acknowledged batch delivery id.
    ///
    /// The counterpart to `events next --follow`: apply a batch's facts, then
    /// ack its `shelbi-event/<project>/<from>-<through>` id so the feed stops
    /// re-delivering it. Idempotent — a duplicate ack never rewinds the stream.
    Ack {
        /// The delivery id printed by `events next --follow`.
        delivery_id: String,
    },
}

pub fn run(project_opt: Option<String>, cmd: OrchestratorCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        OrchestratorCmd::Events { cmd } => match cmd {
            OrchestratorEventsCmd::Drain { cursor } => {
                let cursor_override = cursor.as_deref().map(parse_cursor).transpose()?;
                let response = drain_persisted(&project, cursor_override)?;
                print_response(&response)
            }
            OrchestratorEventsCmd::Next {
                cursor,
                timeout,
                follow,
                max_lifetime,
            } => {
                if follow {
                    let max_lifetime = max_lifetime
                        .as_deref()
                        .map(super::events::parse_duration)
                        .transpose()?;
                    run_feed(&project, max_lifetime)
                } else {
                    let timeout = timeout.ok_or_else(|| {
                        anyhow!("`events next` requires --timeout unless --follow is set")
                    })?;
                    let timeout = super::events::parse_duration(&timeout)?;
                    let response = wait_next(&project, parse_cursor(&cursor)?, timeout)?;
                    print_response(&response)
                }
            }
            OrchestratorEventsCmd::Ack { delivery_id } => ack_delivery(&project, &delivery_id),
        },
    }
}

fn print_response(response: &DrainResponse) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(response)?);
    Ok(())
}

fn wait_next(project: &str, mut cursor: u64, timeout: Duration) -> Result<DrainResponse> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = drain_once(project, cursor)?;
        cursor = response.cursor_offset;
        if !response.events.is_empty() || Instant::now() >= deadline {
            return Ok(response);
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

/// Poll cadence for the durable feed. Matches `events tail --follow` so the
/// live-latency characteristics of the two watches are identical.
const FEED_POLL: Duration = Duration::from_millis(250);

/// Visibility timeout for an in-flight (emitted-but-unacked) batch. The feed
/// holds a delivered batch in lock-step so new events wait behind it under a
/// stable delivery id, but only for this long: once a batch has been
/// outstanding past this window without an ack, the feed redelivers it (picking
/// up anything appended since) rather than hold the claim forever.
///
/// This is the release valve for the failure the lock-step alone caused — a
/// consumer that silently drops an ack (or hangs after receiving a batch) used
/// to leave the follower parked forever, delivering nothing while the stream
/// kept flowing (heartbeats, zen toggles, pane deaths all landing in
/// `events.log` unseen). Chosen comfortably longer than a normal
/// process-then-`events ack` round-trip (seconds) so a healthy consumer is
/// never spammed with duplicates, yet well under the heartbeat cap (minutes) so
/// a stuck follower recovers — and re-blinds no one — within a bounded time.
const FEED_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(90);

/// How many times a single unacked batch may be redelivered before the feed
/// quarantines it: advance the durable cursor past it (without an ack) and move
/// on. A healthy consumer acks within one redelivery, resetting the count to
/// zero, so this only ever trips on a genuinely un-ackable batch — one whose
/// contents crash or hang the consumer every time. Bounding it is the escape
/// valve for case 3 of the wedge: an un-ackable batch that would otherwise be
/// re-emitted under a stable delivery id forever, with every later event queued
/// behind it. At `FEED_VISIBILITY_TIMEOUT` per redelivery this is ~15 min of a
/// stuck batch before the loud quarantine notice fires.
const FEED_MAX_REDELIVERIES: u32 = 10;

/// Durable claim/ack event feed — the Claude orchestrator's replacement for a
/// raw `shelbi events tail --follow` watch.
///
/// Streams normalized event batches keyed on stable delivery ids, resuming
/// from the persisted durable cursor. Unlike `drain`, reading here never
/// advances the cursor: a batch stays "in flight" until
/// `events ack <delivery-id>` moves the cursor past it. Two consequences fall
/// straight out of that:
///
/// * A crash between emit and ack re-delivers the identical batch (same
///   delivery id) on restart — at-least-once, the guarantee the Codex bridge
///   already gives its runner.
/// * An acked batch is never seen again, because the cursor has moved past it.
///
/// The feed advances one acknowledged batch at a time: events that arrive
/// while a batch is in flight wait for the ack and ride the next batch. That
/// lock-step keeps every delivery id a pure function of the range it covers,
/// which is what makes the restart identity hold. The whole pending tail is
/// delivered as a single batch (same read shape as `drain`); memory is bounded
/// by the log's own rotation, not by this command.
///
/// # Lifetime and death
///
/// The loop is a pure filesystem poll of the persisted cursor and
/// `events.log` — it holds no long-lived hub connection, so nothing on the hub
/// side can recycle it out from under the consumer. Left alone it runs forever,
/// emitting a batch only when one is pending. Every lock it takes on the poll
/// path is deadline- and signal-aware, so it can never park past its deadline
/// or ignore a SIGTERM while another holder has the hub-global lock. It stops on
/// its own terms three ways, each of which emits a terminal [`FeedNotice`] on
/// stdout (and a line on stderr) so a dead drain is always observable:
///
/// * a catchable termination signal — SIGTERM/SIGHUP/SIGINT, e.g. an
///   environment reaper or a supervisor recycling the process (exit 0);
/// * the optional `max_lifetime` wall-clock cap, a deterministic self-recycle
///   a supervisor can set below its reaper's threshold so the exit is always a
///   clean, message-bearing one rather than a silent kill (exit 0); and
/// * a genuine terminal error, which emits a `failed` notice before propagating
///   non-zero — never a silent Err to `main`.
///
/// A poison cursor is NOT a stop: a cursor that has fallen past the retained
/// window self-heals (fast-forwards to the earliest retained position, emits a
/// `resynchronized` notice, and continues), so the drain can never wedge on it
/// and re-die on every relaunch.
///
/// On a clean stop the in-flight (unacked) batch is left exactly where it was,
/// so the next follower re-derives and re-delivers it verbatim (at-least-once).
/// An uncatchable `SIGKILL` — or the background shell hosting the feed being
/// reaped when the agent session is recycled — still ends the process without a
/// notice, but the same redelivery guarantee holds: no event is lost, only a
/// restart is spent.
///
/// There is no separate keepalive ping. Liveness is subsumed by the unified
/// event stream: the hub always writes at least a heartbeat on its idle cadence
/// (and real events when busy), so a consumer that has seen no line — batch or
/// heartbeat — for the heartbeat interval plus a grace margin can conclude the
/// follower died and restart it. The source of truth for never missing an event
/// remains the consumer's own periodic claim/ack drain (see the orchestrator
/// instructions), for which this feed is only a latency accelerator.
fn run_feed(project: &str, max_lifetime: Option<Duration>) -> Result<()> {
    // Turn the catchable termination signals a reaper or supervisor sends into
    // a clean, message-bearing exit instead of a silently truncated stdout
    // stream. Each handler records its own signal number so the notice can name
    // the cause; the loop polls the flag every FEED_POLL.
    let signal = Arc::new(AtomicUsize::new(0));
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        signal_hook::flag::register_usize(sig, Arc::clone(&signal), sig as usize).map_err(|e| {
            anyhow!("failed to install signal handler {sig} for the event feed: {e}")
        })?;
    }
    // Claim single-consumer ownership before streaming: this tears down any
    // prior live follower for the project (SIGTERM) so a leaked one from an
    // earlier orchestrator session can't co-consume and starve this drain.
    // `feed_loop` polls the registry and stops with `Superseded` if a newer
    // follower later takes over from us.
    let self_pid = shelbi_state::claim_event_follower(project).map_err(|e| anyhow!(e))?;
    let outcome = feed_loop(project, max_lifetime, &signal);
    // Release only if we still own it — a follower we superseded leaves the
    // registry naming its successor. Best-effort: a release failure must not
    // mask the loop's own outcome.
    let _ = shelbi_state::release_event_follower(project, self_pid);
    match outcome {
        Ok(outcome) => emit_feed_notice(project, &outcome),
        // Never die silently: surface a terminal notice for a genuine error
        // before propagating it, so a dead drain is observable on the stream the
        // supervisor already watches rather than a bare non-zero exit.
        Err(error) => {
            let _ = emit_feed_notice(project, &FeedOutcome::Failed {
                error: error.to_string(),
            });
            Err(error)
        }
    }
}

/// The `--follow` poll loop, factored out of [`run_feed`] so it is testable
/// without installing process-wide signal handlers: the caller owns the
/// `signal` flag and can pre-set it (or pass a zero flag with a short
/// `max_lifetime`) to drive either exit path deterministically.
///
/// Returns on a self-determined stop — a non-zero `signal` flag, an elapsed
/// `max_lifetime`, or a supersede — as `Ok`, and only propagates `Err` on a
/// genuine terminal error (which the caller turns into a `failed` notice). A
/// poison cursor is self-healed inline (fast-forward + `resynchronized` notice)
/// and does NOT return. With `max_lifetime = None` and no signal it loops
/// forever, which is the default indefinite feed.
///
/// Both lock acquisitions on the poll path (the per-project cursor read and the
/// hub-global scan) are bounded by the remaining `max_lifetime` and cancelled by
/// the `signal` flag, so the loop honors its deadline and reacts to SIGTERM even
/// while another holder has the hub-global lock — never the old unbounded park.
///
/// There is no keepalive line: liveness is carried by the unified stream (the
/// hub's heartbeat and real events), so a quiet feed simply emits nothing until
/// the next batch. A consumer detects a dead follower from the *absence* of any
/// stream line past the heartbeat interval plus grace, not from a ping this
/// loop writes.
///
/// A batch is held in lock-step only until [`FEED_VISIBILITY_TIMEOUT`]: an
/// in-flight batch left unacked past that window is redelivered rather than
/// parked forever, so a consumer that silently dropped its ack (or hung after
/// receiving the batch) can never leave the follower delivering nothing while
/// the stream keeps flowing. See [`feed_should_scan`].
fn feed_loop(
    project: &str,
    max_lifetime: Option<Duration>,
    signal: &AtomicUsize,
) -> Result<FeedOutcome> {
    let self_pid = std::process::id();
    feed_loop_with(
        project,
        max_lifetime,
        signal,
        FEED_POLL,
        FEED_VISIBILITY_TIMEOUT,
        FEED_MAX_REDELIVERIES,
        &|| feed_superseded(project, self_pid),
        &mut |batch| emit_feed_batch(batch),
        &mut |notice| write_feed_notice(notice),
    )
}

/// True when a newer consuming follower has taken over this project's registry
/// (its recorded PID no longer names us) — the signal for this follower to exit
/// so at most one consuming follower is ever live. A missing/uninitialized
/// registry is deliberately *not* a supersede: an ad-hoc drain or a test that
/// never called [`shelbi_state::claim_event_follower`] must run normally, and a
/// transient read error fails open (keep serving) rather than kill a healthy
/// feed on a filesystem hiccup.
fn feed_superseded(project: &str, self_pid: u32) -> bool {
    matches!(shelbi_state::event_follower_owner(project), Ok(Some(owner)) if owner != self_pid)
}

/// [`feed_loop`] with the poll cadence, visibility timeout, and batch emitter
/// injected, so tests can drive the redelivery behaviour deterministically
/// (short cadences, an in-memory sink) without touching global stdout or real
/// wall-clock intervals.
#[allow(clippy::too_many_arguments)]
fn feed_loop_with(
    project: &str,
    max_lifetime: Option<Duration>,
    signal: &AtomicUsize,
    poll: Duration,
    visibility_timeout: Duration,
    max_redeliveries: u32,
    superseded: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(&FeedBatch) -> Result<()>,
    emit_notice: &mut dyn FnMut(&FeedNotice) -> Result<()>,
) -> Result<FeedOutcome> {
    let start = Instant::now();
    // A delivered termination signal (SIGTERM/SIGHUP/SIGINT) is the cancel
    // signal threaded into every deadline-aware lock acquire below, so a feed
    // blocked on lock acquisition ends promptly instead of only on the next
    // tick — or, with the old `EINTR`-swallowing blocking lock, only on SIGKILL.
    let cancelled = || signal.load(Ordering::Relaxed) != 0;
    // The batch currently in flight: the cursor it was scanned from, when it
    // was last emitted, and how many times it has been redelivered unacked.
    // `None` means nothing is outstanding, so the next pending batch emits on
    // the very next tick. While a batch is in flight we hold it in lock-step
    // (new events wait behind it under a stable delivery id) until its
    // visibility timeout elapses, at which point it is redelivered.
    let mut inflight: Option<InflightBatch> = None;
    loop {
        let sig = signal.load(Ordering::Relaxed);
        if sig != 0 {
            return Ok(FeedOutcome::Terminated { signal: sig as i32 });
        }
        // A newer follower claimed the registry: stand down so exactly one
        // consuming follower stays live. Checked before scanning so a
        // superseded follower stops claiming batches immediately.
        if superseded() {
            return Ok(FeedOutcome::Superseded);
        }
        if let Some(limit) = max_lifetime {
            if start.elapsed() >= limit {
                return Ok(FeedOutcome::Expired { after: limit });
            }
        }
        // Bound every lock acquire this tick by the remaining lifetime so the
        // deadline is honored even while a slow reader holds the hub-global
        // lock. `None` (no lifetime cap) still leaves the cancel flag as the
        // interrupt, so SIGTERM ends a blocked acquire regardless.
        let deadline = max_lifetime.map(|limit| start + limit);

        let cursor =
            match shelbi_state::read_or_initialize_event_cursor_deadline(project, deadline, &cancelled)
                .map_err(|e| anyhow!(e))?
            {
                Some(cursor) => cursor,
                // Lock timed out or the signal fired — re-check both at the top.
                None => continue,
            };
        if feed_should_scan(inflight.as_ref(), cursor, Instant::now(), visibility_timeout) {
            // A rescan at the same cursor with a batch already in flight is a
            // visibility-timeout redelivery, not a fresh ack-advanced batch.
            let is_redelivery = matches!(&inflight, Some(b) if b.cursor == cursor);
            match feed_scan(project, cursor, deadline, &cancelled)? {
                FeedScan::Batch { batch, through } => {
                    let redeliveries = if is_redelivery {
                        inflight.as_ref().map_or(0, |b| b.redeliveries) + 1
                    } else {
                        0
                    };
                    if max_redeliveries > 0 && redeliveries > max_redeliveries {
                        // Un-ackable batch: it has been redelivered past the cap
                        // and still never acked. Advance the durable cursor past
                        // it (no ack) so it stops wedging the stream, and leave a
                        // loud notice rather than replay it forever.
                        write_persisted_cursor(project, through)?;
                        emit_notice(&quarantine_notice(project, cursor, through, redeliveries))?;
                        inflight = None;
                    } else {
                        emit(&batch)?;
                        inflight = Some(InflightBatch {
                            cursor,
                            emitted: Instant::now(),
                            redeliveries,
                        });
                    }
                }
                // Nothing pending: drop any stale in-flight marker so the next
                // appended event emits immediately instead of waiting out a
                // visibility window that started before the ack landed.
                FeedScan::Empty => inflight = None,
                // Lock still held at the deadline — loop back so the top-of-tick
                // deadline/signal checks decide whether to expire, terminate, or
                // retry, without a spurious extra poll sleep.
                FeedScan::LockUnavailable => continue,
                FeedScan::CursorExpired {
                    earliest,
                    current_base,
                } => {
                    // Poison cursor: it fell past the retained window (case 2 of
                    // the wedge). Fast-forward to the earliest still-readable
                    // position, DURABLY (so a relaunch resumes healed rather than
                    // re-reading the wedged cursor and re-dying), emit a visible
                    // resync notice, and continue — never a silent Err to main.
                    let skipped = earliest.saturating_sub(cursor);
                    write_persisted_cursor(project, earliest)?;
                    inflight = None;
                    emit_notice(&resync_notice(project, cursor, earliest, current_base, skipped))?;
                    continue;
                }
            }
        }
        thread::sleep(poll);
    }
}

/// A batch handed to the consumer but not yet acked. The feed holds it in
/// lock-step — new events wait behind it under a stable delivery id — but only
/// until its visibility timeout elapses, after which it is redelivered so a
/// dropped ack or a hung consumer never leaves the follower silently blind.
struct InflightBatch {
    /// The durable cursor the batch was scanned from. While the persisted
    /// cursor still equals this, the ack has not landed.
    cursor: u64,
    /// When the batch was last emitted; the visibility timeout is measured from
    /// here, and reset on each redelivery so redeliveries are spaced out.
    emitted: Instant,
    /// How many times this same (still-unacked) batch has been redelivered. Reset
    /// to zero once the cursor advances (an ack); once it exceeds
    /// [`FEED_MAX_REDELIVERIES`] the batch is quarantined so one un-ackable batch
    /// cannot wedge the stream forever.
    redeliveries: u32,
}

/// Decide whether the feed should scan and (re)emit on this tick. Split out as
/// a pure function so the redelivery policy is unit-testable without a running
/// loop.
///
/// Returns `true` in three cases:
/// * nothing is in flight (`inflight` is `None`) — emit whatever is pending;
/// * the durable cursor has moved past the in-flight batch — it was acked, so
///   scan the next batch; or
/// * the in-flight batch has been outstanding longer than `visibility_timeout`
///   — redeliver it (at-least-once) instead of holding the claim indefinitely.
///
/// The remaining case — an in-flight batch still inside its visibility window —
/// returns `false`, preserving the lock-step that keeps a batch's delivery id
/// stable while the consumer is actively working it. Because a crash never
/// advances the cursor (only `events ack` does), redelivery here and redelivery
/// after a fresh restart share one guarantee: no acked batch is ever replayed,
/// and no unacked batch is ever dropped.
fn feed_should_scan(
    inflight: Option<&InflightBatch>,
    cursor: u64,
    now: Instant,
    visibility_timeout: Duration,
) -> bool {
    match inflight {
        None => true,
        Some(batch) => {
            batch.cursor != cursor
                || now.saturating_duration_since(batch.emitted) >= visibility_timeout
        }
    }
}

/// Why the `--follow` loop returned instead of streaming forever. Every variant
/// prints a [`FeedNotice`] so a dead drain is always observable rather than a
/// silent stall: the clean stops (`Expired`/`Terminated`/`Superseded`) exit 0,
/// and `Failed` — a genuine terminal error — still emits its notice before the
/// error propagates non-zero to `main`.
#[derive(Debug, PartialEq, Eq)]
enum FeedOutcome {
    /// The optional `--max-lifetime` wall-clock cap elapsed.
    Expired { after: Duration },
    /// A catchable termination signal arrived (SIGTERM/SIGHUP/SIGINT).
    Terminated { signal: i32 },
    /// A newer consuming follower took over the project's single-consumer
    /// registry, so this one stood down to keep exactly one follower live.
    Superseded,
    /// The loop hit a genuine terminal error (I/O, parse, cursor write). Carried
    /// so the error is surfaced as a notice before propagating, never as a silent
    /// Err to main. A poison cursor is NOT this — it self-heals and continues.
    Failed { error: String },
}

/// The recovery instruction spelled out on every terminal notice so the
/// supervising agent needn't infer it.
const FEED_RECOVERY_NOTE: &str = "unacked batches re-deliver on the next run; \
     re-run `shelbi orchestrator events next --follow` to continue";

/// Terminal notice printed when a `--follow` feed stops on its own terms. Its
/// `feed` discriminant (`"expired"` or `"terminated"`) never appears on a
/// [`FeedBatch`], so a supervisor watching stdout can tell a clean stream end
/// from a crash (which emits no notice at all).
#[derive(Debug, Serialize)]
struct FeedNotice {
    feed: &'static str,
    project: String,
    reason: String,
    note: &'static str,
}

/// Build the terminal notice for a stopped-on-its-own-terms outcome.
fn feed_notice_for(project: &str, outcome: &FeedOutcome) -> FeedNotice {
    match outcome {
        FeedOutcome::Expired { after } => FeedNotice {
            feed: "expired",
            project: project.to_string(),
            reason: format!("--max-lifetime of {} reached", format_duration(*after)),
            note: FEED_RECOVERY_NOTE,
        },
        FeedOutcome::Terminated { signal } => FeedNotice {
            feed: "terminated",
            project: project.to_string(),
            reason: format!("received signal {signal}"),
            note: FEED_RECOVERY_NOTE,
        },
        FeedOutcome::Superseded => FeedNotice {
            feed: "superseded",
            project: project.to_string(),
            reason: "a newer follower for this project took over the consumer slot".to_string(),
            note: FEED_RECOVERY_NOTE,
        },
        FeedOutcome::Failed { error } => FeedNotice {
            feed: "failed",
            project: project.to_string(),
            reason: format!("event feed drain failed: {error}"),
            note: FEED_RECOVERY_NOTE,
        },
    }
}

/// Non-terminal notice for a self-healed poison cursor: the cursor fell past the
/// retained window and was fast-forwarded so the drain keeps making progress.
fn resync_notice(project: &str, from: u64, to: u64, current_base: u64, skipped: u64) -> FeedNotice {
    FeedNotice {
        feed: "resynchronized",
        project: project.to_string(),
        reason: format!(
            "cursor {from} predated the retained event log; fast-forwarded to {to} \
             (current base {current_base}), skipping {skipped} bytes of expired events"
        ),
        note: FEED_RECOVERY_NOTE,
    }
}

/// Non-terminal notice for a quarantined un-ackable batch: it was redelivered
/// past the cap without an ack, so the cursor was advanced past it to unwedge
/// the stream.
fn quarantine_notice(project: &str, from: u64, through: u64, redeliveries: u32) -> FeedNotice {
    FeedNotice {
        feed: "quarantined",
        project: project.to_string(),
        reason: format!(
            "batch {from}-{through} went unacked across {redeliveries} redeliveries; \
             advanced the cursor past it to unwedge the stream"
        ),
        note: FEED_RECOVERY_NOTE,
    }
}

fn emit_feed_notice(project: &str, outcome: &FeedOutcome) -> Result<()> {
    write_feed_notice(&feed_notice_for(project, outcome))
}

/// Write a notice to both streams: a human-readable line on stderr for log
/// scans, and the machine notice on stdout — the same stream the supervisor
/// already parses for batches. Used for both terminal outcomes and the
/// non-terminal resync/quarantine notices.
fn write_feed_notice(notice: &FeedNotice) -> Result<()> {
    eprintln!(
        "shelbi orchestrator events feed [{}]: {} — {}",
        notice.feed, notice.reason, notice.note
    );
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, notice)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

/// Render a duration as a compact `1h30m` / `45s` string for the notice
/// `reason`. Zero collapses to `0s` rather than the empty string.
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

/// Scan the next deliverable batch starting at `cursor` WITHOUT advancing the
/// durable cursor. Returns `None` when no project-scoped events are pending.
///
/// Test-only blocking sibling of [`feed_scan`]: the live loop uses the
/// deadline-aware [`feed_scan`], but the batch-identity/redelivery tests read
/// more clearly against this simple `Option<FeedBatch>` shape.
#[cfg(test)]
fn scan_feed_batch(project: &str, cursor: u64) -> Result<Option<FeedBatch>> {
    let response = drain_once(project, cursor)?;
    if response.events.is_empty() {
        return Ok(None);
    }
    let through = response.cursor_offset;
    // Derive the batch id from the shared event-log core so the feed keys its
    // batches identically to the Codex native bridge.
    let delivery_id = shelbi_state::delivery_id(project, cursor, through);
    Ok(Some(FeedBatch {
        ack: format!("shelbi orchestrator events ack {delivery_id}"),
        delivery_id,
        project: project.to_string(),
        cursor: FeedCursor {
            from: cursor.to_string(),
            through: through.to_string(),
        },
        events: response.events,
    }))
}

/// Outcome of one deadline-aware feed scan. Distinguishes the states the
/// `--follow` loop must react to differently: a pending batch, an empty stream,
/// a lock it could not take before its deadline, and a poison cursor that fell
/// past the retained window and must be resynchronized rather than fatal.
#[derive(Debug)]
enum FeedScan {
    /// A pending batch to emit, plus the logical cursor it advances to on ack.
    Batch { batch: FeedBatch, through: u64 },
    /// Nothing project-scoped is pending.
    Empty,
    /// The hub-global lock was still held at the deadline; retry after the
    /// caller re-checks its own deadline and termination signal.
    LockUnavailable,
    /// `cursor` predates the earliest retained generation. The caller must
    /// fast-forward to `earliest` and resynchronize.
    CursorExpired { earliest: u64, current_base: u64 },
}

/// Deadline-aware scan for the `--follow` loop: reads the next deliverable
/// batch from `cursor` WITHOUT advancing the durable cursor, honoring `deadline`
/// / `cancelled` on the hub-global lock and surfacing a poison cursor as
/// [`FeedScan::CursorExpired`] instead of a fatal error.
fn feed_scan(
    project: &str,
    cursor: u64,
    deadline: Option<Instant>,
    cancelled: &dyn Fn() -> bool,
) -> Result<FeedScan> {
    let read = match shelbi_state::read_event_log_from_deadline(cursor, deadline, cancelled)
        .map_err(|e| anyhow!(e))?
    {
        shelbi_state::FeedRead::Read(read) => read,
        shelbi_state::FeedRead::LockUnavailable => return Ok(FeedScan::LockUnavailable),
        shelbi_state::FeedRead::CursorExpired {
            earliest,
            current_base,
            ..
        } => {
            return Ok(FeedScan::CursorExpired {
                earliest,
                current_base,
            })
        }
    };
    let response = parse_drain(project, read)?;
    if response.events.is_empty() {
        return Ok(FeedScan::Empty);
    }
    let through = response.cursor_offset;
    let delivery_id = shelbi_state::delivery_id(project, cursor, through);
    Ok(FeedScan::Batch {
        batch: FeedBatch {
            ack: format!("shelbi orchestrator events ack {delivery_id}"),
            delivery_id,
            project: project.to_string(),
            cursor: FeedCursor {
                from: cursor.to_string(),
                through: through.to_string(),
            },
            events: response.events,
        },
        through,
    })
}

fn emit_feed_batch(batch: &FeedBatch) -> Result<()> {
    // Explicit flush: stdout is a pipe under `run_in_background`, so the block
    // buffer would otherwise hide the batch from the Monitor watch until the
    // next batch (or process exit) forced a flush.
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, batch)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

/// Advance the durable cursor past an acknowledged batch.
///
/// Idempotent by design: acking a batch already behind the cursor is a no-op,
/// so re-acking a re-delivered batch the orchestrator had in fact processed
/// never rewinds the stream. It refuses to jump the cursor *forward* over a
/// gap (`from` beyond the current cursor), which would silently drop the
/// unacknowledged events in between.
fn ack_delivery(project: &str, delivery_id: &str) -> Result<()> {
    let (id_project, from, through) = parse_delivery_id(delivery_id)?;
    if id_project != project {
        return Err(anyhow!(
            "delivery id `{delivery_id}` is scoped to project `{id_project}`, not `{project}`"
        ));
    }
    let current = read_persisted_cursor(project)?;
    let (outcome, cursor_after) = if through <= current {
        // Already drained past this batch — a duplicate or stale ack.
        ("already-acked", current)
    } else if from <= current {
        write_persisted_cursor(project, through)?;
        ("acked", through)
    } else {
        return Err(anyhow!(
            "delivery id `{delivery_id}` starts at {from} but the durable cursor is at {current}; \
             acking it would skip unacknowledged events — ack the pending batch first"
        ));
    };
    let response = AckResponse {
        project: project.to_string(),
        delivery_id: delivery_id.to_string(),
        outcome: outcome.to_string(),
        cursor: cursor_after.to_string(),
    };
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// Parse `shelbi-event/<project>/<from>-<through>` into its parts. `<from>` and
/// `<through>` are the logical byte cursors the batch covers. The project
/// segment may itself contain `/`, so the numeric range is split off the tail.
fn parse_delivery_id(delivery_id: &str) -> Result<(String, u64, u64)> {
    let rest = delivery_id
        .strip_prefix("shelbi-event/")
        .ok_or_else(|| anyhow!("`{delivery_id}` is not a shelbi-event delivery id"))?;
    let (project, range) = rest.rsplit_once('/').ok_or_else(|| {
        anyhow!("`{delivery_id}` is missing its `<project>/<from>-<through>` range")
    })?;
    let (from, through) = range
        .rsplit_once('-')
        .ok_or_else(|| anyhow!("`{delivery_id}` range `{range}` is not `<from>-<through>`"))?;
    if project.is_empty() {
        return Err(anyhow!("`{delivery_id}` has an empty project segment"));
    }
    let from = parse_cursor(from)?;
    let through = parse_cursor(through)?;
    if through < from {
        return Err(anyhow!(
            "`{delivery_id}` range ends ({through}) before it starts ({from})"
        ));
    }
    Ok((project.to_string(), from, through))
}

#[derive(Debug, Serialize)]
struct FeedBatch {
    delivery_id: String,
    project: String,
    cursor: FeedCursor,
    /// The exact command that advances the durable cursor past this batch.
    /// Re-emitted verbatim on every restart until it is acked.
    ack: String,
    events: Vec<NormalizedEvent>,
}

#[derive(Debug, Serialize)]
struct FeedCursor {
    from: String,
    through: String,
}

#[derive(Debug, Serialize)]
struct AckResponse {
    project: String,
    delivery_id: String,
    /// `acked` (cursor advanced) or `already-acked` (idempotent no-op).
    outcome: String,
    cursor: String,
}

fn drain_once(project: &str, cursor: u64) -> Result<DrainResponse> {
    let read = shelbi_state::read_event_log_from(cursor).map_err(|e| anyhow!(e))?;
    parse_drain(project, read)
}

/// Parse a raw [`EventLogRead`] into a project-scoped [`DrainResponse`]. Shared
/// by the blocking [`drain_once`] and the deadline-aware [`feed_scan`], so both
/// derive the same normalized events, cursor, and delivery range from a read.
fn parse_drain(project: &str, read: shelbi_state::EventLogRead) -> Result<DrainResponse> {
    let scope = ProjectScope::load(project)?;
    let start = read.start;
    let buf = read.bytes;

    let complete_len = match buf.iter().rposition(|b| *b == b'\n') {
        Some(idx) => idx + 1,
        None => 0,
    };
    let next_cursor = start + complete_len as u64;
    let text = String::from_utf8_lossy(&buf[..complete_len]);
    let mut events = Vec::new();
    let mut line_offset = start;

    for line_with_nl in text.split_inclusive('\n') {
        let line_len = line_with_nl.len() as u64;
        let line = line_with_nl.trim_end_matches(['\r', '\n']);
        let line_cursor = line_offset + line_len;
        if !line.is_empty() {
            if let Some(event) = normalize_line(&scope, line_offset, line_cursor, line) {
                events.push(event);
            }
        }
        line_offset = line_cursor;
    }

    Ok(DrainResponse {
        project: project.to_string(),
        cursor: next_cursor.to_string(),
        cursor_offset: next_cursor,
        events,
    })
}

/// Drain with durable-cursor persistence anchored in the project config
/// dir (`~/.shelbi/projects/<name>/event-cursor`), independent of the
/// caller's shell cwd.
///
/// * `cursor_override = None` — resume from the persisted cursor.
/// * `cursor_override = Some(n)` — start at `n` (an explicit replay).
///
/// Either way the new cursor is written back only after `drain_once`
/// succeeds, so a failed drain never clobbers the persisted position.
fn drain_persisted(project: &str, cursor_override: Option<u64>) -> Result<DrainResponse> {
    let start = match cursor_override {
        Some(cursor) => cursor,
        None => read_persisted_cursor(project)?,
    };
    let response = drain_once(project, start)?;
    write_persisted_cursor(project, response.cursor_offset)?;
    Ok(response)
}

/// Durable cursor path: a fixed location in the project config dir, NOT
/// under `.claude/` — Shelbi state is runner-agnostic (the Codex runner
/// drains the same stream) and `.claude/` is deployed agent config, not
/// durable state.
#[cfg(test)]
fn cursor_path(project: &str) -> Result<PathBuf> {
    shelbi_state::event_cursor_path(project).map_err(|e| anyhow!(e))
}

fn read_persisted_cursor(project: &str) -> Result<u64> {
    shelbi_state::read_or_initialize_event_cursor(project).map_err(|e| anyhow!(e))
}

fn write_persisted_cursor(project: &str, cursor: u64) -> Result<()> {
    shelbi_state::write_event_cursor(project, cursor).map_err(|e| anyhow!(e))
}

fn parse_cursor(cursor: &str) -> Result<u64> {
    cursor
        .trim()
        .parse()
        .map_err(|_| anyhow!("cursor `{cursor}` is not a Shelbi event cursor"))
}

#[derive(Debug)]
struct ProjectScope {
    project: String,
    task_ids: HashSet<String>,
}

impl ProjectScope {
    fn load(project: &str) -> Result<Self> {
        let task_ids = shelbi_state::issue_store_for(project)
            .and_then(|s| s.list())
            .map_err(|e| anyhow!(e))?
            .into_iter()
            .map(|tf| tf.task.id)
            .collect();
        Ok(Self {
            project: project.to_string(),
            task_ids,
        })
    }
}

#[derive(Debug, Serialize)]
struct DrainResponse {
    project: String,
    cursor: String,
    #[serde(skip)]
    cursor_offset: u64,
    events: Vec<NormalizedEvent>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct NormalizedEvent {
    cursor: String,
    offset: u64,
    timestamp: Option<String>,
    kind: String,
    raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    metadata: BTreeMap<String, String>,
}

fn normalize_line(
    scope: &ProjectScope,
    offset: u64,
    cursor: u64,
    line: &str,
) -> Option<NormalizedEvent> {
    let parsed = ParsedLine::parse(line);
    if !line_belongs_to_project(scope, &parsed) {
        return None;
    }

    let kind = event_kind(&parsed);
    Some(NormalizedEvent {
        cursor: cursor.to_string(),
        offset,
        timestamp: parsed.timestamp,
        kind,
        raw: line.to_string(),
        task: parsed.fields.get("task").cloned(),
        workspace: parsed.fields.get("workspace").cloned(),
        workflow: parsed.fields.get("workflow").cloned(),
        from: parsed.from,
        to: parsed.to,
        reason: parsed.fields.get("reason").cloned(),
        metadata: parsed.fields,
    })
}

fn line_belongs_to_project(scope: &ProjectScope, parsed: &ParsedLine) -> bool {
    if let Some(project) = parsed.fields.get("project") {
        return project == &scope.project;
    }

    // Compatibility for pre-project-scoped task/message lines. New task
    // transition events include `project=`, but old logs are still useful.
    // Workspace-only legacy lines are intentionally excluded because
    // workspace names are unique only within a project.
    parsed
        .fields
        .get("task")
        .is_some_and(|task| scope.task_ids.contains(task))
}

fn event_kind(parsed: &ParsedLine) -> String {
    // Match the review-ready discriminator before the broad `task=` / `workspace=`
    // branches: a review-ready line carries both as metadata but is its own kind.
    if parsed.fields.contains_key("review-ready") {
        return "review_ready".into();
    }
    if parsed.fields.contains_key("heartbeat") {
        return "heartbeat".into();
    }
    if parsed.fields.get("mode").is_some_and(|mode| mode == "zen") && parsed.from.is_some() {
        return "zen_mode_transition".into();
    }
    if parsed.fields.contains_key("task") && parsed.from.is_some() {
        return "task_transition".into();
    }
    if parsed.fields.contains_key("workspace") && parsed.from.is_some() {
        return "workspace_transition".into();
    }
    if parsed.fields.contains_key("message") {
        return "message".into();
    }
    if parsed.fields.contains_key("question") {
        return "clarification".into();
    }
    if parsed
        .fields
        .get("pane_alive")
        .is_some_and(|alive| alive == "false")
        || parsed
            .fields
            .get("server_alive")
            .is_some_and(|alive| alive == "false")
    {
        return "pane_death".into();
    }
    if parsed.fields.contains_key("pane_alive") || parsed.fields.contains_key("server_alive") {
        return "pane_lifecycle".into();
    }
    if parsed.fields.contains_key("supervision") {
        return "supervision".into();
    }
    if parsed.fields.contains_key("send") {
        return "send".into();
    }
    if parsed.fields.contains_key("dispatch") {
        return "dispatch".into();
    }
    if parsed.fields.contains_key("rebase") {
        return "rebase".into();
    }
    "event".into()
}

#[derive(Debug)]
struct ParsedLine {
    timestamp: Option<String>,
    fields: BTreeMap<String, String>,
    from: Option<String>,
    to: Option<String>,
}

impl ParsedLine {
    fn parse(line: &str) -> Self {
        let mut parts = line.split_whitespace().collect::<Vec<_>>();
        let timestamp = parts.first().map(|s| (*s).to_string());
        if !parts.is_empty() {
            parts.remove(0);
        }

        let mut fields = BTreeMap::new();
        for part in &parts {
            if let Some((k, v)) = part.split_once('=') {
                fields.insert(k.to_string(), v.to_string());
            } else if *part == "heartbeat" {
                fields.insert("heartbeat".into(), "true".into());
            } else if *part == "dispatch"
                || *part == "send"
                || *part == "rebase"
                || *part == "zen-dryrun"
                || *part == "review-ready"
            {
                fields.insert((*part).to_string(), "true".into());
            }
        }

        let mut from = None;
        let mut to = None;
        if let Some(idx) = parts.iter().position(|p| *p == "->") {
            if idx > 0 {
                from = Some(parts[idx - 1].to_string());
            }
            if idx + 1 < parts.len() {
                to = Some(parts[idx + 1].to_string());
            }
        }

        Self {
            timestamp,
            fields,
            from,
            to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use crate::commands::test_support::ENV_LOCK;
    use chrono::Utc;
    use shelbi_core::{Column, Issue};
    use shelbi_state::{
        append_external_event, append_heartbeat_event, append_send_event, append_task_event,
        append_workspace_event, list_tasks, save_task, WorkspaceState,
    };
    use tempfile::TempDir;

    struct TestHome {
        _tmp: TempDir,
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_ROOT");
        }
    }

    fn setup_home() -> (std::sync::MutexGuard<'static, ()>, TestHome) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::remove_var("SHELBI_HOME");
        std::env::set_var("SHELBI_ROOT", tmp.path());
        // Register the `demo` project these tests seed against, so the
        // project-scope board read (`ProjectScope::load`) can resolve an
        // `IssueStore` for it (the migrated path loads the project config).
        let projects = tmp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("demo.yaml"), DEMO_PROJECT_YAML).unwrap();
        (guard, TestHome { _tmp: tmp })
    }

    /// Minimal parseable project registration for the `demo` fixture — enough
    /// for `load_project` + `resolve_issue_store` (defaulting to the
    /// `file_system` backend) to succeed.
    const DEMO_PROJECT_YAML: &str = "name: demo\n\
repo: /tmp/demo\n\
default_branch: main\n\
orchestrator:\n\
\x20 runner: claude\n\
agent_runners:\n\
\x20 claude:\n\
\x20\x20\x20 command: claude\n\
\x20\x20\x20 flags: []\n\
machines:\n\
\x20 - name: local\n\
\x20\x20\x20 kind: local\n\
\x20\x20\x20 work_dir: /tmp/demo\n\
workspaces: []\n";

    fn save_demo_task(project: &str, id: &str) {
        save_demo_task_in_column(project, id, Column::todo());
    }

    fn save_demo_task_in_column(project: &str, id: &str, column: Column) {
        let now = Utc::now();
        let task = Issue {
            id: id.into(),
            title: id.into(),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        };
        save_task(project, &task, "body").unwrap();
    }

    fn event<'a>(response: &'a DrainResponse, kind: &str, id: &str) -> &'a NormalizedEvent {
        response
            .events
            .iter()
            .find(|event| {
                event.kind == kind
                    && (event.task.as_deref() == Some(id) || event.workspace.as_deref() == Some(id))
            })
            .unwrap_or_else(|| panic!("missing {kind} event for {id}: {:?}", response.events))
    }

    /// Golden test: `events drain` output must stay byte-for-byte stable for
    /// a fixed `events.log` fixture. The fixture pins the on-disk timestamps
    /// (so the result is deterministic), the per-line byte cursors, the
    /// project-scope filtering, and the whole `event_kind` vocabulary this
    /// command projects onto the stream. If this diff moves, a runner that
    /// parses the drain JSON has to be re-checked — it is not a free refactor.
    #[test]
    fn drain_output_is_byte_stable_for_a_fixed_log_fixture() {
        let (_guard, _tmp) = setup_home();
        let fixture = "\
2026-01-02T03:04:05+00:00 project=demo task=fix-1 workflow=task todo -> in_progress reason=dispatch from_category=backlog to_category=active
2026-01-02T03:04:06+00:00 project=demo workspace=alpha working -> awaiting_input
2026-01-02T03:04:07+00:00 project=demo heartbeat zen_eligible=1 idle_workspaces=2
2026-01-02T03:04:08+00:00 send project=demo workspace=alpha status=stuck detail=unconfirmed_after_retry
2026-01-02T03:04:09+00:00 project=demo workspace=alpha pane_alive=false reason=signal:SIGHUP
2026-01-02T03:04:10+00:00 project=other workspace=beta working -> awaiting_input
";
        fs::write(shelbi_state::events_log_path().unwrap(), fixture).unwrap();

        let response = drain_once("demo", 0).unwrap();
        let json = serde_json::to_string_pretty(&response).unwrap();

        let expected = r#"{
  "project": "demo",
  "cursor": "582",
  "events": [
    {
      "cursor": "141",
      "offset": 0,
      "timestamp": "2026-01-02T03:04:05+00:00",
      "kind": "task_transition",
      "raw": "2026-01-02T03:04:05+00:00 project=demo task=fix-1 workflow=task todo -> in_progress reason=dispatch from_category=backlog to_category=active",
      "task": "fix-1",
      "workflow": "task",
      "from": "todo",
      "to": "in_progress",
      "reason": "dispatch",
      "metadata": {
        "from_category": "backlog",
        "project": "demo",
        "reason": "dispatch",
        "task": "fix-1",
        "to_category": "active",
        "workflow": "task"
      }
    },
    {
      "cursor": "222",
      "offset": 141,
      "timestamp": "2026-01-02T03:04:06+00:00",
      "kind": "workspace_transition",
      "raw": "2026-01-02T03:04:06+00:00 project=demo workspace=alpha working -> awaiting_input",
      "workspace": "alpha",
      "from": "working",
      "to": "awaiting_input",
      "metadata": {
        "project": "demo",
        "workspace": "alpha"
      }
    },
    {
      "cursor": "304",
      "offset": 222,
      "timestamp": "2026-01-02T03:04:07+00:00",
      "kind": "heartbeat",
      "raw": "2026-01-02T03:04:07+00:00 project=demo heartbeat zen_eligible=1 idle_workspaces=2",
      "metadata": {
        "heartbeat": "true",
        "idle_workspaces": "2",
        "project": "demo",
        "zen_eligible": "1"
      }
    },
    {
      "cursor": "408",
      "offset": 304,
      "timestamp": "2026-01-02T03:04:08+00:00",
      "kind": "send",
      "raw": "2026-01-02T03:04:08+00:00 send project=demo workspace=alpha status=stuck detail=unconfirmed_after_retry",
      "workspace": "alpha",
      "metadata": {
        "detail": "unconfirmed_after_retry",
        "project": "demo",
        "send": "true",
        "status": "stuck",
        "workspace": "alpha"
      }
    },
    {
      "cursor": "501",
      "offset": 408,
      "timestamp": "2026-01-02T03:04:09+00:00",
      "kind": "pane_death",
      "raw": "2026-01-02T03:04:09+00:00 project=demo workspace=alpha pane_alive=false reason=signal:SIGHUP",
      "workspace": "alpha",
      "reason": "signal:SIGHUP",
      "metadata": {
        "pane_alive": "false",
        "project": "demo",
        "reason": "signal:SIGHUP",
        "workspace": "alpha"
      }
    }
  ]
}"#;
        assert_eq!(json, expected, "drain golden output drifted:\n{json}");
    }

    #[test]
    fn drain_filters_by_project_and_returns_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "owned");
        save_demo_task("other", "foreign");
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event("other", "alpha", None, WorkspaceState::Working).unwrap();
        append_task_event(
            "demo",
            "owned",
            "default",
            Column::todo(),
            Column::done(),
            "test",
        )
        .unwrap();
        append_task_event(
            "other",
            "foreign",
            "default",
            Column::todo(),
            Column::done(),
            "test",
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.project, "demo");
        assert!(response.cursor_offset > 0);
        assert_eq!(response.events.len(), 2);
        assert_eq!(response.events[0].workspace.as_deref(), Some("alpha"));
        assert_eq!(response.events[1].task.as_deref(), Some("owned"));
        assert_eq!(response.events[1].kind, "task_transition");
    }

    #[test]
    fn drain_labels_send_verdicts_and_filters_by_project() {
        let (_guard, _tmp) = setup_home();
        append_send_event("demo", "alpha", "submitted", "busy_observed").unwrap();
        append_send_event("demo", "bravo", "stuck", "still_in_input_after_retry").unwrap();
        append_send_event("other", "charlie", "stuck", "transport_error").unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.events.len(), 2);
        let submitted = event(&response, "send", "alpha");
        assert_eq!(
            submitted.metadata.get("send").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            submitted.metadata.get("status").map(String::as_str),
            Some("submitted")
        );
        assert_eq!(
            submitted.metadata.get("detail").map(String::as_str),
            Some("busy_observed")
        );

        let stuck = event(&response, "send", "bravo");
        assert_eq!(
            stuck.metadata.get("status").map(String::as_str),
            Some("stuck")
        );
        assert_eq!(
            stuck.metadata.get("detail").map(String::as_str),
            Some("still_in_input_after_retry")
        );
        assert!(
            response
                .events
                .iter()
                .all(|event| event.workspace.as_deref() != Some("charlie")),
            "foreign-project send must not cross the durable drain boundary"
        );
    }

    #[test]
    fn bundled_orchestrator_prompt_makes_stuck_send_actionable() {
        let prompt = shelbi_state::DEFAULT_ORCHESTRATOR_INSTRUCTIONS;
        assert!(
            prompt.contains("send project=<you> workspace=<name> status=<status> detail=<reason>")
        );
        assert!(prompt.contains("For `status=stuck`, do not assume the worker received the text"));
        assert!(prompt.contains("never fall back to raw\n  `tmux send-keys`"));
        assert!(prompt.contains("shelbi message <task-id> directive"));
    }

    #[test]
    fn drain_starts_after_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();
        let first = drain_once("demo", 0).unwrap();
        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();

        let second = drain_once("demo", first.cursor_offset).unwrap();

        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].task.as_deref(), Some("second"));
    }

    #[test]
    fn next_timeout_returns_updated_cursor_for_unrelated_events() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("other", "alpha", None, WorkspaceState::Working).unwrap();

        let response = wait_next("demo", 0, Duration::from_millis(1)).unwrap();

        assert!(response.events.is_empty());
        assert!(response.cursor_offset > 0);
    }

    #[test]
    fn future_cursor_fails_closed_instead_of_replaying_another_generation() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();

        let error = drain_once("demo", 999_999).unwrap_err();
        assert!(error.to_string().contains("ahead of event-log head"));
    }

    #[test]
    fn legacy_future_cursor_is_normalized_only_before_index_establishment() {
        let (_guard, _tmp) = setup_home();
        let path = shelbi_state::events_log_path().unwrap();
        let line = "t project=demo workspace=alpha none -> working\n";
        fs::write(&path, line).unwrap();
        shelbi_state::write_event_cursor("demo", 999_999).unwrap();
        let index_path = path.with_extension("log.index.json");
        assert!(!index_path.exists());

        let migrated = drain_persisted("demo", None).unwrap();
        assert_eq!(migrated.events.len(), 1);
        assert_eq!(migrated.events[0].workspace.as_deref(), Some("alpha"));
        assert_eq!(migrated.cursor_offset, line.len() as u64);
        assert!(index_path.exists());

        // Once the logical index exists, the same impossible cursor is no
        // longer legacy rotation evidence and must fail closed.
        shelbi_state::write_event_cursor("demo", 999_999).unwrap();
        let error = drain_persisted("demo", None).unwrap_err();
        assert!(error.to_string().contains("ahead of event-log head"));
        assert_eq!(
            fs::read_to_string(cursor_path("demo").unwrap())
                .unwrap()
                .trim(),
            "999999"
        );
    }

    #[test]
    fn drain_crosses_rotation_without_skipping_regrown_current_prefix() {
        let (_guard, _tmp) = setup_home();
        let path = shelbi_state::events_log_path().unwrap();
        let first = "t project=demo workspace=first none -> working\n";
        let second = "t project=demo workspace=second none -> working\n";
        let foreign = "t project=other workspace=filler none -> working\n";
        let mut old = String::with_capacity(8 * 1024 * 1024 + foreign.len());
        old.push_str(first);
        old.push_str(second);
        while old.len() < 8 * 1024 * 1024 {
            old.push_str(foreign);
        }
        fs::write(&path, old).unwrap();
        let cursor = first.len() as u64;

        // This append rotates the over-size old file and creates a current
        // generation. Make current longer than the old physical cursor so the
        // regression cannot be hidden by the historical `cursor > len` reset.
        for index in 0..4 {
            append_workspace_event(
                "demo",
                &format!("current-{index}"),
                None,
                WorkspaceState::Working,
            )
            .unwrap();
        }
        assert!(fs::metadata(&path).unwrap().len() > cursor);

        let response = drain_once("demo", cursor).unwrap();
        assert_eq!(response.events[0].workspace.as_deref(), Some("second"));
        for index in 0..4 {
            let expected = format!("current-{index}");
            assert!(response
                .events
                .iter()
                .any(|event| event.workspace.as_deref() == Some(expected.as_str())));
        }
        assert!(response
            .events
            .iter()
            .all(|event| event.workspace.as_deref() != Some("filler")));
    }

    #[test]
    fn drain_presents_ready_task_and_idle_workspace_as_distinct_facts() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-task");
        append_task_event(
            "demo",
            "ready-task",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();
        append_workspace_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.events.len(), 2);
        let task = &response.events[0];
        assert_eq!(task.kind, "task_transition");
        assert_eq!(task.task.as_deref(), Some("ready-task"));
        assert_eq!(task.from.as_deref(), Some("backlog"));
        assert_eq!(task.to.as_deref(), Some("todo"));
        assert_eq!(
            task.metadata.get("to_category").map(String::as_str),
            Some("ready")
        );
        assert_eq!(task.reason.as_deref(), Some("user:move"));

        let workspace = &response.events[1];
        assert_eq!(workspace.kind, "workspace_transition");
        assert_eq!(workspace.workspace.as_deref(), Some("alpha"));
        assert_eq!(workspace.from.as_deref(), Some("working"));
        assert_eq!(workspace.to.as_deref(), Some("awaiting_input"));
    }

    #[test]
    fn drain_surfaces_review_ready_with_distinct_kind_and_location_metadata() {
        let (_guard, _tmp) = setup_home();
        shelbi_state::append_review_ready_event(&shelbi_state::ReviewReadyEvent {
            project: "demo",
            task_id: "fix-login",
            title: "Fix the login redirect loop",
            workspace: "review",
            pane: "shelbi-demo:review",
            worktree: "/repo/.shelbi/wt/review",
            port: Some(4310),
            url: Some("http://localhost:4310"),
            notes: "Redirect resolves once; no loop on refresh",
        })
        .unwrap();

        let response = drain_once("demo", 0).unwrap();
        assert_eq!(response.events.len(), 1);
        let ev = &response.events[0];

        // Its own kind — not mislabeled as a task/workspace transition despite
        // carrying task=/workspace= metadata (it has no `->` edge).
        assert_eq!(ev.kind, "review_ready");
        assert_eq!(ev.task.as_deref(), Some("fix-login"));
        assert_eq!(ev.workspace.as_deref(), Some("review"));

        // The structured location payload the orchestrator keys on to jump
        // straight to the pane + worktree without discovery.
        assert_eq!(ev.metadata.get("state").map(String::as_str), Some("serving"));
        assert_eq!(
            ev.metadata.get("pane").map(String::as_str),
            Some("shelbi-demo:review")
        );
        assert_eq!(
            ev.metadata.get("worktree").map(String::as_str),
            Some("/repo/.shelbi/wt/review")
        );
        assert_eq!(ev.metadata.get("port").map(String::as_str), Some("4310"));
        assert_eq!(
            ev.metadata.get("url").map(String::as_str),
            Some("http://localhost:4310")
        );
        assert!(ev
            .metadata
            .get("title")
            .is_some_and(|t| t.contains("Fix_the_login")));
        assert!(ev
            .metadata
            .get("notes")
            .is_some_and(|n| n.contains("Redirect_resolves_once")));
    }

    #[test]
    fn polling_turn_boundary_delivers_ready_task_without_scheduling_it() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-task");
        append_task_event(
            "demo",
            "ready-task",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();

        // Polling-only runners see the fact when they drain at the next turn
        // boundary. This is a fact batch, not an implicit dispatch.
        let response = drain_once("demo", 0).unwrap();
        let task = event(&response, "task_transition", "ready-task");

        assert_eq!(task.from.as_deref(), Some("backlog"));
        assert_eq!(task.to.as_deref(), Some("todo"));
        assert_eq!(
            task.metadata.get("to_category").map(String::as_str),
            Some("ready")
        );
        let stored = list_tasks("demo").unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].task.column, Column::todo());
        assert_eq!(
            stored[0].task.assigned_to, None,
            "event delivery must not assign or start work"
        );
    }

    #[test]
    fn drain_presents_active_to_handoff_and_idle_workspace_as_facts() {
        let (_guard, _tmp) = setup_home();
        save_demo_task_in_column("demo", "handoff-task", Column::review());
        append_task_event(
            "demo",
            "handoff-task",
            "default",
            Column::in_progress(),
            Column::review(),
            "workspace:ready",
        )
        .unwrap();
        append_workspace_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();
        let handoff = event(&response, "task_transition", "handoff-task");
        assert_eq!(handoff.from.as_deref(), Some("in_progress"));
        assert_eq!(handoff.to.as_deref(), Some("review"));
        assert_eq!(
            handoff.metadata.get("from_category").map(String::as_str),
            Some("active")
        );
        assert_eq!(
            handoff.metadata.get("to_category").map(String::as_str),
            Some("handoff")
        );

        let workspace = event(&response, "workspace_transition", "alpha");
        assert_eq!(workspace.from.as_deref(), Some("working"));
        assert_eq!(workspace.to.as_deref(), Some("awaiting_input"));
    }

    #[test]
    fn idle_workspace_event_arrives_while_ready_task_remains_orchestrator_choice() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-next");
        append_workspace_event(
            "demo",
            "dev-1",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();
        let workspace = event(&response, "workspace_transition", "dev-1");
        assert_eq!(workspace.to.as_deref(), Some("awaiting_input"));

        let stored = list_tasks("demo").unwrap();
        assert_eq!(stored[0].task.id, "ready-next");
        assert_eq!(stored[0].task.column, Column::todo());
        assert_eq!(
            stored[0].task.assigned_to, None,
            "idle-workspace delivery must not consume the ready task"
        );
    }

    #[test]
    fn heartbeat_backstop_surfaces_missed_immediate_event_for_bounded_sweep() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-after-miss");
        append_task_event(
            "demo",
            "ready-after-miss",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();
        let missed = drain_once("demo", 0).unwrap();

        append_heartbeat_event("demo", 1, 1, None).unwrap();

        let response = wait_next("demo", missed.cursor_offset, Duration::from_millis(250)).unwrap();
        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].kind, "heartbeat");
        assert_eq!(
            response.events[0]
                .metadata
                .get("zen_eligible")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            response.events[0]
                .metadata
                .get("idle_workspaces")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn drain_labels_heartbeat_and_pane_death_without_scheduling() {
        let (_guard, _tmp) = setup_home();
        append_heartbeat_event("demo", 1, 1, None).unwrap();
        append_external_event("project=demo workspace=alpha pane_alive=false reason=signal:SIGHUP")
            .unwrap();
        append_external_event("project=demo workspace=review server_alive=false reason=exit:1")
            .unwrap();

        let response = drain_once("demo", 0).unwrap();

        let kinds = response
            .events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["heartbeat", "pane_death", "pane_death"]);
        assert_eq!(
            response.events[0]
                .metadata
                .get("zen_eligible")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            response.events[0]
                .metadata
                .get("idle_workspaces")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(response.events[1].workspace.as_deref(), Some("alpha"));
        assert_eq!(response.events[2].workspace.as_deref(), Some("review"));
    }

    #[test]
    fn cursor_path_lives_in_project_config_dir_not_claude() {
        let (_guard, _tmp) = setup_home();
        let path = cursor_path("demo").unwrap();
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("event-cursor"));
        assert_eq!(
            path.parent(),
            Some(shelbi_state::project_dir("demo").unwrap().as_path())
        );
        assert!(
            !path.components().any(|c| c.as_os_str() == ".claude"),
            "cursor must not live under .claude/: {path:?}"
        );
    }

    #[test]
    fn drain_persisted_resumes_from_persisted_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        // No override: reads 0 (no file yet), drains, and persists the new cursor.
        let first = drain_persisted("demo", None).unwrap();
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].task.as_deref(), Some("first"));
        let stored = fs::read_to_string(cursor_path("demo").unwrap()).unwrap();
        assert_eq!(stored.trim(), first.cursor_offset.to_string());

        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();

        // No override again: resumes from the persisted cursor, so the first
        // event is not replayed.
        let second = drain_persisted("demo", None).unwrap();
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].task.as_deref(), Some("second"));
    }

    #[test]
    fn drain_persisted_explicit_cursor_overrides_and_advances_persisted() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();
        // Consume the first event and persist a non-zero cursor.
        let first = drain_persisted("demo", None).unwrap();
        assert_eq!(first.events.len(), 1);

        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();

        // Explicit `--cursor 0` replays the whole log despite the persisted cursor.
        let replay = drain_persisted("demo", Some(0)).unwrap();
        assert_eq!(replay.events.len(), 2);

        // A successful override drain advances the persisted cursor too.
        let stored = fs::read_to_string(cursor_path("demo").unwrap()).unwrap();
        assert_eq!(stored.trim(), replay.cursor_offset.to_string());
    }

    #[test]
    fn read_persisted_cursor_defaults_to_zero_when_absent() {
        let (_guard, _tmp) = setup_home();
        assert_eq!(read_persisted_cursor("demo").unwrap(), 0);
    }

    fn read_cursor(project: &str) -> u64 {
        read_persisted_cursor(project).unwrap()
    }

    #[test]
    fn feed_batch_uses_the_codex_delivery_id_scheme() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        let cursor = read_cursor("demo");
        let batch = scan_feed_batch("demo", cursor).unwrap().unwrap();

        // The id is the shared core's derivation, byte-for-byte.
        let through: u64 = batch.cursor.through.parse().unwrap();
        assert_eq!(
            batch.delivery_id,
            shelbi_state::delivery_id("demo", cursor, through)
        );
        assert!(batch.delivery_id.starts_with("shelbi-event/demo/"));
        assert_eq!(batch.ack, format!("shelbi orchestrator events ack {}", batch.delivery_id));
        assert_eq!(batch.cursor.from, cursor.to_string());
    }

    #[test]
    fn feed_reads_do_not_advance_the_durable_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        let before = read_cursor("demo");
        let first = scan_feed_batch("demo", before).unwrap().unwrap();
        // Reading again from the still-unadvanced cursor re-derives the exact
        // same batch — this is the crash-between-emit-and-ack restart path.
        let replay = scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap();

        assert_eq!(read_cursor("demo"), before, "feed read must not advance the cursor");
        assert_eq!(first.delivery_id, replay.delivery_id);
        assert_eq!(first.events, replay.events);
    }

    #[test]
    fn feed_loop_exits_cleanly_when_max_lifetime_elapses() {
        let (_guard, _tmp) = setup_home();
        // A zero flag never trips the signal path; a zero lifetime is already
        // elapsed on the first iteration, so the loop returns immediately
        // without sleeping or blocking forever.
        let signal = AtomicUsize::new(0);
        let outcome = feed_loop("demo", Some(Duration::ZERO), &signal).unwrap();
        assert_eq!(outcome, FeedOutcome::Expired { after: Duration::ZERO });
    }

    #[test]
    fn feed_loop_exits_cleanly_on_termination_signal() {
        let (_guard, _tmp) = setup_home();
        // Pre-set the flag the way a real signal handler would; the loop must
        // report the signal number and stop rather than stream on.
        let signal = AtomicUsize::new(SIGTERM as usize);
        // No lifetime cap: only the signal can end this loop.
        let outcome = feed_loop("demo", None, &signal).unwrap();
        assert_eq!(outcome, FeedOutcome::Terminated { signal: SIGTERM });
    }

    #[test]
    fn follower_death_redelivers_the_unacked_batch() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        // Run a real feed loop that emits one batch, then dies on its
        // self-imposed lifetime cap without ever acking — the exact shape of a
        // follower reaped mid-flight. The short cap trips on the second poll.
        let before = read_cursor("demo");
        let signal = AtomicUsize::new(0);
        let outcome = feed_loop("demo", Some(Duration::from_millis(1)), &signal).unwrap();
        assert!(matches!(outcome, FeedOutcome::Expired { .. }));

        // Death left the cursor untouched, so the next follower re-derives the
        // identical batch — no event dropped, only a restart spent.
        assert_eq!(read_cursor("demo"), before, "a dead follower must not advance the cursor");
        let redelivered = scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap();
        assert_eq!(redelivered.cursor.from, before.to_string());
        assert_eq!(redelivered.events.len(), 1);
        assert_eq!(redelivered.events[0].task.as_deref(), Some("first"));
    }

    #[test]
    fn feed_should_scan_holds_inflight_batch_until_ack_or_timeout() {
        let now = Instant::now();
        let lease = Duration::from_secs(90);

        // Nothing in flight → emit whatever is pending.
        assert!(feed_should_scan(None, 100, now, lease));

        let inflight = InflightBatch {
            cursor: 100,
            emitted: now,
            redeliveries: 0,
        };

        // Same cursor, still inside the visibility window → hold in lock-step
        // (this is the property that keeps a batch's delivery id stable while
        // the consumer works it).
        assert!(!feed_should_scan(Some(&inflight), 100, now, lease));

        // The ack advanced the durable cursor past the batch → scan the next.
        assert!(feed_should_scan(Some(&inflight), 250, now, lease));

        // Same cursor but the visibility timeout has elapsed → redeliver rather
        // than park the claim forever. This is the branch that un-blinds a
        // follower whose consumer dropped or hung on its ack.
        assert!(feed_should_scan(Some(&inflight), 100, now + lease, lease));
    }

    #[test]
    fn feed_loop_redelivers_an_unacked_batch_instead_of_going_silent() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        // A follower whose consumer never acks. With a zero visibility timeout
        // every poll past the first redelivers, so we capture several deliveries
        // of the same still-unacked batch — the follower keeps the orchestrator
        // fed rather than silently stalling.
        //
        // Termination is delivery-count driven, NOT wall-clock driven: the emit
        // callback raises the cancel signal once it has seen enough redeliveries
        // to prove the point. A prior version bounded the loop with an 80ms
        // `max_lifetime`, which flaked on a CPU-starved CI box — under load the
        // lifetime could elapse before the loop scanned even once, yielding zero
        // deliveries and a false "held silently" failure that then poisoned the
        // shared test lock and cascaded. Counting deliveries makes the loop stop
        // exactly when its invariant is satisfied, regardless of scheduling.
        let signal = AtomicUsize::new(0);
        let mut deliveries: Vec<String> = Vec::new();
        let outcome = feed_loop_with(
            "demo",
            // No wall-clock cap: only the delivery-count signal below ends it.
            None,
            &signal,
            Duration::from_millis(5),
            Duration::ZERO,
            // No redelivery cap: this test is about redelivery happening at all,
            // not the quarantine escape hatch (covered separately).
            0,
            &|| false,
            &mut |batch| {
                deliveries.push(batch.delivery_id.clone());
                // Three deliveries is one more than the invariant needs, so the
                // redelivery is unambiguous; raise the cancel flag a real signal
                // handler would set so the loop stops on the next tick.
                if deliveries.len() >= 3 {
                    signal.store(SIGTERM as usize, Ordering::Relaxed);
                }
                Ok(())
            },
            &mut |_notice| Ok(()),
        )
        .unwrap();

        assert_eq!(outcome, FeedOutcome::Terminated { signal: SIGTERM });
        assert!(
            deliveries.len() >= 2,
            "an unacked batch must be redelivered, not held silently: {deliveries:?}"
        );
        // Never acked, so the cursor never moved and every redelivery carries
        // the identical delivery id (at-least-once, idempotent).
        assert!(deliveries.iter().all(|id| *id == deliveries[0]));
        assert_eq!(
            read_cursor("demo"),
            0,
            "redelivery must never advance the durable cursor"
        );
    }

    #[test]
    fn redelivery_folds_in_events_appended_after_an_unacked_batch() {
        let (_guard, _tmp) = setup_home();
        // A first event is delivered as a batch...
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        let cursor = read_cursor("demo");
        let first = scan_feed_batch("demo", cursor).unwrap().unwrap();
        assert_eq!(first.events.len(), 1);

        // ...but never acked. Meanwhile the stream keeps flowing: a heartbeat
        // and a zen-mode toggle land — exactly the lines the live follower
        // silently dropped while parked on an unacked batch.
        append_heartbeat_event("demo", 0, 1, None).unwrap();
        shelbi_state::append_zen_mode_event("demo", "off", "on", "user:palette").unwrap();

        // Redelivery re-scans from the SAME unadvanced cursor, so the
        // redelivered batch is a superset that finally surfaces the events the
        // stall had hidden.
        let redelivered = scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap();
        assert_eq!(
            read_cursor("demo"),
            cursor,
            "redelivery must not advance the cursor"
        );
        assert!(redelivered.events.len() >= 3);
        assert!(redelivered.events.iter().any(|e| e.kind == "heartbeat"));
        assert!(redelivered
            .events
            .iter()
            .any(|e| e.kind == "zen_mode_transition"));
    }

    #[test]
    fn feed_notice_is_distinguishable_from_a_batch() {
        // A supervisor watching stdout keys off the `feed` discriminant, which
        // a batch never carries, and a batch's `delivery_id`, which a notice
        // never carries.
        let expired = serde_json::to_value(FeedNotice {
            feed: "expired",
            project: "demo".to_string(),
            reason: "--max-lifetime of 4h reached".to_string(),
            note: FEED_RECOVERY_NOTE,
        })
        .unwrap();
        assert_eq!(expired["feed"], "expired");
        assert!(expired.get("delivery_id").is_none());
        assert!(expired["note"].as_str().unwrap().contains("re-deliver"));

        let batch = FeedBatch {
            delivery_id: "shelbi-event/demo/0-10".to_string(),
            project: "demo".to_string(),
            cursor: FeedCursor { from: "0".to_string(), through: "10".to_string() },
            ack: "shelbi orchestrator events ack shelbi-event/demo/0-10".to_string(),
            events: Vec::new(),
        };
        let batch = serde_json::to_value(batch).unwrap();
        assert!(batch.get("feed").is_none());
        assert_eq!(batch["delivery_id"], "shelbi-event/demo/0-10");
    }

    #[test]
    fn quiet_feed_emits_nothing_and_still_honors_lifetime() {
        let (_guard, _tmp) = setup_home();
        // No events at all — the feed is quiet. With the standalone keepalive
        // retired, a quiet feed writes no line at all; it must still exit
        // cleanly on its lifetime cap rather than panicking or blocking forever
        // on a silent stream. Liveness now rides the unified stream's heartbeat,
        // not a ping this loop emits.
        let before = read_cursor("demo");
        let signal = AtomicUsize::new(0);
        let outcome = feed_loop("demo", Some(Duration::from_millis(1)), &signal).unwrap();
        assert!(matches!(outcome, FeedOutcome::Expired { .. }));
        // A quiet feed never advances the durable cursor.
        assert_eq!(
            read_cursor("demo"),
            before,
            "a quiet feed must not advance the cursor"
        );
    }

    #[test]
    fn format_duration_humanizes_common_spans() {
        assert_eq!(format_duration(Duration::ZERO), "0s");
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_duration(Duration::from_secs(4 * 3600)), "4h");
        assert_eq!(format_duration(Duration::from_secs(3600 + 60)), "1h1m");
    }

    #[test]
    fn acked_batch_is_never_redelivered_across_restart() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        // Emit the batch (crash-safe: cursor still at the batch start).
        let cursor = read_cursor("demo");
        let batch = scan_feed_batch("demo", cursor).unwrap().unwrap();

        // Restart before ack: same batch, same delivery id.
        assert_eq!(
            scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap().delivery_id,
            batch.delivery_id
        );

        // Ack advances the durable cursor past the batch.
        ack_delivery("demo", &batch.delivery_id).unwrap();
        assert!(read_cursor("demo") > cursor);

        // Restart after ack: the batch is gone, nothing pending.
        assert!(scan_feed_batch("demo", read_cursor("demo")).unwrap().is_none());

        // A fresh event produces a distinct, contiguous batch.
        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();
        let next = scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap();
        assert_ne!(next.delivery_id, batch.delivery_id);
        assert_eq!(next.cursor.from, batch.cursor.through);
        assert_eq!(next.events.len(), 1);
        assert_eq!(next.events[0].task.as_deref(), Some("second"));
    }

    #[test]
    fn ack_is_idempotent_and_refuses_to_skip_a_gap() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();
        let batch = scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap();
        let through: u64 = batch.cursor.through.parse().unwrap();

        ack_delivery("demo", &batch.delivery_id).unwrap();
        assert_eq!(read_cursor("demo"), through);

        // Re-acking the same (now behind-cursor) batch is a harmless no-op.
        ack_delivery("demo", &batch.delivery_id).unwrap();
        assert_eq!(read_cursor("demo"), through, "duplicate ack must not rewind");

        // Acking a batch that starts beyond the cursor would skip events.
        let gap = shelbi_state::delivery_id("demo", through + 100, through + 200);
        let error = ack_delivery("demo", &gap).unwrap_err();
        assert!(error.to_string().contains("skip unacknowledged events"));
        assert_eq!(read_cursor("demo"), through, "rejected ack must not advance");

        // Foreign-project delivery id is refused.
        let foreign = shelbi_state::delivery_id("other", 0, 10);
        assert!(ack_delivery("demo", &foreign).unwrap_err().to_string().contains("scoped to project"));
    }

    #[test]
    fn parse_delivery_id_roundtrips_and_rejects_garbage() {
        let id = shelbi_state::delivery_id("demo", 12, 345);
        assert_eq!(parse_delivery_id(&id).unwrap(), ("demo".to_string(), 12, 345));

        assert!(parse_delivery_id("shelbi-event/demo/12").is_err());
        assert!(parse_delivery_id("not-a-delivery-id").is_err());
        assert!(parse_delivery_id("shelbi-event//0-1").is_err());
        // Range that ends before it starts is rejected.
        assert!(parse_delivery_id("shelbi-event/demo/10-5").is_err());
    }

    #[test]
    fn feed_batch_filters_by_project_like_drain() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event("other", "beta", None, WorkspaceState::Working).unwrap();

        let batch = scan_feed_batch("demo", read_cursor("demo")).unwrap().unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].workspace.as_deref(), Some("alpha"));
        // The cross-project line is consumed by the cursor range but not
        // surfaced — same scoping the ack must advance across.
        ack_delivery("demo", &batch.delivery_id).unwrap();
        assert!(scan_feed_batch("demo", read_cursor("demo")).unwrap().is_none());
    }

    #[test]
    fn pane_death_fact_is_delivered_without_supervision_restart_action() {
        let (_guard, _tmp) = setup_home();
        append_external_event("project=demo workspace=alpha pane_alive=false reason=exit:1")
            .unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].kind, "pane_death");
        assert_eq!(response.events[0].workspace.as_deref(), Some("alpha"));
        assert_eq!(response.events[0].reason.as_deref(), Some("exit:1"));
        assert!(
            !response.events[0].metadata.contains_key("supervision"),
            "pane death delivery must not be conflated with auto-restart"
        );
    }

    /// A follower whose registry slot was taken over by a newer one stands down
    /// on its next tick with a clean `Superseded` outcome — the mechanism that
    /// keeps exactly one consuming follower live per project.
    #[test]
    fn feed_loop_stands_down_when_superseded_by_a_newer_follower() {
        let (_guard, _tmp) = setup_home();
        let signal = AtomicUsize::new(0);
        // No lifetime cap and no signal: only a supersede can stop this loop,
        // so the clean exit proves the supersede path drives it.
        let outcome = feed_loop_with(
            "demo",
            None,
            &signal,
            Duration::from_millis(5),
            Duration::ZERO,
            FEED_MAX_REDELIVERIES,
            &|| true,
            &mut |_batch| Ok(()),
            &mut |_notice| Ok(()),
        )
        .unwrap();
        assert!(matches!(outcome, FeedOutcome::Superseded));
    }

    /// `feed_superseded` is the registry check the live loop runs: the recorded
    /// owner keeps serving, any other PID (a leaked/duplicate follower) is told
    /// to stand down, and an unclaimed project never falsely supersedes.
    #[test]
    fn feed_superseded_tracks_the_registry_owner() {
        let (_guard, _tmp) = setup_home();
        // Nobody has claimed the slot: an ad-hoc drain must run normally.
        assert!(!feed_superseded("demo", std::process::id()));

        let owner = shelbi_state::claim_event_follower("demo").unwrap();
        assert!(!feed_superseded("demo", owner));
        assert!(feed_superseded("demo", owner.wrapping_add(1)));
    }

    /// Repro for the starvation bug: two would-be consumers, a burst of events,
    /// and the single-consumer owner still observes every event in one batch —
    /// no false "no batch" caused by a co-running consumer. The registry is what
    /// guarantees only the owner drains; a second follower is superseded rather
    /// than left to claim batches the owner then misses.
    #[test]
    fn single_owner_sees_every_event_in_a_burst_despite_a_second_follower() {
        let (_guard, _tmp) = setup_home();

        // The primary claims the sole consumer slot.
        let primary = shelbi_state::claim_event_follower("demo").unwrap();

        // A burst lands while a second (now-stale) follower notionally co-runs.
        for i in 0..6 {
            append_workspace_event("demo", &format!("w{i}"), None, WorkspaceState::Working)
                .unwrap();
        }

        // The owner is never superseded, so its drain from the durable cursor
        // surfaces the whole burst as one batch — every event, within one pass.
        assert!(!feed_superseded("demo", primary));
        let batch = scan_feed_batch("demo", 0).unwrap().unwrap();
        assert_eq!(
            batch.events.len(),
            6,
            "the single owner must observe every burst event, not a fraction"
        );

        // The second follower, holding any other PID, is told to stand down —
        // so it can never claim a batch out from under the primary.
        assert!(feed_superseded("demo", primary.wrapping_add(1)));
    }

    /// Path to the hub-global `events.log.lock`, the lock a slow reader/writer
    /// holds while the feed wants to scan.
    fn events_log_lock_path() -> std::path::PathBuf {
        shelbi_state::events_log_path()
            .unwrap()
            .with_extension("log.lock")
    }

    /// Holds the hub-global `events.log` lock for as long as the guard lives,
    /// exactly as a slow reader/writer in another process would. `flock` locks
    /// are per open-descriptor, so this excludes the feed's own acquire even
    /// within one process.
    struct HeldEventsLogLock {
        _file: std::fs::File,
    }

    fn hold_events_log_lock() -> HeldEventsLogLock {
        use std::os::unix::io::AsRawFd;
        let path = events_log_lock_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(rc, 0, "test could not grab the events.log lock");
        HeldEventsLogLock { _file: file }
    }

    /// Acceptance: `--max-lifetime` must be honored even while another holder has
    /// the hub-global lock. Before the fix, the feed's blocking `flock(LOCK_EX)`
    /// parked past the deadline (only SIGKILL could break it); now it returns
    /// `Expired` within a small multiple of the cap.
    #[test]
    fn feed_honors_max_lifetime_while_events_log_lock_is_held() {
        let (_guard, _tmp) = setup_home();
        // Establish the index and the cursor file so the cursor read stays on
        // its own uncontended lock — only the scan contends on the held lock.
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        let _ = read_cursor("demo");

        let held = hold_events_log_lock();
        let signal = AtomicUsize::new(0);
        let lifetime = Duration::from_millis(200);
        let start = Instant::now();
        let outcome = feed_loop("demo", Some(lifetime), &signal).unwrap();
        let elapsed = start.elapsed();
        drop(held);

        assert!(
            matches!(outcome, FeedOutcome::Expired { .. }),
            "expected a timely Expired under lock contention, got {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "feed hung past its deadline while the lock was held: {elapsed:?}"
        );
    }

    /// Acceptance: SIGTERM must end a feed that is blocked on lock acquisition,
    /// with no reliance on SIGKILL. The signal flag is threaded into the
    /// deadline-aware acquire as the cancel predicate, so a blocked scan drops
    /// out within a poll interval of the signal landing.
    #[test]
    fn sigterm_ends_a_feed_blocked_on_lock_acquisition() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        let _ = read_cursor("demo");

        let held = hold_events_log_lock();
        let signal = AtomicUsize::new(0);
        let start = Instant::now();
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                // Give the loop time to enter its blocked acquire, then signal.
                std::thread::sleep(Duration::from_millis(100));
                signal.store(SIGTERM as usize, Ordering::Relaxed);
            });
            feed_loop_with(
                "demo",
                None, // no lifetime cap: only the signal can end this loop
                &signal,
                Duration::from_millis(5),
                FEED_VISIBILITY_TIMEOUT,
                FEED_MAX_REDELIVERIES,
                &|| false,
                &mut |_batch| Ok(()),
                &mut |_notice| Ok(()),
            )
            .unwrap()
        });
        let elapsed = start.elapsed();
        drop(held);

        assert_eq!(outcome, FeedOutcome::Terminated { signal: SIGTERM });
        assert!(
            elapsed < Duration::from_secs(2),
            "SIGTERM did not promptly end a lock-blocked feed: {elapsed:?}"
        );
    }

    /// Overwrite the index so the current generation begins at `current_base`,
    /// stranding any smaller cursor past the retained window — the poison-cursor
    /// state the prbadge drain died on.
    fn strand_cursor_before_retained_window(current_base: u64) {
        let path = shelbi_state::events_log_path().unwrap();
        let index_path = path.with_extension("log.index.json");
        fs::write(
            &index_path,
            format!(r#"{{"version":1,"previous_base":null,"current_base":{current_base}}}"#),
        )
        .unwrap();
    }

    /// Acceptance: a drain whose cursor predates the earliest retained generation
    /// resynchronizes (fast-forwards to the current base) and keeps making
    /// progress, emitting a visible "resynchronized" notice — instead of exiting
    /// with a silent Err and re-dying on every relaunch (the prbadge wedge).
    #[test]
    fn poison_cursor_resynchronizes_and_makes_progress_instead_of_dying() {
        let (_guard, _tmp) = setup_home();
        // A real event lands in the current generation...
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        let _ = read_cursor("demo");
        // ...but the retained window has moved far ahead of the frozen cursor.
        strand_cursor_before_retained_window(1000);
        shelbi_state::write_event_cursor("demo", 0).unwrap();

        let signal = AtomicUsize::new(0);
        let mut delivered_from: Vec<String> = Vec::new();
        let mut notices: Vec<String> = Vec::new();
        let outcome = feed_loop_with(
            "demo",
            Some(Duration::from_millis(150)),
            &signal,
            Duration::from_millis(5),
            FEED_VISIBILITY_TIMEOUT,
            FEED_MAX_REDELIVERIES,
            &|| false,
            &mut |batch| {
                delivered_from.push(batch.cursor.from.clone());
                Ok(())
            },
            &mut |notice| {
                notices.push(notice.feed.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert!(matches!(outcome, FeedOutcome::Expired { .. }), "{outcome:?}");
        assert!(
            notices.iter().any(|feed| feed == "resynchronized"),
            "a self-healed poison cursor must emit a visible resync notice: {notices:?}"
        );
        assert_eq!(
            read_cursor("demo"),
            1000,
            "poison cursor must fast-forward to the earliest retained position"
        );
        assert!(
            delivered_from.iter().any(|from| from == "1000"),
            "after resync the feed must deliver from the healed cursor: {delivered_from:?}"
        );

        // A relaunched drain resumes from the healed cursor and progresses,
        // rather than re-reading the wedged cursor and re-dying.
        let relaunch = scan_feed_batch("demo", read_cursor("demo")).unwrap();
        assert!(
            relaunch.is_some(),
            "a relaunch on the healed cursor must make progress, not re-die"
        );
    }

    /// A poison cursor surfaces as data (`FeedRead::CursorExpired`) rather than a
    /// hard error, which is what lets the loop resync instead of dying.
    #[test]
    fn expired_cursor_reads_as_data_not_a_fatal_error() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        let _ = read_cursor("demo");
        strand_cursor_before_retained_window(1000);

        match feed_scan("demo", 0, None, &|| false).unwrap() {
            FeedScan::CursorExpired {
                earliest,
                current_base,
            } => {
                assert_eq!(earliest, 1000);
                assert_eq!(current_base, 1000);
            }
            other => panic!("expected CursorExpired, got a different scan outcome: {other:?}"),
        }
    }

    /// Acceptance (poison-batch escape): a batch redelivered past the cap without
    /// an ack is quarantined — the cursor advances past it so one un-ackable
    /// batch cannot wedge the stream forever, and a loud notice records it.
    #[test]
    fn unackable_batch_is_quarantined_after_the_redelivery_cap() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        let before = read_cursor("demo");

        let signal = AtomicUsize::new(0);
        let mut notices: Vec<String> = Vec::new();
        let outcome = feed_loop_with(
            "demo",
            Some(Duration::from_millis(150)),
            &signal,
            Duration::from_millis(2),
            // Zero visibility timeout: every tick past the first is a redelivery.
            Duration::ZERO,
            2, // small cap so the quarantine trips quickly
            &|| false,
            &mut |_batch| Ok(()),
            &mut |notice| {
                notices.push(notice.feed.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert!(matches!(outcome, FeedOutcome::Expired { .. }), "{outcome:?}");
        assert!(
            notices.iter().any(|feed| feed == "quarantined"),
            "an un-ackable batch must be quarantined past the cap: {notices:?}"
        );
        assert!(
            read_cursor("demo") > before,
            "quarantine must advance the durable cursor past the wedged batch"
        );
    }
}
