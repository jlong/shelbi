# Removing tmux

Tracking doc for the umbrella effort that removes tmux from Shelbi. The full
design lives in the ContextStore plan `shelbi/Plans/removing-tmux.md` (revised
2026-10-02, baseline `7f331c5`); this file is the in-repo map of phases,
subtasks, and decisions.

All work lands on the long-lived branch `jlong/remove-tmux` first. The
foundation (this umbrella, task `remove-tmux`) adds the scaffolding everything
else builds on; roughly 30 `rt-*` subtasks (workflow `remove-tmux-subtask`) then
branch from and squash-merge back into it, many in parallel. When every subtask
has landed, the human reviews the umbrella and merges the whole branch to `main`.
**tmux stays the default runtime on this branch until the Phase 6 cutover
subtasks flip it.**

## How to use this doc

- Each subtask updates its own **Status** cell when it lands (`Pending` →
  `Landed`).
- Phase 0 spikes write their findings to `phase0/<spike>.md` (see
  [`phase0/`](phase0/)).
- The board-move gated-merge bug fix is a **separate task on `main`**
  (`board-move-runs-gated-merge`), not an `rt-*` subtask, because it is a bug
  today independent of this effort.

## Target architecture (summary)

Shelbi stops using tmux entirely. Each agent, shell, and orchestrator runs under
its own small `shelbi __session` process that owns one PTY and one headless
terminal emulator, listens on its own socket under
`~/.shelbi/sessions/<short-id>/`, survives UI crashes and quits, and answers
terminal queries itself. The TUI becomes a single ratatui process that renders
sessions in terminal views. The daemon takes over the poller, supervision, and
all mutations. Remotes run the same session processes, reached through
`ssh <host> shelbi relay`.

## Phases and subtasks

Each phase lands on `main` (via this branch) and leaves Shelbi working.

### Foundation (task `remove-tmux`, this umbrella)

| Item | Status |
| --- | --- |
| `shelbi-proto`, `shelbi-client`, `shelbi-term` workspace crates with documented responsibilities | Landed |
| Frozen protocol core in `shelbi-proto` (framing + hello/attach/output/input/resize/snapshot/kill/exited) with round-trip tests | Landed |
| `spikes/remove-tmux/`, excluded from the cargo workspace | Landed |
| This tracking doc and `docs/removing-tmux/phase0/` | Landed |

### Phase 0 — Spikes

Throwaway prototypes (under `spikes/remove-tmux/`) to retire the real risks.
Exit criteria: a written list of what does not work and whether each item is
fixable, plus the emulator decision.

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-spike-emulator-replay` | Serialize full emulator state and rebuild it in a second emulator; **decides the emulator crate** | Landed — see [`phase0/emulator-replay.md`](phase0/emulator-replay.md); chose vendored `alacritty_terminal` |
| `rt-spike-agents` | Claude Code (full-screen + inline) and Codex in a PTY rendered by a ratatui widget: keys incl. Shift+Enter, mouse, paste, wide chars, redraw cost | Pending |
| `rt-spike-runtime` | Process survival (outliving the launcher on macOS and Linux/logind; child group dies on kill), the startup query responder, and nesting inside tmux and Screen | Landed (macOS + ssh + `portable-pty` verified; Linux/logind untested, no reachable host; see [`phase0/runtime.md`](phase0/runtime.md)) |

### Phase 1 — Session process, protocol, attach

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-session-process` | The `shelbi __session` process: PTY, emulator, socket, spawn, lifecycle; `shelbi session ls\|new\|kill\|send\|snapshot` | Pending |
| `rt-protocol-client` | Flesh out `shelbi-proto` additive capabilities and `shelbi-client` connect/handshake/request API and reader | Pending |
| `rt-replay` | Full-state attach replay (both buffers, saved cursors, modes, keyboard stack) | Pending |
| `rt-snapshot` | `snapshot` text matching `capture-pane -p -J` shape | Pending |
| `rt-term` | `shelbi-term` client emulator, scrollback, selection, search, input encoding; chosen emulator crate wired in | Pending |
| `rt-session-cli-attach` | The new rendered `shelbi attach <session>` | Pending |

