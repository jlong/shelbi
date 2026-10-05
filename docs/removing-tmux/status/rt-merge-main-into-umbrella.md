# rt-merge-main-into-umbrella

Status: **done** — `origin/main` (9 commits) merged into `jlong/remove-tmux`
with a real merge commit; every conflict resolved so both sides' intent
survives. `cargo build --workspace`, `cargo clippy --workspace --all-targets -D
warnings`, and the targeted tests for #1494 / #1507 / #1485 / #1470 / #1483 are
green locally. `site/` is byte-identical to the umbrella base (no merged-in main
commit touches it), so its lint/build state is unchanged.

## Main commits carried in

`7e562685` (#1507 handoff refuse), `a5d60990` (#1494 supplant guard), `3531b934`
(#1485 own-pgroup guard), `eb315e4d` (#1483 zen `--no-fail-fast`), `99cb916e`
(#1481 MSRV→cargo), `d4ae6144` (#1472 squash-title test), `a804b33d` (#1470
heartbeat idle count), `6d9f0478` (#1466 real-tmux palette), `9074a7fe` (#1460
zen concurrent-dep probe test).

## Conflicted files and how each was resolved

| File | Resolution |
| --- | --- |
| `crates/shelbi-cli/src/commands/mod.rs` | Union — kept both `pub mod msrv_check;` (#1481) and the umbrella's `pub mod mutate_client;`. |
| `crates/shelbi-cli/src/commands/config_upgrade_apply.rs` | Union (x2) — kept the umbrella's tmux-instruction sniffer symbols/healer **and** #1483's `cargo_test_without_no_fail_fast` / `insert_no_fail_fast` / `heal_zen_cargo_test_fail_fast`. Both heal-blocks now run. |
| `crates/shelbi-cli/src/commands/issue.rs` | Took the umbrella's version (`--ours`). The only main change here was #1494, which lives entirely in functions (`start`, the supplant release block, guards + tests) the umbrella moved to `shelbi-orchestrator`. #1494's behavior was re-implemented there (below), so nothing was lost. |
| `crates/shelbi-orchestrator/src/lib.rs` | Took the umbrella's version (`--ours`). The only main change here was #1466, whose `apply_palette_binding` / `is_shelbi_executable` / `reload_target_tmux_tests` are tmux server-global palette code the umbrella deleted outright — **dropped** (see below). |
| `crates/shelbi-orchestrator/src/workspace.rs` | Split resolution: **kept** #1507's handoff code (`HandoffReadiness`, `rev_list_count`, `verify_worktree_ready_for_handoff`, `push_worker_directive`, `append_message_log_line`) and #1485's two own-pgroup guard tests; **dropped** main's tmux window helpers (`list_windows_argv`, `list_window_ids_argv`, `slot_window_ids_from_list`) and the real-tmux `kill_workspace_pane_reaps_duplicate_windows_sharing_the_slot_name` test — all replaced/deleted by the umbrella's session backend. #1485's production guard (`pgid_is_safe_to_signal` / `terminate_process_group`) auto-merged cleanly into `stop_review_server`. |
| `crates/shelbi-orchestrator/src/tmux_test_support.rs` | **Dropped** (`git rm`). Deleted by the umbrella (#1505); #1466 only modified it. See below. |

## #1494 ported into the session backend (not a conflict file, but required)

The umbrella moved dispatch from `commands/issue.rs::start` into
`shelbi-orchestrator`. #1494's overlay-based supplant-release guard was
re-implemented against the session backend:

- `mutate.rs`: added `pub(crate) fn workspace_busy_with_other` (overlay-sourced,
  Warm-board terminal confirmation, `Err` on unconfirmable stale board) beside
  the existing `workspace_occupied_by`.
- `mutate/start.rs`: added `workspace_pane_liveness` (via
  `workspace_target` + `workspace_slot_alive`, the session-backend idiom) and
  `release_supplanted_pane`; rewrote the release block to the
  `None`→release / `Some`→`skipped` / `Err`→leave-alone match, routing warnings
  through the `OutputSink`.
- `mutate/tests.rs`: ported all six `workspace_busy_with_other_*` tests plus the
  GitHub-backed test helpers; use `crate::test_lock::acquire()`. All six pass.

## #1507 poller call-sites fixed after the rename merge

Git rename-detected main's `crates/shelbi-tui/src/poller.rs` → the umbrella's
`crates/shelbi-orchestrator/src/poller.rs` and auto-merged #1507's handoff-verify
block in. Those lines kept shelbi-tui's crate-qualified paths
(`shelbi_orchestrator::workspace::…`, `shelbi_orchestrator::branch::…`) and its
test conventions, invalid inside `shelbi-orchestrator`. Fixed to `crate::…`, and
the two merged-in #1507 poller tests adapted to the orchestrator's conventions
(`crate::test_lock::acquire()`, `SessionTarget::slot("s","w")` in place of
`crate::test_support::ENV_LOCK` / `TmuxAddr`).

## Main fixes dropped because the umbrella deleted their code

- **#1466** (`fix(test): stop real-tmux tests clobbering the global palette
  binding`). The palette binding is a tmux server-global chord; the umbrella's
  session backend deleted `apply_palette_binding`, `is_shelbi_executable`, the
  `reload_target_tmux_tests` module, and `tmux_test_support.rs` entirely. Its
  guard and test target code that no longer exists, so all of #1466's hunks were
  dropped.
- The real-tmux test `kill_workspace_pane_reaps_duplicate_windows_sharing_the_slot_name`
  (added on main alongside #1485's tests) drives a live tmux server via the
  deleted `tmux_test_support`; dropped for the same reason. The orchestrator's
  own session-backend `kill_workspace_pane` reaping is covered by its existing
  tests.

## Notes

- `Cargo.lock` unchanged by the merge → no MSRV re-check needed.
- AC1 is content-based (squash-merge erases ancestry): `git merge-tree
  --write-tree jlong/remove-tmux origin/main` should merge cleanly once this
  subtask lands on the umbrella.

## Rework (2026-10-05): CI scan fix

PR #1515's `cargo test --workspace` failed on
`project_paths::scan::no_new_callsite_hand_builds_a_per_project_shelbi_path`:
the ported #1494 helper `write_github_project_yaml` in
`crates/shelbi-orchestrator/src/mutate/tests.rs` hand-built
`home.join("projects")`. The scan's `strip_test_scope` only skips lines after a
file's first `#[cfg(test)]`; `mutate/tests.rs` is pulled in as a module and has
no such line, so the whole file is scanned (the other ported helpers in
`lib.rs`/`quit.rs`/etc. sit behind `#[cfg(test)]` and are skipped).

Fixed by routing the helper through `shelbi_state::projects_dir()` (callers
already set `SHELBI_HOME` under `crate::test_lock::acquire()` before calling) and
dropping the now-redundant `home` parameter, matching the already-correct
`fresh_home()` helper. Verified: `cargo test -p shelbi-state --lib` (862 pass,
scan included), `cargo test -p shelbi-orchestrator mutate` (22 pass, all six
`workspace_busy_with_other` #1494 tests green), `cargo build --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings` clean. Test-only change,
`Cargo.lock` untouched.
