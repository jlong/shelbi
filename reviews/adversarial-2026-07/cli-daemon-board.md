# Adversarial review: hub daemon + board CLI (shelbi-cli)

Reviewed:

- `crates/shelbi-cli/src/commands/daemon.rs` (1500 lines)
- `crates/shelbi-cli/src/commands/task.rs` (1240 lines)
- `crates/shelbi-cli/src/commands/zen.rs` (623 lines)
- `crates/shelbi-cli/src/commands/status.rs` (568 lines)
- `crates/shelbi-cli/src/commands/workspace.rs` (561 lines)
- `crates/shelbi-cli/src/main.rs` (626 lines)

Supporting code read for context (findings are reported against scope files only):
`shelbi-state/src/workspace_status.rs` (event append + socket emit),
`shelbi-state/src/lib.rs` (task persistence), `shelbi-state/src/ssh_control.rs`
(PID file / CM cleanup), `shelbi-core/src/model.rs` (`Column`, id validation),
`shelbi-core/src/workflow.rs`, `shelbi-orchestrator/src/workspace.rs`,
`shelbi-cli/src/commands/events.rs` (`parse_duration`).

| #   | Finding                                                                                             | Severity | Confidence  | Category         |
|-----|-----------------------------------------------------------------------------------------------------|----------|-------------|------------------|
| F1  | Task ids are not validated on read paths — `task rm/show/move/...` accept `../` path traversal       | high     | certain     | hardening        |
| F2  | Daemon accepts unbounded socket lines — memory exhaustion + oversized events.log records             | medium   | certain     | hardening        |
| F3  | Fire-and-forget socket protocol + no shutdown drain silently drops accepted-but-unread events        | medium   | certain     | failure-scenario |
| F4  | `prepare_socket` TOCTOU: two daemons racing can unlink each other's live socket                      | medium   | certain     | failure-scenario |
| F5  | `task move --to` can never reach custom workflow statuses, yet the error message advertises them     | medium   | certain     | bug              |
| F6  | Board mutations are unlocked read-modify-write — concurrent invocations corrupt priorities / lose tasks | medium | likely      | failure-scenario |
| F7  | `task start` launches the workspace pane before persisting the column change                          | medium   | likely      | failure-scenario |
| F8  | `main.rs` `session` positional is parsed but never read — typo'd subcommands silently launch the TUI  | medium   | certain     | bug              |
| F9  | events.log grows unbounded and `status` reads the whole file to scan the last 200 lines              | medium   | certain     | assumption       |
| F10 | Signal listener consumes only the first signal; a wedged shutdown then ignores SIGTERM forever        | low      | certain     | failure-scenario |
| F11 | Socket permissions: chmod-after-bind window + unconditional `chmod 700` of the socket's parent dir    | low      | certain     | hardening        |
| F12 | Inconsistent mutex-poison policy: pushes/acks error forever, reaper recovers                          | low      | certain     | bug              |
| F13 | Pending-ack map is unbounded; hostile `message-pushed` spam amplifies into events.log                 | low      | likely      | hardening        |
| F14 | `task edit` breaks when `$EDITOR` contains arguments                                                  | low      | certain     | bug              |
| F15 | `zen dry-run --interval 0` busy-loops; `format_duration` renders 0s as `0d`                           | low      | certain     | bug              |
| F16 | `zen dry-run` tick error kills the whole preview loop despite "best-effort" contract                  | low      | certain     | bug              |
| F17 | One corrupt `status.yaml` kills the entire `workspace status` table                                   | low      | certain     | failure-scenario |
| F18 | `release_workspace_tasks` double-write (unassign, then move) is not crash-safe                        | low      | certain     | failure-scenario |
| F19 | `move_to` cuts the branch before the move; a failed move leaves the side-effect behind                | low      | certain     | failure-scenario |
| F20 | Crash-recovery "recent" heuristic is line-count based, not time based                                 | low      | certain     | assumption       |
| F21 | Daemon echoes raw client bytes to stderr — terminal-escape injection into daemon logs                 | low      | certain     | hardening        |
| F22 | `daemon.rs` mixes socket server and OS-supervisor plumbing in one 1500-line file                      | low      | certain     | simplification   |

---

## F1: Task ids are not validated on read paths — path traversal in `task rm/show/move/edit/...`

- **Where:** crates/shelbi-cli/src/commands/task.rs:186-199 (validation), task.rs:335, task.rs:753; crates/shelbi-state/src/lib.rs:1097
- **Category:** hardening
- **Severity / Confidence:** high / certain
- **Evidence:** `validate_task_id` is called in exactly one place — `add()`, and only when the user passes `--id`:

  ```rust
  let id = match args.id {
      Some(id) => {
          validate_task_id(&id).map_err(|e| anyhow!(e))?;
  ```

  Every other subcommand hands the raw CLI string straight to the path join in `shelbi_state::task_path`:

  ```rust
  pub fn task_path(project: &str, id: &str) -> Result<PathBuf> {
      Ok(tasks_dir(project)?.join(format!("{id}.md")))
  }
  ```

  `validate_agent_id` (which `validate_task_id` wraps) restricts ids to `[A-Za-z0-9_-]`, so `add` is safe — but `show`, `edit`, `rm`, `move`, `assign`, `unassign`, `depends`, `start`, and `prio` never call it. An id containing `/` or `..` escapes `tasks_dir`.