### Phase 2 — A session seam in the orchestrator

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-backend-trait-tmux` | `SessionBackend` trait (session operations only), keyed on a backend-neutral `SessionTarget`, implemented over `shelbi-tmux` with no behavior change; migrated `workspace.rs`, `submit.rs`, `ready.rs`, `handoff.rs`, `load.rs` | Landed |
| `rt-backend-callers` | Move call sites (`workspace.rs`, `submit.rs`, `ready.rs`, `handoff.rs`, `load.rs`, `issue.rs`, `send.rs`, `open.rs`, `open/pane.rs`, `wake.rs`, poller probes) onto the trait | Landed |
| `rt-backend-sessions` | Implement `SessionBackend` over session processes; hidden backend-select setting | Pending |

#### Session-op seam gate (`rt-backend-callers`)

After `rt-backend-callers`, production code performs **session operations**
through `SessionBackend` (`backend()` + a `SessionTarget`), never through the
low-level `shelbi_tmux::` session-op functions. A guard test,
`crates/shelbi-cli/tests/session_op_seam_gate.rs`, parses the three consumer
crates with `syn` and fails the build if a direct
`shelbi_tmux::{new_session, has_session, has_session_with_deadline, send_text,
send_enter, send_line, capture, capture_history, pane_title}` call appears in
production (non-`#[cfg(test)]`) code outside the allowlist below. `#[cfg(test)]`
code is skipped: the integration tests that drive a real tmux server are deleted
at cutover with the `tmux_available()` guards.

Still permitted direct session-op calls (keep in sync with the gate's `ALLOWED`
list):

