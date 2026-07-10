# Deferred findings — disposition

The `review-misc-low-findings` task (merged as **PR #166**, `98c2157`)
deliberately limited scope to safe one-liners and contained logic fixes so
its diff stayed within the Zen merge conditions. The larger findings from
that triage bucket were deferred to the follow-up task
`core-orchestrator-deferred-refactors-from-review-misc-low-findings-…`.

This document enumerates the **remaining** findings — every finding in the
five buckets #166 touched (core-model, cli-daemon-board, state-runtime,
orchestrator-lifecycle, cli-session-ux) that was neither fixed by #166 nor
by any other merge already on `main` — and records how each is disposed.

## How the remainder was derived

Each bucket's findings table was cross-checked against the fix commits on
`main`. Beyond #166, the following merges had already cleared findings:

| PR | Cleared |
|----|---------|
| #164 `f215dbc` | core-model F1, F2, F3 |
| #166 `98c2157` | core-model F4/F5(doc)/F7/F8; cli-daemon-board F11/F13/F15/F16/F17; state-runtime F11/F16; orchestrator-lifecycle F16; cli-session-ux F16/F17 |
| #146 `9f304d6` | cli-daemon-board F2/F3/F4/F10/F12/F21 |
| #151 `31c954f` | cli-daemon-board F5/F7/F8/F9/F14/F20; cli-session-ux F5 |
| #137 `603bb98` | cli-daemon-board F1 |
| #132 `2d2c0f0` | cli-daemon-board F6; state-runtime F9 |
| #149 `6ea074d` | state-runtime F1/F2/F8; orchestrator-lifecycle F8 |
| #150 `94a7972` | state-runtime F3/F4/F10/F12/F13 |
| #143 `5b70780` | state-runtime F6 |
| #152 `8e19782` | orchestrator-lifecycle F5/F6/F7/F14 |
| #153 `d81ef4d` | orchestrator-lifecycle F3/F4/F10/F15/F17 |
| #144 `6940963` | orchestrator-lifecycle F1/F2 |
| #148 `b6b3541` | cli-session-ux F1/F2/F3/F4/F8/F10 |

## Fixed in this task

