# rt-auto-restart-killed-panes-up-to-5-times-then-relaunch-on-re-open

Status: ready for review.

Auto-restart every TUI main-area pane kind up to 5 times, then relaunch on
re-open.

- Raised the shared crash-loop cap to `MAX_RESTARTS_IN_WINDOW = 5` in
  `shelbi-orchestrator::supervision`; `load.rs` and the poller resume states
  share the constant, so all daemon-side caps move together. Added
  `SupervisionState::request_relaunch` / `restart_count` / `ever_alive`.
- Added `shelbi_state::supervision_relaunch`: per-pane "relaunch now" markers a
  TUI reopen drops and each daemon supervision pass consumes to reset its spent
  budget (orchestrator, per-workspace supervisor, stranded dev/review resume).
- Added a client-side restart controller in the TUI `SessionManager` (reusing
  the same decision core): it detects an unexpected pane exit, auto-restarts
  content sessions through the daemon `Ensure` op, shows
  "Session exited — restarting (N/5)…", and after the cap shows
  "Stopped after 5 restarts — select it again to relaunch"; re-opening resets
  the budget and relaunches. Deliberate teardown (`q`/Close) is not restarted.

Verified: `cargo build`/`clippy --workspace --all-targets -D warnings` clean;
`shelbi-orchestrator` (928), `shelbi-state` (872), and `shelbi-tui` shell (174)
lib tests green. Full-suite git tests that do real commits fail locally only
because this pane signs commits with a passphrase-protected SSH key (no
passphrase prompt possible headless); they pass with signing neutralized and
are CI's to confirm. No `Cargo.lock` change (no MSRV step).

Design note: persistent panes (orchestrator / agents / review slot) must come
back even when no TUI is attached, so their auto-restart stays daemon-side; the
client only reflects state and, on reopen, resets the daemon budget via the
markers. Content sessions are client-driven (the daemon has no "wanted"
signal), so their budget lives in the TUI. One known cosmetic edge: an external
`shelbi workspace stop` of the pane currently shown in the main area can render
the gave-up notice (re-selecting it correctly does nothing).