- **Failure scenario:** `shelbi task rm '../../other-project/tasks/foo'` loads and deletes another project's task file (`rm` at task.rs:753 does `load_task` → `delete_task`, both resolving through the traversing path), then renumbers the *wrong* project's column. `shelbi task show '../../../<anything>'` dumps any `.md` file the user can read, e.g. a private note outside the project tree. `move`/`assign` follow the same path and will happily rewrite a foreign task file. All of this is reachable from the orchestrator agent, which constructs `shelbi task ...` invocations from LLM output — the id is not a trusted string.
- **Recommendation:** Call `validate_task_id` (or at minimum reject `/`, `\`, and `..`) at the top of `run()` in task.rs for every subcommand carrying an id, and for each `--depends-on` value. Belt-and-braces: make `task_path` itself refuse ids that don't pass `validate_task_id`.
- **Effort:** S

## F2: Daemon accepts unbounded socket lines — memory exhaustion and oversized events.log records

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:413-432, daemon.rs:486-505
- **Category:** hardening
- **Severity / Confidence:** medium / certain
- **Evidence:** `handle_client` reads with `BufReader::lines()` and no length cap:

  ```rust
  let reader = BufReader::new(stream);
  for line in reader.lines() {
  ```

  `BufRead::lines` accumulates into a `String` until it sees `\n` — a client that streams gigabytes without a newline grows that allocation without bound. `handle_event` then checks only for emptiness and embedded newlines before appending:

  ```rust
  if body.contains('\n') || body.contains('\r') {
      return Err(anyhow!("event `line` may not contain newlines"));
  }
  shelbi_state::append_external_event(body).map_err(|e| anyhow!(e))?;
  ```

  There is no maximum message size anywhere on the socket path. `append_event_line` (shelbi-state/workspace_status.rs:666) explicitly relies on writes ≤ PIPE_BUF being atomic ("POSIX guarantees that writes <= PIPE_BUF (4096B) under O_APPEND are atomic"); a multi-megabyte `line` from a client blows past that, so a concurrent degraded-mode appender (`emit_event_body`'s fallback) can tear records.
- **Failure scenario:** A buggy worker hook that accidentally pipes a build log into `{"verb":"event","line":"<10MB>"}` (or a hostile same-user process) makes the daemon allocate the full payload, then writes a 10MB single line into events.log — every tail consumer (`shelbi events tail`, the activity feed, `has_recent_crash_event`'s full-file read, F9) now chokes on it, and the ≤PIPE_BUF atomicity assumption for concurrent appenders is void. Worst case, a newline-free infinite stream OOMs the daemon; launchd restarts it and the client can immediately repeat.
- **Recommendation:** Read with `BufRead::read_line` into a reused buffer via `take(MAX_FRAME)` (e.g. 64KB), reject over-limit frames with a logged error and connection close, and enforce a max `line` length in `handle_event` (the clarification path already truncates to 120 chars — events deserve the same discipline).
- **Effort:** S

## F3: Fire-and-forget socket protocol + no shutdown drain silently drops accepted-but-unread events

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:237-263 (accept loop / shutdown), daemon.rs:244-248 (detached handler threads)
- **Category:** failure-scenario
- **Severity / Confidence:** medium / certain
- **Evidence:** The module header (daemon.rs:54-55) promises "the safety net is 'no silent loss'". But the accept loop spawns detached handlers and never joins them:

  ```rust
  Ok(stream) => {
      let daemon = daemon.clone();
      thread::spawn(move || handle_client(stream, &daemon));
  }
  ```

  On shutdown the loop breaks, removes the socket, and `run_foreground` returns — the process exits while handler threads may be mid-`dispatch`. Meanwhile the client side (shelbi-state/workspace_status.rs:641-657, `try_emit_via_socket`) treats a successful `write_all` + `shutdown(Write)` as delivery — it never waits for any acknowledgement:

  ```rust
  stream.write_all(&payload)?;
  let _ = stream.shutdown(std::net::Shutdown::Write);
  Ok(())
  ```

  Additionally, a connection that lands between the signal (stop flag set) and the `break` is `accept()`ed and immediately dropped by the `if stop.load(...) { break; }` check at daemon.rs:241 — its payload is never read.
- **Failure scenario:** Worker pane dies → wrapper emits `pane_alive=false` → `write_all` succeeds into the kernel socket buffer → SIGTERM lands on the daemon (e.g. `shelbi daemon restart` picking up a new binary) → daemon exits before the handler thread reads/dispatches the line. The kernel discards the buffered bytes. The client saw `Ok`, so the degraded-mode file fallback never fires. The `pane_alive=false` event is silently gone and the orchestrator never reacts to the dead pane — exactly the loss class the doc says can't happen. Restarts are routine here (systemd `Restart=always`, `RestartSec=1s`), so the window is hit in practice.
- **Failure scenario (mid-message):** SIGTERM while a handler thread is between `map.insert` and returning is harmless, but a handler mid-`append_external_event` when `main` returns can be killed between `open` and `write_all` — no torn line (single `write_all`) but the event is dropped after the client got success.
- **Recommendation:** Track handler `JoinHandle`s (or a live-connection counter) and drain them with a short deadline before `run_foreground` returns; keep reading already-accepted connections during shutdown instead of dropping them. For real delivery semantics, have the daemon write a one-byte ack and make `try_emit_via_socket` read it before returning `Ok` — the fallback path then covers daemon-death correctly.
- **Effort:** M

## F4: `prepare_socket` TOCTOU — two daemons racing can unlink each other's live socket

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:359-389, daemon.rs:394-408
- **Category:** failure-scenario
- **Severity / Confidence:** medium / certain (sequence traced; window is small but real)
- **Evidence:**

  ```rust
  match UnixStream::connect(sock) {
      Ok(_) => { return Err(anyhow!("another shelbi daemon is already listening ...")); }
      Err(_) => {
          fs::remove_file(sock).with_context(...)?;
      }
  }
  ```

  The check (connect fails) and the act (remove, then `bind` back in `run_foreground`) are not atomic. Sequence: daemons A and B start concurrently against a stale socket. Both `connect` → both fail → A removes + binds; B then removes **A's freshly bound live socket** and binds its own. `bind()` never returns EADDRINUSE for B because B just unlinked the path.
- **Failure scenario:** A keeps running with an orphaned listener: existing connections work, but every new `connect` reaches B. Worse, A's shutdown path depends on a self-connect to wake `accept()` (daemon.rs:404 `let _ = UnixStream::connect(&sock);`) — that poke now connects to *B's* socket, so A's accept loop never wakes and A ignores SIGTERM indefinitely (compounded by F10). On A's eventual kill, its exit path `fs::remove_file(&sock)` (daemon.rs:255) deletes **B's** live socket, knocking the healthy daemon off the path too. This is precisely the "two daemons racing to bind" scenario, and launchd/systemd auto-restart makes concurrent starts plausible (manual `shelbi daemon` + supervisor respawn).
- **Recommendation:** Take an exclusive advisory lock (e.g. `flock` on `~/.shelbi/hub.sock.lock`) for the daemon's lifetime before touching the socket path; only the lock holder may unlink/bind/remove. That also makes the exit-path `remove_file` safe.
- **Effort:** M

## F5: `task move --to` can never reach custom workflow statuses, yet the rejection message advertises them as valid

- **Where:** crates/shelbi-cli/src/commands/task.rs:408-420; crates/shelbi-core/src/model.rs:796-809
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:** The destination is parsed into the fixed five-variant `Column` enum *before* the workflow membership check:

  ```rust
  let column = Column::from_str(to).map_err(|e| anyhow!(e))?;
  let workflow = resolve_task_workflow(project, &tf.task)?;
  if !workflow_contains_column(&workflow, column) {
      let valid: Vec<String> = workflow.statuses.iter().map(|s| s.id.clone()).collect();
      bail!("`{to}` is not a status in workflow `{}` (valid: {})", ...);
  ```

  `Column::from_str` accepts only `backlog/todo/in_progress/review/done` (plus aliases). A workflow that declares a custom status — say `id: qa` — is loadable and listable, but `shelbi task move x --to qa` dies earlier with `unknown column: qa`. Meanwhile, when a move to a *built-in* status is rejected, the `(valid: …)` list is built from `workflow.statuses` and can include exactly those unreachable custom ids — the error message tells the user to type something the parser will refuse.
- **Failure scenario:** Project authors a QA workflow per `Plans/workflows.md`; orchestrator or user runs `task move fix-1 --to review` on a workflow without `review` → error says `(valid: backlog, todo, in-progress, qa, done)` → user retries `--to qa` → `unknown column: qa`. Custom workflows are effectively half-supported by the board CLI; the only discoverable path out is editing frontmatter by hand.
- **Also noteworthy:** the membership check normalizes to lowercase alphanumerics (`norm_status_id`, task.rs:480-485), so two distinct workflow statuses that normalize identically (`in-progress` / `in_progress` / `InProgress`) are indistinguishable — currently benign but a latent aliasing trap for workflow authors.
- **Recommendation:** Either resolve `--to` against the task's workflow status ids first and map to `Column` (erroring clearly when a status has no `Column` backing — "status `qa` isn't supported by the board yet"), or filter the `(valid: …)` list to statuses `Column::from_str` can actually parse. Document the constraint either way.
- **Effort:** M

## F6: Board mutations are unlocked read-modify-write — concurrent invocations corrupt priorities and can silently lose a new task

- **Where:** crates/shelbi-cli/src/commands/task.rs:199, task.rs:205-207, task.rs:767-789; crates/shelbi-state/src/lib.rs:1406-1466
- **Category:** failure-scenario
- **Severity / Confidence:** medium / likely (interleavings traced; requires concurrent invocation, which the design invites)
- **Evidence:** Every board operation is load → compute → save with no inter-process lock. `add()`:

  ```rust
  None => generate_unique_id(project, &args.title)?,
  ...
  let priority = shelbi_state::list_column(project, column)...len() as u32;
  ```

  `generate_unique_id` probes `tasks.join(format!("{candidate}.md")).exists()` in a loop, then the caller saves via `atomic_write` (temp+rename) — atomic per file, but two processes that both find `fix-login.md` absent both settle on `fix-login` and the second **rename clobbers the first task entirely** (title, description, deps — gone, exit 0 on both sides). Likewise `move_task` computes `new_priority = list_column(...).len()` and `renumber_column` rewrites peers one file at a time (lib.rs:1447-1459) — two concurrent moves interleave into duplicate or gapped priorities, violating the module contract at task.rs:8-10 ("Priorities within a column are contiguous integers 0..N ... callers can treat `priority` as a stable position index").
- **Failure scenario:** The orchestrator auto-dispatches (`task move --to in_progress`) at the same moment the user drags a card in the TUI or runs `task add`. Both read the same column snapshot; the results interleave; the board ends with two priority-3 cards or a lost task. The workspace poller and Zen loop make concurrent invocation the normal case, not the exception. The system partially self-heals (next `renumber_column` restamps 0..N in current-priority order) but lost `add`s do not come back.
- **Recommendation:** Take a per-project advisory lock (flock on `<project>/tasks/.lock`) around mutating operations in shelbi-state; in `add`, create the task file with `OpenOptions::new().create_new(true)` so an id collision is an error instead of a silent overwrite.
- **Effort:** M

## F7: `task start` launches the workspace pane before persisting the column change

- **Where:** crates/shelbi-cli/src/commands/task.rs:683-710
- **Category:** failure-scenario
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  let addr = shelbi_orchestrator::workspace::start_workspace_on_task(...)?;
  // Persist task state. Move to in_progress before saving so the
  // assigned_to/branch land alongside the column change in a single write.
  ...
  shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
  ```

  The pane is spawned (tmux window created, agent runner launched with the task prompt) *before* `save_task` moves the card to `in_progress`. If `save_task` or the earlier `list_column` fails (disk error, F6 interference, process killed), the agent is already running against a task the board still shows in `todo`.
- **Failure scenario:** Crash between spawn and save → board shows `todo`, unassigned; a second `task start` (or the orchestrator's auto-dispatch, which selects from `todo`) happily dispatches the same task to another workspace — the guard at task.rs:635-647 only checks the `in_progress` column, which the first start never reached. Two agents now work the same branch. Also note the guard itself is check-then-act: two simultaneous `task start` invocations for different tasks on the same workspace both pass the conflict check before either saves.
- **Recommendation:** Persist the in_progress move (with `assigned_to`) *before* spawning, and roll it back on spawn failure — a card stuck in `in_progress` pointing at a never-started pane is visible and recoverable (`workspace stop` releases it), whereas a running agent invisible to the board is not.
- **Effort:** S

## F8: `main.rs` `session` positional is parsed but never read — typo'd subcommands silently launch the TUI

- **Where:** crates/shelbi-cli/src/main.rs:28-31, main.rs:304; crates/shelbi-cli/src/commands/mod.rs:63-67
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  /// Session (workspace) to load — only used when no subcommand is given.
  /// Defaults to $SHELBI_SESSION or "default".
  #[arg(env = "SHELBI_SESSION")]
  session: Option<String>,
  ```

  `cli.session` is never referenced again anywhere in main.rs — `None => default_entry(cli.project.clone())` ignores it. The helper that would consume it is literally named dead: `commands::mod.rs:63 pub fn _resolve_session(...)` (leading underscore, zero callers).
- **Failure scenario:** Any mistyped subcommand is swallowed by the positional: `shelbi statsu`, `shelbi tsk list` → clap parses `statsu` as `session`, no error, and `default_entry` boots the project TUI or first-run wizard. The user gets a full-screen TUI instead of "unrecognized subcommand", and in a script the command "succeeds" while doing something completely different. Free-form values also flow in via `$SHELBI_SESSION` with the same non-effect.
- **Recommendation:** Either wire the value through (`default_entry(cli.project, cli.session)`) if session selection is still planned, or delete the positional and `_resolve_session` so unknown tokens fail parsing. If it must stay for compatibility, at least warn when it's set and ignored.
- **Effort:** S

## F9: events.log grows unbounded and `status` reads the whole file to scan the last 200 lines

- **Where:** crates/shelbi-cli/src/commands/status.rs:339-363, status.rs:46
- **Category:** assumption
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  let text = match fs::read_to_string(&path) { ... };
  for line in text.lines().rev().take(CRASH_EVENT_SCAN_LINES) {
  ```

  The entire log is materialized in memory to look at its final 200 lines. Nothing in the reviewed surface (daemon append path, `append_event_line`, any CLI command) rotates, truncates, or caps `~/.shelbi/events.log` — the daemon docs describe it as "the durable record", and heartbeats (`append_heartbeat_event`) are emitted on a cadence, so the file grows monotonically for the life of the install.
- **Failure scenario:** After months of heartbeat + task churn (or one F2-style oversized line), events.log is hundreds of MB. `shelbi status` — the orchestrator's *bootstrap* call, run on every agent start — must read all of it into a `String` before answering, turning a "read-only, side-effect free" summary into a multi-second, memory-spiking call. Same cost hits `--full`, which calls `zen_snapshot` too.
- **Recommendation:** Read the tail only: seek to `len - 64KB` and scan forward (a fixed-size tail read covers 200 lines comfortably). Separately, give the daemon (the natural single owner) a size-based rotation of events.log, or document an external logrotate expectation.
- **Effort:** S

## F10: Signal listener consumes only the first signal; a wedged shutdown then ignores SIGTERM forever

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:394-408
- **Category:** failure-scenario
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  thread::spawn(move || {
      if let Some(sig) = signals.forever().next() {
          eprintln!("shelbi daemon: received signal {sig}, shutting down");
          stop.store(true, Ordering::SeqCst);
          let _ = UnixStream::connect(&sock);
      }
  });
  ```

  `.next()` takes exactly one signal; the thread then exits and the `Signals` iterator is dropped. `signal_hook` keeps the handlers registered but nobody consumes further deliveries, so a second SIGTERM/SIGINT does nothing (it doesn't kill the process either, since the default disposition was replaced).
- **Failure scenario:** The wake-up self-connect fails to unblock `accept()` — e.g. the socket path was replaced by another daemon (F4) or already unlinked. The stop flag is set but the main thread stays parked in `accept()`. The operator's second Ctrl-C / `kill` is swallowed silently; only SIGKILL works, which skips the socket/PID-file cleanup at daemon.rs:255-261 and leaves exactly the stale state the cleanup exists to prevent.
- **Recommendation:** Loop `for sig in signals.forever()` and escalate: first signal → graceful stop; second → `std::process::exit(1)`. That is the conventional double-signal contract and costs three lines.
- **Effort:** S

## F11: Socket permissions — chmod-after-bind window, and `prepare_socket` chmods the socket's parent dir unconditionally

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:212-215, daemon.rs:360-367
- **Category:** hardening
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let listener = UnixListener::bind(&sock)...;
  fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))...;
  ```

  The socket exists with umask-derived permissions between `bind` and the chmod. In the default layout that's fine (`~/.shelbi` is forced to 0700 two lines up), but the path is user-controllable via `$SHELBI_HUB_SOCK` (shelbi-state/workspace_status.rs:220-225). And for *any* override, `prepare_socket` does:

  ```rust
  fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
      .with_context(|| format!("chmod 700 {}", parent.display()))?;
  ```

  — an unconditional chmod-700 of whatever directory contains the socket.
- **Failure scenario:** `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock` (the same default the code itself uses for the *remote* end, `remote_hub_socket_path` → `/tmp/shelbi-hub.sock`) → `set_permissions("/tmp", 0700)` fails with EPERM and the daemon refuses to start with a baffling "chmod 700 /tmp" error. If the parent is a user-owned shared directory instead, shelbi silently locks it to 0700 and breaks every other consumer of that directory. Answering the seed question directly: with the default path, another local user cannot connect (parent 0700 + socket 0600); a permissive umask plus an override into a world-readable dir is the only exposure, bounded by the chmod race window.
- **Recommendation:** Only chmod the parent when the daemon created it (or when it's under `shelbi_home()`); for the socket itself, set the umask around `bind` (or bind in a private temp dir and rename) instead of chmod-after-bind.
- **Effort:** S

## F12: Inconsistent mutex-poison policy — pushes/acks error forever, reaper recovers

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:545-548, daemon.rs:562-566 vs daemon.rs:299-302
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:** `reap_expired` deliberately survives poisoning:

  ```rust
  let mut map = match daemon.pending.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
  };
  ```

  but `handle_message_pushed` and `handle_message_ack` bail:

  ```rust
  .lock()
  .map_err(|_| anyhow!("pending map poisoned"))?;
  ```

- **Failure scenario:** If any thread ever panics while holding the lock, the daemon enters a half-alive state: the reaper keeps draining (and emitting `ack=timeout` lines), while every subsequent `message-pushed`/`message-ack` is rejected until restart — acks stop clearing entries, so *every* pushed message from that point "times out" even though workers acked. The two policies should agree; given the map operations can't panic mid-mutation, recovering everywhere is safe.
- **Recommendation:** Use `unwrap_or_else(PoisonError::into_inner)` in all three sites (or switch to `parking_lot::Mutex`, which doesn't poison).
- **Effort:** S

## F13: Pending-ack map is unbounded; hostile `message-pushed` spam amplifies into events.log

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:542-551, daemon.rs:318-322
- **Category:** hardening
- **Severity / Confidence:** low / likely
- **Evidence:** `handle_message_pushed` inserts `(task_id, msg_id)` pairs with no cap and no validation that the task exists or that a corresponding `push=ok` was ever written:

  ```rust
  map.insert((task_id.to_string(), msg_id.to_string()), Instant::now());
  ```

  Every entry that ages past the timeout becomes one `ack=timeout` line in events.log via the reaper.
- **Failure scenario:** Any same-user process can loop `{"verb":"message-pushed","task_id":"x","msg_id":"<n>"}` and (a) grow daemon memory linearly, and (b) 60 seconds later, have the reaper flood events.log with fabricated `ack=timeout` lines — noise the orchestrator treats as real delivery failures, and log-volume amplification (small input line → durable output line) that compounds F9. Not remote-reachable, but a buggy worker retry loop produces the same effect by accident.
- **Recommendation:** Cap the map (e.g. 10k entries; reject or evict-oldest beyond it with a stderr warning) and cap key lengths. Optionally require `message-pushed` to reference a `msg_id` shape the CLI actually mints.
- **Effort:** S

## F14: `task edit` breaks when `$EDITOR` contains arguments

- **Where:** crates/shelbi-cli/src/commands/task.rs:745-746
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
  let status = std::process::Command::new(&editor).arg(&path).status()?;
  ```

  `EDITOR="code --wait"` or `EDITOR="emacsclient -t"` — both common — are passed as a single program name; exec fails with "No such file or directory" naming the whole string.
- **Failure scenario:** `EDITOR="code --wait" shelbi task edit fix-1` → `Error: No such file or directory (os error 2)` with no hint that the flags are the problem. The conventional contract is that EDITOR is interpreted by the shell.
- **Recommendation:** Split on whitespace (`editor.split_whitespace()` → first token program, rest args) or invoke via `sh -c "$EDITOR \"$file\""`. Also honor `$VISUAL` before `$EDITOR` per convention.
- **Effort:** S

## F15: `zen dry-run --interval 0` busy-loops; `format_duration` renders zero as `0d`

- **Where:** crates/shelbi-cli/src/commands/zen.rs:236-239, zen.rs:434-457, zen.rs:522-533; crates/shelbi-cli/src/commands/events.rs:145-168
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:** `parse_duration("0")` returns `Duration::ZERO` (the function only rejects empty/garbage/overflow). With `interval == 0` the loop at zen.rs:434 does `thread::sleep(Duration::ZERO)` and immediately re-runs `zen::dry_run_tick` — a hot spin over board scans and git probes. Same zero flows into `ci-watch --timeout 0`, where zen.rs:198-204 would report "timed out after 0d" — because `format_duration`:

  ```rust
  let secs = d.as_secs();
  if secs % 86_400 == 0 { format!("{}d", secs / 86_400) }
  ```

  `0 % 86_400 == 0` → `"0d"`.
- **Failure scenario:** A user (or an orchestrator prompt template with an unset variable) passes `--interval 0` → one CPU core pinned and the events.log/dry-run log written as fast as decisions dedupe, until Ctrl-C. Cosmetic sibling: `--timeout 0` prints `timed out after 0d`.
- **Recommendation:** Reject zero in `parse_duration` (or clamp interval to a floor, e.g. 1s, with a warning) and special-case `0` in `format_duration` to `"0s"`.
- **Effort:** S

## F16: `zen dry-run` tick error kills the whole preview loop despite the best-effort contract

- **Where:** crates/shelbi-cli/src/commands/zen.rs:435
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:** The sinks are explicitly best-effort ("a transient I/O failure shouldn't kill the preview loop", zen.rs:487-488), but the tick itself is not:

  ```rust
  let decisions = zen::dry_run_tick(&project_obj).map_err(|e| anyhow!(e))?;
  ```

  Any single failed tick — a `gh` rate-limit blip, a transient git lock while a workspace commits, a momentarily malformed task file — propagates out of the loop and terminates a run the user may have started with `--for 2h` to observe overnight behavior.
- **Failure scenario:** `shelbi zen dry-run --for 8h` started before stepping away; 40 minutes in, one tick's git probe races a workspace's rebase and errors; the whole preview exits. The user returns to a dead run with 7 hours of the window unobserved — the tool's entire purpose (watch what Zen *would* do over time) defeated by one transient.
- **Recommendation:** Log the tick error to stderr + the dry-run log and continue to the next tick; abort only after N consecutive failures.
- **Effort:** S

## F17: One corrupt `status.yaml` kills the entire `workspace status` table

- **Where:** crates/shelbi-cli/src/commands/workspace.rs:213-236
- **Category:** failure-scenario
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  for name in names {
      let row = shelbi_state::load_workspace_status(name).map_err(|e| anyhow!(e))?;
  ```

  `load_workspace_status` propagates YAML parse errors (`serde_yaml::from_str(&text)?`). The missing-file case is handled (`None` row prints `?` / `never`), but a *malformed* file aborts the whole command mid-table.
- **Failure scenario:** The hub poller is killed mid-write of one workspace's `status.yaml` (it uses `atomic_write`, so this specific writer is safe — but a remote-synced or hand-edited file isn't), or an operator edits the YAML. `shelbi workspace status` then fails outright, hiding the state of every *healthy* workspace exactly when the operator is debugging. Contrast with `list_tasks`, which skips malformed task files with a deduped warning — the same degrade-gracefully policy belongs here.
- **Recommendation:** Match on the load result per row: parse error → print the name with a `(corrupt status.yaml: …)` cell and continue.
- **Effort:** S

## F18: `release_workspace_tasks` double-write is not crash-safe

- **Where:** crates/shelbi-cli/src/commands/workspace.rs:277-294
- **Category:** failure-scenario
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  task.assigned_to = None;
  ...
  shelbi_state::save_task(project, &task, &tf.body)...;   // write 1: unassign
  let moved = shelbi_state::move_task(project, &id, Column::Todo)...;  // write 2: re-reads + moves
  ```

  Two independent writes; the comment acknowledges `move_task` re-reads the file. A crash (or a failure in `move_task`) between them leaves the task **in `in_progress` with `assigned_to: None`**.
- **Failure scenario:** `workspace stop alpha` kills the pane (already done, workspace.rs:161), then dies between the writes. The board now shows an in_progress card owned by nobody, with no pane behind it. Nothing releases it: `release_workspace_tasks` filters on `assigned_to == workspace` so a re-run of `workspace stop` skips it, and `task start`'s conflict check (task.rs:639) also matches on `assigned_to`, so it won't collide — the card just sits in `in_progress` until a human notices and moves it manually.
- **Recommendation:** Do it in one write: load, set `assigned_to = None` *and* `column = Todo` + bottom priority, save once, then `renumber_column(InProgress)` — mirroring what `move_task` does internally but with the unassign folded into the same `atomic_write`.
- **Effort:** S

## F19: `move_to` cuts the branch before the move; a failed move leaves the side-effect behind

- **Where:** crates/shelbi-cli/src/commands/task.rs:434-441
- **Category:** failure-scenario
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  if column == Column::InProgress && tf.task.column != Column::InProgress {
      ...
      shelbi_orchestrator::lifecycle::ensure_branch_for_in_progress(&project_yaml, id)?;
  }
  let moved = shelbi_state::move_task(project, id, column)?;
  ```

  The hub-side branch cut (which also persists `branch:` onto the task file, per the doc comment) happens before `move_task`. If `move_task` fails — the task file was deleted/renamed between the `load_task` at task.rs:409 and here, or an IO error during the column renumber — the command errors, but the branch exists and the task's frontmatter now carries `branch:` while the card never moved.
- **Failure scenario:** Orchestrator retries the failed move against a *different* column, or the user re-points the task; the stale `shelbi/<id>` branch and `branch:` field persist and the next `in_progress` transition short-circuits on "branch already recorded" against a ref cut from a base that may have moved. Low impact (the cut is idempotent and depends_on-aware) but it's a one-way side-effect inside a command that can still fail — worth noting because the code comment argues ordering carefully in the other direction.
- **Recommendation:** Accept and document the ordering (idempotence makes it mostly benign), or move the cut after a `move_task` dry-check (load + validate destination) so the only writes preceding it are safe ones.
- **Effort:** S

## F20: Crash-recovery "recent" heuristic is line-count based, not time based

- **Where:** crates/shelbi-cli/src/commands/status.rs:46, status.rs:354-361
- **Category:** assumption
- **Severity / Confidence:** low / certain
- **Evidence:** `has_recent_crash_event` scans the last `CRASH_EVENT_SCAN_LINES = 200` lines for `project=<name> … zen=off … reason=crash-recovery`, with no timestamp comparison, even though every line begins with an RFC3339 timestamp.
- **Failure scenario:** Two failure modes in opposite directions. Quiet hub: a crash line from three weeks ago is still within the last 200 lines, so every `shelbi status` bootstrap keeps telling the orchestrator "crash-recovery flagged — review in-flight work" long after the user dealt with it; the orchestrator re-raises it in its first reply forever. Busy hub: heartbeats alone (one per cadence tick, per project) push a genuine crash line past position 200 within minutes, so the orchestrator that boots an hour after the crash sees nothing. The flag's meaning ("recent") silently depends on unrelated event volume.
- **Recommendation:** Parse the leading timestamp and flag only lines newer than a wall-clock window (e.g. 24h), scanning however many lines that takes from the tail (combines naturally with the F9 tail-read fix).
- **Effort:** S

## F21: Daemon echoes raw client bytes to stderr — terminal-escape injection into daemon logs

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:426-430
- **Category:** hardening
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  if let Err(e) = dispatch(&line, daemon) {
      eprintln!("shelbi daemon: rejected message: {e}: {line}");
  }
  ```

  A rejected line is echoed verbatim. `lines()` strips `\n`, but every other byte — ANSI escape sequences, `\r`, control characters — passes through into `daemon.err` (launchd) / `daemon.log` (systemd), which operators read with `tail -f` in a terminal.
- **Failure scenario:** A malicious or fuzz-happy local process sends `{"verb":"\x1b]0;pwned\x07\x1b[2J..."}`-style payloads; the daemon rejects them but faithfully replays the escape bytes into the operator's terminal during `tail -f daemon.err` — title changes, screen clears, or in older terminals worse. Also applies to the unknown-verb error path, which interpolates `msg.verb` (daemon.rs:482).
- **Recommendation:** Log with `{line:?}` (Rust debug-escaping) or truncate + strip control characters before echoing. Same treatment for `other` in the unknown-verb error.
- **Effort:** S

## F22: `daemon.rs` mixes socket server and OS-supervisor plumbing in one 1500-line file

- **Where:** crates/shelbi-cli/src/commands/daemon.rs:1-1500 (split point at the `Supervision` banner, daemon.rs:584)
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:** Lines 1–583 are the socket listener, message protocol, reaper, and shutdown logic; lines 584–1063 are launchd/systemd install/uninstall/status/restart with per-OS `cfg` fan-out (four near-identical three-way dispatchers at daemon.rs:593-647); the rest is tests. The two halves share nothing but `Result` — no types, no state. The `#[allow(dead_code)]` on `SYSTEMD_SERVICE_NAME`, `SystemdInputs`, and `render_systemd_unit` (daemon.rs:93, 1016, 1035) exists purely because the cross-platform test surface lives in the same module as the cfg-gated production code.
- **Recommendation:** Split into `daemon/serve.rs` and `daemon/supervise.rs` (or `supervise_launchd.rs` / `supervise_systemd.rs`). The four `cfg` dispatcher quartets collapse to one `#[cfg]` at the module level, and most `#[allow(dead_code)]` annotations disappear. Pure mechanical move; also makes the F2/F3 fixes easier to review in isolation.
- **Effort:** M

---

## Seed-question outcomes not carrying a numbered finding

- **hub.sock stale after crash:** handled correctly — `prepare_socket` connect-probes and reclaims only dead sockets (daemon.rs:369-387); the PID file is written after bind and removed on clean exit, and the CM-cleanup read path tolerates staleness. The remaining gap is the race in F4.
- **Invalid UTF-8 / malformed JSON on the socket:** handled — `BufRead::lines` surfaces invalid UTF-8 as an `Err` that closes just that connection (daemon.rs:416-421); malformed JSON is rejected per-line and the connection continues (daemon.rs:426-430, tests at daemon.rs:1127). No panic path found. Oversized payloads are the gap (F2).
- **events.log interleaving:** the single-writer daemon path plus the O_APPEND single-`write_all` fallback (shelbi-state/workspace_status.rs:659-680) is correct for lines ≤ PIPE_BUF; torn lines only become possible via F2's unbounded bodies.
- **Dead tmux server:** `workspace stop` degrades correctly — `workspace_pane_alive` / `has_session` treat a non-zero tmux exit as "not alive" and the kill path no-ops, so task release still proceeds (orchestrator/workspace.rs:381-394, 419-423).
- **CLI output vs exit codes:** the deliberate exit-0-with-warning paths (event-append failures in `move_to`/`start`/`release_workspace_tasks`, HANDOFF.md delete failure, `zen pr-merge` scan hiccup, unsupported-OS `daemon install`) all match documented intent and print to stderr; `zen ci-watch` correctly maps red→1 and timeout→2. No silent-success mismatches found beyond F8's swallowed typos.