| Bucket | # | Finding | Disposition |
|--------|---|---------|-------------|
| cli-daemon-board | **F22** | `daemon.rs` mixes socket server + OS-supervisor plumbing in one 1500-line file | **FIXED** — split into `commands/daemon/{serve,supervise}.rs` with a thin `daemon.rs` module root (`DaemonCmd` + `run()`). Pure mechanical move; behavior unchanged, 26 daemon tests + clippy green. |
| core-model | **F5** | Transition side-effect layer (`Transition.actions`/`target`) parsed & validated but never executed; `target` `{{var}}` never substituted | **FIXED (wired, not removed)** — `Workflow::resolve_transition_target` substitutes the target; `orchestrator::transition::execute_transition` walks the declared actions into the existing `actions::*` primitives; surfaced as `shelbi action apply-transition`. Chose *wire* over *remove* because the primitives already accepted `target_override` and `TransitionAction` maps 1:1 onto the action set — the layer was designed to be executed, only the walker was missing. `restack` is rejected as a standalone edge action (it needs a parent branch a status move doesn't carry; `merge` auto-fires it on dependents). |

## Remaining — deferred to dedicated tasks

Each of the items below is an independent behavior change (bug fix,
hardening, or simplification) in a subsystem **outside** the two headline
deliverables of this task (daemon split + transition executor). They are
**WONTFIX-here**: bundling ~20 unrelated fixes into this PR would recreate
exactly the oversized-diff problem that caused the original deferral and
would blow the Zen merge bar. Each should be picked up as its own scoped
task so it lands with a focused diff and its own test + review.

Severity/effort carried over from the review tables (S/M = small/medium).

| Bucket | # | Summary | Sev | Eff | Note |
|--------|---|---------|-----|-----|------|
| core-model | F6 | `Task.params` `#[serde(flatten)]` swallows optional-field typos; a numeric extra hard-fails the whole task parse | low | M | Open. Wants a post-deserialize Levenshtein-1 warning against known optional fields (`assigned_to`/`branch`/`workflow`/`prefers_machine`/`zen`) + a per-field error for numeric extras. Validation-surface change; own task. |
| cli-daemon-board | F18 | `release_workspace_tasks` double-write (unassign, then move) not crash-safe → card stuck in-progress/unowned | low | S | Open. Needs a single atomic status+owner write, or an idempotent recovery path. |
| cli-daemon-board | F19 | `move_to` cuts the branch before `move_task`; a failed move leaves the branch side-effect behind | low | S | Open, low impact (branch cut is idempotent). Reorder move-before-cut, or make it transactional. |
| state-runtime | F5 | Daemon PID-file check treats *any* live PID (incl. `EPERM`) as "another daemon" → PID reuse permanently skips ControlMaster cleanup | med | M | Open. `is_process_alive` needs process-identity (start-time / cmdline) verification, not bare liveness. |
| state-runtime | F7 | Agent-divergence detection byte-compares against the currently-compiled default → false positive on shelbi upgrade, false negative on repeated edits | med | M | Open. **Twin of cli-session-ux F6** — same root cause, two surfaces. Fix once with a provenance mechanism (content hash + prior-default tracking). |
| state-runtime | F14 | `workspace_status_path` / `agent_workspace_dir` join workspace/agent names into paths unvalidated → `..`/absolute traversal | low | S | **Partially handled**: #137 validated `project_dir`/`task_path`/`agent_path`, but these two chokepoints are still bare `join`s. Residual security hardening. |
| state-runtime | F15 | Chord parser edge cases: `ctrl-` → `MultiKeyNotSupported("")`; stale `from_event` doc; `canonical()` round-trip trap | low | S | Open. #150 fixed the keymap *loader*, not the `chord.rs` parser edges. |
| orchestrator-lifecycle | F9 | `ensure_dashboard` returns at `pane_count>=2` before `create_hidden_views`; a half-created view stash never heals | med | M | Open. Early return precedes view creation; needs an idempotent heal pass. |
| orchestrator-lifecycle | F11 | Concurrent `ensure_dashboard` double-splits / orphans the orch pane (check-then-act, no lock) | med | M | Open. Wants an flock around dashboard bootstrap. Related to F9. |
| orchestrator-lifecycle | F12 | Local dispatch computes `launch` then discards it, delegating to `open --as-pane` → two divergent launch paths | med | M | Open (simplification). Unify the local + SSH launch paths. |
| orchestrator-lifecycle | F13 | `show_view` swallows a non-zero `swap-pane` exit → clicking a view silently no-ops | low | S | Open. Surface the swap-pane failure instead of `let _ = …`. |
| orchestrator-lifecycle | F18 | `review.rs` porcelain carve-out parses paths via `l.get(3..)`, mishandling renames and git-quoted names | low | S | Open. Parse `git status --porcelain -z` properly (NUL-delimited, rename-aware). |
| orchestrator-lifecycle | F19 | `session-closed[42]` uses a fixed global hook slot and interpolates the session name into a shell `case` | low | M | Open. Hook-slot collision + quoting; needs a per-session hook or escaped interpolation. |
| cli-session-ux | F6 | `customized_marker` byte-compare conflates "user customized" with "stale prior default" | low | M | Open. **Twin of state-runtime F7** — fix together. |
| cli-session-ux | F7 | palette `run()` error paths leak raw mode / alt-screen and swallow `picker_loop` errors | low | S | Open. Wants a `Drop`-guard terminal restore. |
| cli-session-ux | F9 | `exists()`→`write` guard in `scaffold_project` claims race safety it lacks | low | S | Open, low risk (#148 made steps idempotent). Switch to `create_new`. |
| cli-session-ux | F11 | Pane-wrapper signal windows: listener installed after spawn; `kill_task_tail` signals a PID with no identity check | low | M | Open. Install-before-spawn + guard the kill. |
| cli-session-ux | F12 | `run_local_tmux`/`run_tmux` collapse all failures to `false` with null stderr → undiagnosable | low | S | Open. Capture + surface stderr. |
| cli-session-ux | F13 | `run_pick_up` duplicates init scaffolding (drifted `repo:` field) | low | M | Open (simplification). Extract a shared `write_project_yaml`/`finish_scaffold`. |
| cli-session-ux | F14 | palette dead code: identical `entry_from_row` match arms, duplicated `run_tmux`, code after test module | low | S | Open (cleanup). |
| cli-session-ux | F15 | `agent show`/`edit` skip the name validation `new` enforces; `read_skills` skips symlinked skills | low | S | Open. Apply `validate_agent_name` on all read paths; follow symlinks in skill scan. |

### Cross-cutting notes for whoever picks these up

- **state-runtime F7 ⇔ cli-session-ux F6** are the *same* byte-compare
  divergence bug in two crates — do them as one task with a shared
  provenance mechanism, not two.
- **state-runtime F14** is the only "partially handled" item: the storage
  chokepoints hardened in #137 don't cover `workspace_status_path` /
  `agent_workspace_dir`, so a hostile/synced workspace or agent name can
  still traverse.
- The medium-severity concurrency gaps (**orchestrator-lifecycle F9/F11**,
  **cli-session-ux F11**) are the highest-value remainder.
