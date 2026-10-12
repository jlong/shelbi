# rt-tui-headless-startup-block

Status: **done** — the single-process TUI now draws its first frame within
milliseconds of launch; the daemon start, dashboard bootstrap, and kitty probe
that used to run synchronously before the first draw are off the first-frame
path.

## Which step blocked, with timings

Launching `shelbi` in a headless PTY drew nothing for 8+ seconds. Three steps
ran synchronously **before the first frame**, in this order:

1. **Daemon start** — `run_main` → `shelbi_state::ensure_daemon_running()`. On a
   cold launch (no daemon answering, which is exactly the CI-smoke / headless
   case) this spawns `shelbi daemon` and blocks in `wait_for_socket` up to
   `START_DEADLINE = 10s` (polling every 100 ms) until the socket answers —
   typically a few hundred ms to ~1 s for the daemon to bind.
2. **Dashboard bootstrap** — `run_main` → `shelbi_orchestrator::ensure_dashboard()`.
   Cold, this probes the session backend, takes the dashboard lock, and launches
   the orchestrator agent (the stub in the smoke job still goes through a real
   agent spawn + readiness wait). This is the largest and most variable chunk —
   multiple seconds on a cold start.
3. **Capability probe** — `shell::run` → `Caps::detect()` →
   `crossterm::terminal::supports_keyboard_enhancement()`. crossterm writes the
   `CSI ? u` / `CSI c` query to `/dev/tty` and then waits a **hard-coded 2000 ms**
   for a reply (crossterm-0.28.1 `src/terminal/sys/unix.rs:234`). A headless PTY
   (and an outer tmux/Screen that swallows the query) never replies, so this was
   a flat ~2 s every time.

Board read and session attach were already off the UI thread (the background
refresher and `SessionManager::show`'s connect worker), so they were **not**
first-frame blockers — but the orchestrator session attach can only succeed
*after* the dashboard bootstrap, which is why it is re-fired when bootstrap
completes.

Sum ≈ (daemon ~0.3–1 s) + (dashboard cold launch, several s) + (caps 2 s) ≈ the
reported 8 s. Warm launches (daemon already up, orchestrator already serving)
hid the problem, which is why it only surfaced in the headless smoke job.

## The fix

- **Capability detection split** (`shell/caps.rs`): `Caps::detect_fast()` reads
  only the instant env signals (truecolor from `COLORTERM`, nesting from
  `$TMUX`/`$STY`) and defaults `kitty: false`. The kitty round-trip moved to
  `probe_keyboard_enhancement(KITTY_PROBE_TIMEOUT = 150ms)`, a bounded
  poll-stdin probe that falls back to `false` when no reply arrives. The shell
  draws the first frame, *then* runs the probe (raw mode on, nothing else
  draining stdin yet), then folds the result in via `set_caps` (which arms the
  one-time keyboard notice). Worst case the probe adds 150 ms *after* the first
  frame, never before it.
- **Daemon + dashboard moved off the UI thread** (`shell/mod.rs`,
  `lib.rs::run_main`): `ensure_daemon_running` + `ensure_dashboard` now run in a
  background startup job (`ShellState::spawn_startup`, behind a `Bootstrap`
  seam). `run_main` no longer calls them synchronously. The event loop draws the
  first frame (sidebar placeholders + a "starting orchestrator…" status) while
  the job runs; `poll_startup` folds the result in — on success it re-attaches
  the orchestrator session (`SessionManager::reconnect`, so an attach that
  failed before the session existed retries), on failure it surfaces the error
  as a status line.
- **Migration pass stays synchronous before the shell**: its `[y/N]` consent
  prompt needs a plain-terminal stdin, and it probes tmux/SSH directly (needs
  neither the daemon nor the dashboard), so it can't move into the alt-screen
  startup job. It is a no-op for local-only projects (the headless/CI case), so
  it does not reintroduce the block.

## Tests

- `shell::caps::classify_kitty_reply_reads_the_first_terminator` — pure parser:
  `u` before `c` ⇒ kitty, `c` first ⇒ none, neither ⇒ keep reading.
- `shell::caps::kitty_probe_times_out_fast_when_the_terminal_never_answers` — a
  pipe that never answers returns `false` near the (short) timeout, not after 2 s.
- `shell::caps::kitty_probe_reads_a_pending_flags_reply` — a seeded flags reply
  is read and reported as supported.
- `shell::tests::a_slow_bootstrap_never_blocks_the_ui_thread` — a `Bootstrap`
  stub wedged mid-start: `poll_startup` stays non-blocking and input is still
  handled (Ctrl+Space opens the palette) while it runs; completion folds in.

## Open item for the orchestrator (criterion 4)

The `rt-cutover-packaging-docs` tmux/Screen CI smoke job has **not** merged into
`jlong/remove-tmux` yet (no first-frame smoke job exists in `.github/workflows/`,
no `rt-cutover-packaging-docs.md` status file). So there is nothing to tighten
here. When that job lands, it should assert a **rendered first frame** (non-empty
output within ~1 s) under both `tmux` and `screen` with a PTY that never answers
the capability query — the two scenarios this task fixed.
