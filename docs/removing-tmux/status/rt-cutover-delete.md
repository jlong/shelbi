# rt-cutover-delete (Phase 6: Cutover, part 2)

**Status:** complete. `cargo build --workspace --all-targets`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `cargo test --workspace` are all
green.

Delete tmux from the codebase: the `shelbi-tmux` crate, `TmuxAddr`, all `$TMUX`
branching, the tmux dashboard/stash/sidebar/palette layer, the legacy agent
commands, the standalone overlay processes, and the 47 `tmux_available()`
guards. One runtime remains: the session-process backend.

## Deleted modules / commands (and the plan bullet that removes each)

Plan bullets are from `removing-tmux.md` → "What gets deleted".

- **`shelbi-tmux` crate** + its workspace-member / dependency entries
  (orchestrator, cli, tui) — "The `shelbi-tmux` crate, `TmuxAddr`, …".
- **`TmuxAddr`** (`shelbi-core::model`) + re-export — same bullet. `Agent` loses
  its `tmux` field; a persisted record carrying `tmux:` still loads (unknown
  field ignored — regression test kept).
- **`TmuxBackend`** + its argv builders/parsers (`session_backend.rs`); the
  `Backend` enum collapses to a newtype over `SessionProcessBackend`;
  `SessionTarget::from_tmux_addr`/`to_tmux_addr` removed. `backend()` always
  returns the session backend. — "The tmux implementation of `SessionBackend` …
  One runtime remains."
- **CLI commands** `spawn`, `archive`, `tail`, legacy `attach`, `merge`,
  `popup`, `open --as-pane`, `quit`/`quit_project`/`quit_shelbi`/`teardown`,
  and the `open/pane.rs` wrapper — legacy-agent + popup + quit bullets.
- **Legacy views/overlays** `__sidebar`/`__tasks`/`__activity`/`__machines`/
  `__review-panel` and `__palette`/`__review-confirm`/`__review-reject-reason`/
  `__error-log` + their CLI modules (`palette.rs`, `review_confirm.rs`,
  `review_reject.rs`, `error_log.rs`, `list.rs`, `diff.rs`) — "standalone
  overlay processes" + legacy `list`/`diff` fallbacks.
- **orchestrator `lib.rs`** tmux dashboard/stash/sidebar/palette layer:
  `ensure_dashboard`'s tmux body, `ensure_hidden_views`, the sidebar-clamp
  helpers, the session-closed hook, `apply_palette_binding`, `workspace_pane_cmd`,
  the pane-reload machinery, and the tmux `reload`/`reload_target` family —
  "ensure_dashboard, the stash session, show_view's swap-pane logic, the sidebar
  clamp hooks, apply_palette_binding, the session-closed hook".
- **`review_ui.rs`** pane plumbing (`open_review_interface`, `show_review_view`
  swap-pane, the editor/diff pane builders, the stash session, the session-var
  interface state) — "The pane plumbing in review_ui.rs". Kept: `approve_review_task`,
  `close_review_window`, `reject_review`, `review_layout_state`, the git builders.
- **`to_tmux_key`/`tmux_keyname`** (`shelbi-state::keymap::chord`) +
  `tmux_palette_key` (GlobalState; preserved as an unknown field on load) —
  "to_tmux_key and tmux_palette_key in shelbi-state".
- **`tmux_test_support.rs`** + every `tmux_available()`-guarded real-tmux test
  across lib/workspace/poller/submit/handoff/load/wake/review_ui, and the
  `session_op_seam_gate.rs` guard test — "tmux_test_support.rs and the 47
  tmux_available() guards. Tests that only exercised tmux are deleted".
- **Hidden dev settings** `session_backend` + `daemon_mutations` (DevConfig,
  removed whole) and the `SHELBI_DAEMON_POLLER` sidebar-poller setting — "the
  hidden backend setting, the sidebar-poller setting and the daemon-mutation
  setting". With the poller gate gone, the daemon's poller manager runs
  unconditionally (one poller per open project); the stale doc comments that
  described the gate and the "sidebar owns the poller by default" fallback are
  reworded.
- **CLI `keys.rs`** (crossterm→chord edge) and **`zen_intro.rs`** (re-export for
  the deleted `palette` popover) — both orphaned once their only consumers (the
  deleted overlay/palette processes) were removed.
- **`mutate_client.rs`** dead helpers `map_mutate_err` + `StdoutSink` (the
  in-process `OutputSink` path), orphaned with the deleted direct-mutation CLI
  commands.
- **Supervise-restart launch build.** `supervise_restart_orchestrator` no longer
  rebuilds a launch command: the one runtime's `respawn` ignores the command and
  returns `Failed` (a session keeps the binary it started with), so the restart
  just asks the backend to respawn the `shelbi-<project>` session. The old
  pane-pin path (`SHELBI_PANE_orch` lookup, `orchestrator_pane_cmd` wrapper) is
  gone; its two tests are rewritten to the session contract.

## Renames / rewires

- `shelbi session attach` → **`shelbi attach <workspace>`** (top-level), detach
  key configurable (`--detach-key`, default `ctrl-]`). The debug `shelbi session`
  group keeps `ls/new/kill/send/snapshot`.
- `workspace::workspace_tmux_addr` → **`workspace_target`**, returning a
  `SessionTarget` (not `TmuxAddr`); every caller rewired. The orchestrator's
  internal addressing is now `SessionTarget` end to end.