| File | Why exempt | Loses exemption |
| --- | --- | --- |
| `shelbi-orchestrator/src/session_backend.rs` | The tmux backend *is* the seam — these calls are the delegation every other caller routes through | Never (replaced by `rt-backend-sessions`' second backend) |
| `shelbi-orchestrator/src/lib.rs` | Orchestrator bootstrap + stash-session probes; not in the Phase 2 caller scope | Phase 3 (poller/supervision move to the daemon) |
| `shelbi-cli/src/commands/spawn.rs`, `tail.rs`, `merge.rs` | Legacy agent commands | Phase 6 cutover (deleted wholesale) |

**Session ops vs. layout.** Only session operations move behind the seam.
tmux *layout* (`new-window`, `select-window`, `split-window`, `swap-pane`,
`join-pane`, `break-pane`, `kill-window`, `kill-pane`, the `-e` env-injecting
pane spawn, and the `ssh … tmux attach` proxy window) is **not** abstracted —
it stays as raw `["tmux", …]` argv and is **deleted by Phase 4** when the
single-process TUI replaces what it does. These are the ~200 raw layout calls
(`lib.rs`, `review_ui.rs`, `workspace.rs`, `open.rs`'s shell/proxy windows, and
others); the gate does not police them. The tmux-topology-only session-ish
operations that have no non-tmux analogue (`kill_window`, `kill_pane`,
`live_pane_ids`, `spawn_local_pane`) live as inherent methods on `TmuxBackend`,
reached through the concretely-typed `backend()`.

### Phase 3 — Poller and supervision move to the daemon

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-daemon-lifecycle` | On-demand daemon start; retire launchd/systemd units with an upgrade step | Pending |
| `rt-daemon-poller` | Per-project poller manager and the open-project record | Pending |
| `rt-daemon-layout-split` | Split layout out of the poller (session half to daemon, layout half to clients) | Pending |
| `rt-mutations-daemon` | "The daemon executes mutations": control socket, per-issue queue, expected state, recheck before irreversible steps (also the second half of Phase 4a) | Pending |
| `rt-daemon-cancellation` | Generations, subprocess deadlines, and the quit barrier for every daemon job | Pending |
| `rt-session-restarts-daemon` | Session processes restart a dead daemon | Pending |

### Phase 4 — The single-process TUI

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-app-model` | **4a** `shelbi-app`: navigation, typed command registry, view models, background refresh; move the chord type off crossterm | Landed |
| `rt-tui-shell` | **4b** One event loop, sidebar, terminal-view widget, focus/mouse model, scrollback, selection, search | Pending |
| `rt-tui-native-views` | **4c** Issues and activity in-process; the new machines view | Pending |
| `rt-tui-overlays` | **4d** Port the five popup processes (palette, review confirm, reject reason, error log, zen intro) to in-process overlays | Pending |
| `rt-tui-review` | **4e** Review interface: panel native view, editor + diff sessions, in-process split | Pending |
| `rt-tui-project-quit` | **4f** Project switching and the three quit actions | Pending |

### Phase 5 — Remote hosts

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-machine-setup` | `shelbi machine setup`, the PATH probe, the version check | Landed |
| `rt-relay` | `shelbi relay` and the remote transport in `shelbi-client` | Pending |
| `rt-remote-spawn` | Remote spawn through session processes; delete the paste-buffer launch; reconnect after an SSH drop | Pending |

### Phase 6 — Cutover (one release)

| Subtask | Scope | Status |
| --- | --- | --- |
| `rt-cutover-migration` | Per-workspace migration state; dispatch waits for a workspace to be proven idle | Pending |
| `rt-cutover-delete` | Flip the default and delete everything under the plan's "What gets deleted" (incl. `shelbi-tmux`, `spikes/`) | Pending |
| `rt-cutover-instructions` | Rewrite the three shipped agent templates off tmux; config-upgrade rule for user-edited instructions; session names in review events | Pending |
| `rt-cutover-packaging-docs` | Drop the tmux dependency from packaging; update docs and `site/`; CI smoke job inside tmux and Screen | Pending |

## Decisions

Copied from the plan's decisions table.

| Question | Decision |
| --- | --- |
| Terminal emulator crate | Vendored `alacritty_terminal` (`vendor/alacritty_terminal/`, Apache-2.0). It is the only candidate that models both screen buffers, saved cursors, scroll region, tab stops, charsets, the kitty keyboard protocol, and history; vendored because that state is private upstream and the crate is pre-1.0. Decided in `rt-spike-emulator-replay` ([`phase0/emulator-replay.md`](phase0/emulator-replay.md)). |
| Persistence | Agents survive quitting or crashing the UI. Clients attach and detach. |
| Who owns the PTYs | One small process per session. No shared host. Chosen because it is the most reliable shape: a bug can cost one agent at most, and upgrades never touch running agents. |
| Remote machines | The `shelbi` binary is installed on each remote. Agents survive SSH drops. |
| Remote binary | Use a compatible `shelbi` already on the remote's PATH; otherwise install to `~/.shelbi/bin`. |
| Concurrent clients | Any number of UIs attached at once. |
| Terminal size with several clients | The most recently active client's size, debounced. |
| Full-screen agents | Left in their default mode. The mouse is forwarded to an agent that has asked for it; Shift+wheel and Shift+drag are Shelbi's own. |
| Terminal features | Scrollback, selection and copy, and search, for sessions on the normal screen. `shelbi attach <workspace>` from any terminal, rendered, not raw passthrough. |
| Reserved keys | One chord, Ctrl+Space, opens the palette. Everything else is a palette command or a mouse action. |
| History on disk | A readable text snapshot when a session ends. The raw output log is opt-in per project. |
| Daemon lifecycle | Started on demand. The launchd and systemd units are retired. Session processes restart it if it dies. |
| Mutations | Executed by the daemon, one per issue at a time. The CLI, TUI, and desktop app send commands to it. |
| Rollout | Hard cut at parity. No long-lived tmux fallback. |
| Platforms | macOS and Linux now. New code is Windows-ready: session transport, locking, and PTYs sit behind abstractions (`portable-pty` covers ConPTY; a named pipe can stand in for the socket). Existing Unix-only code is not ported here. |
| Legacy agent commands | `shelbi spawn`, `archive`, `tail`, the old `attach`, and `merge` are removed at cutover. |
| Desktop | Sibling of the TUI at feature parity. The shared app model is built here, in Phase 4. |
