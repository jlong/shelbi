# dt-merge-main: `origin/main` merged into the umbrella

## What was merged

A real merge (`git merge origin/main`) of the five commits `main` had gained
since the umbrella (`jlong/desktop-app`) was last synced, on top of the
umbrella head `c8f7cc9c` (after `dt-parity-audit`):

| Commit | PR | Change |
|---|---|---|
| `5ee778bd` | #1689 | dispatch: deliver a resume prompt only into a live, ready session |
| `11c55af7` | #1690 | supervision: relaunch a dead orchestrator for real, log restart only once it is live |
| `a2c2dbba` | #1693 | daemon: root-scoped daemon stop, signal forensics, hub-safety preamble |
| `4bc92c60` | #1702 | client: never attach to a session that has already pushed `exited` |
| `c9f8d3be` | #1703 | clarification: store the full question on the hub |

Every test those commits added is on this branch and passes (each named test
function occurs once, checked with a grep over `crates/` and `desktop/src/`).

## How each conflict was resolved

1. **`crates/shelbi-orchestrator/src/supervision.rs`.** The umbrella moved the
   supervision state machine to `crates/shelbi-app/src/supervision.rs` and left
   only `pub use shelbi_app::supervision::*;`; #1690 edited the original file.
   The orchestrator file stays the re-export. #1690's two hunks (a pane seen
   alive while give-up is latched is an outside relaunch: reset the budget and
   clear the give-up; plus the test
   `an_outside_relaunch_after_give_up_starts_a_fresh_budget`) were applied to
   the shelbi-app copy unchanged, by rewriting the path of
   `git show 11c55af7 -- crates/shelbi-orchestrator/src/supervision.rs`. Both
   hunks applied cleanly (3-line offset).
2. **`crates/shelbi-state/src/lib.rs`.** The `daemon_lifecycle` re-export list
   is the union: the umbrella's `ensure_daemon_running_with`,
   `set_daemon_launcher`, `DaemonLauncher`, `DaemonStartError` plus main's
   `StopOutcome`.
3. **`crates/shelbi-tui/src/shell/mod.rs`.** `skeleton_sidebar_model` was not
   new on main: it predates the merge base, and main's only edit to it was the
   new `orchestrator_status: None` field. The umbrella had already moved it to
   `SidebarModel::skeleton` in `shelbi-app` (`dt-shell-sidebar`), and every
   call site (the cold-switch frame `switch_frame_sidebar`, the TUI's
   `beta_config_skeleton` test, the desktop) already calls that. So the
   resolution drops the free function again and adds
   `orchestrator_status: None` to `SidebarModel::skeleton`; the cold-switch
   behavior main relies on is unchanged.
4. **`crates/shelbi-tui/src/shell/sidebar.rs`.** Main's behavior, the
   umbrella's tokens: `ORCH_STATUS_ROWS` (3 rows reserved so the supervisor's
   sentence wraps) is kept; main's raw `ERROR_BUTTON_BG` / `ZEN_BAND_BG`
   colors are dropped (the umbrella paints those from `palette().error_button`
   and `palette().zen`). The orchestrator-failure status style is
   `palette().ansi_red` (the dark theme's swatch is `Color::Red`, so the TUI is
   byte-identical to main), the first-run hint stays
   `palette().ansi_dark_gray`. Main's test
   `orchestrator_supervisor_failure_paints_red_on_the_status_row` now asserts
   `palette().ansi_red`.

## `orchestrator_status` in both clients

Main added one public view-model field, `SidebarModel.orchestrator_status`
(the daemon supervisor's reason a dead orchestrator is not coming back). It
was the only new public field in main's `shelbi-app` changes (`view.rs` and a
`refresh.rs` test fixture). The TUI shows it in two places, and so does the
desktop now:

- **Sidebar footer.** The wording moved into `shelbi-app` as
  `SidebarModel::footer_status` (`Orchestrator: <status>`, taking the row over
  `status_line`), used by the TUI's `SidebarView` and the desktop's
  `views/sidebar.rs` `footer_status`. The desktop paints it red
  (`theme.ansi_red`) above the version line (`sidebar-status`).
- **Orchestrator view placeholder.** The text moved into `shelbi-app` as
  `session::orchestrator_down_notice`; the TUI's gate (orchestrator target,
  not live or idle) stays in `shell/mod.rs`. The desktop's `SessionView`
  carries the status (`set_supervisor_status`, pushed by `app.rs`
  `update_chat_supervisor_status` on every sidebar change and when the chat is
  created) and its `notice` returns the same placeholder under the same gate.

`desktop/docs/parity.md` has the row; `parity_doc` passes.

Tests: `footer_status_names_the_orchestrator_failure_over_the_status_line`
(`view.rs`), the `orchestrator_down_notice` wording in
`restart_and_give_up_notices` (`session/mod.rs`),
`footer_status_shows_the_orchestrator_failure_in_red` and
`the_footer_shows_the_orchestrator_failure` (desktop `views/sidebar.rs`),
`a_down_orchestrator_names_the_supervisor_failure` (desktop
`views/session_view.rs`).

## Verification

All from this branch after the merge:

- `git merge-base --is-ancestor origin/main HEAD` holds;
  `git merge-tree --write-tree origin/main HEAD` reports no conflicts.
- Root: `cargo check --workspace --locked`, `cargo build --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `env -u TASK_ID -u PROJECT cargo test --workspace` (4119 passed, 0 failed).
- Desktop (`desktop/`): `cargo build`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test` (287 passed).
- `Cargo.lock` and `desktop/Cargo.lock` are unchanged by the merge.