- **Local dispatch off `--as-pane` (decision — flag for review).** The session
  backend's local dispatch previously re-exec'd `shelbi open <ws> --as-pane` as
  the session's child. With `--as-pane` deleted, `deploy_and_spawn`'s local arm
  now launches the runner directly (mirroring the remote arm:
  `cd <wt> … exec $SHELL -lc <launch>`), carrying the same per-dispatch env
  (`TASK_ID`/`PROJECT`/`SHELBI_HUB_SOCK`/`SHELBI_AGENT`/`PORT`/
  `SHELBI_MANAGED_CONTEXT`). The `shelbi __session` process + daemon poller own
  lifecycle/crash detection (the poller already has a lost-`pane_alive` backstop),
  so the wrapper's event emission is no longer needed. `shelbi open <ws>` now
  ensures the workspace session (an idle login shell via
  `orch_workspace::open_user_shell` when no task is assigned). **This is the one
  non-mechanical behavior change; verify against `session_backend_e2e`.**
- **`shelbi reload`** always runs the session-runtime reload
  (`reload_session::run` + the backend-agnostic self-heal); targeted pane reloads
  print a note and fall through. The tmux `reload`/`reload_target` are gone.
- **Daemon board open-project discovery off tmux.** `daemon::board::open_project_names`
  listed live `shelbi-<name>` tmux sessions via `tmux list-sessions`; it now reads
  the Phase 3 open-project record (`shelbi_state::list_open_projects`). The
  `parse_open_project_names` session-line parser + its test are deleted.

## Migration (kept, per "keeping only what migration needs to detect and close a leftover tmux session")

- `migration.rs` stays. `RealMigrationProbe` no longer calls `shelbi_tmux`; it
  shells `tmux has-session`/`kill-session` directly via `shelbi-ssh` (exact
  `=<name>` match). This is the **only** place Shelbi still invokes tmux — the
  one-time leftover-session check for an install upgrading off the tmux runtime.
- The `session_backend_enabled()` early-returns are removed (one runtime);
  `ensure_project_openable` now always runs (no-op when tmux is absent). Its
  message points at `tmux kill-session` (the deleted `shelbi quit` is gone).
- **Carried-over review #1 (dispatch gate):** `ensure_workspace_dispatchable`
  now blocks only on an explicit `MigrationState::Pending`; a workspace with **no**
  recorded entry reads as dispatchable (a never-recorded workspace — e.g. one
  added post-cutover via `shelbi workspace add` — is no longer wedged). Added
  `absent_migration_entry_reads_as_dispatchable` test.

## Notes for review

- **Carried-over review #2 (seam-gate allowlist):** the `session_op_seam_gate.rs`
  guard test is deleted wholesale (it policed `shelbi_tmux::` calls; the crate is
  gone), so its README allowlist section is removed rather than gaining a
  `migration.rs` row.
- A few **doc comments** across cli/core still say "tmux" as historical/cutover
  context (e.g. "remove-tmux backend", "replaces tmux attach"); the `wake.rs`
  `native_event_delivery_has_no_composer_transport` lint test deliberately keeps
  an obfuscated `tmux` token to guard against reintroduction.
- Agent-instruction templates, packaging (deb/formula/goreleaser), docs, site,
  and the CI tmux/Screen smoke job are **sibling tasks** (`rt-cutover-instructions`,
  `rt-cutover-packaging-docs`) — not touched here.
- **`spikes/remove-tmux/` is NOT deleted here (deferred for explicit approval).**
  The plan's "What gets deleted" and the README cutover row name `spikes/`, but
  it was not in this dispatch's enumerated deletion list and is outside every
  acceptance criterion (the sweep targets `crates/`; `spikes` is `exclude`d from
  the workspace, so it is build/clippy/test-neutral). The throwaway Phase 0
  prototypes' findings are already captured under `docs/removing-tmux/phase0/`.
  Removing the directory is a safe follow-up (also drop `"spikes"` from the
  `Cargo.toml` `exclude` list) — left out here because a bulk directory delete
  needs a human go-ahead.

## Hermetic tests

No test queries or kills sessions on the default tmux server or touches the real
`HOME`. Confirmed: `cargo test --workspace` ran green (46 suites, 3373 tests, 0
failures), and both before and after the run `tmux ls` still showed the live
`shelbi-shelbi` / `_shelbi-shelbi` sessions with identical creation times (the
suite never touched them) and `launchctl print gui/$(id -u)/dev.shelbi.daemon`
still succeeded.

## Acceptance sweep

`rg -i tmux crates/` finds only:
- the kept migration leftover-session check (`migration.rs`);
- the `$TMUX`/`$STY` nesting detection (`shell/caps.rs`) and the terminal-var
  scrub list (`login_env.rs::SCRUBBED_TERMINAL_VARS`);
- the deliberate reintroduction-guard token in `wake.rs`
  (`["shelbi", "tmux"].join("_")`);
- the shelbi-session `capture-pane` parity oracle (`capture_parity.rs`,
  skip-guarded, isolated `-L` socket) — the emulator's golden, out of this
  task's scope;
- the agent-instruction templates (sibling task `rt-cutover-instructions`);
- historical doc comments and the `extra`-field forward-compat tests that assert
  a persisted `tmux_palette_key` still round-trips as an unknown field.

No `shelbi_tmux` / `TmuxAddr` / `from_tmux_addr` remain; no production tmux
process spawn survives outside `migration.rs`.
