# rt-tui-add-project (Phase 4f)

**Status:** ready for review.

The command palette's "Add project" command now opens a real in-process form in
the single-process (session-backend) TUI, creating + opening the project
through a shared engine. Previously the TUI palette only showed a status note,
because the add-project form and project-creation logic lived in `shelbi-cli`
(which `shelbi-tui` can't depend on).

## What landed

- **Shared creation engine** (`shelbi-orchestrator::project_create`): the
  project-root validators, starter-YAML renderers, atomic registration writer,
  agents/workflows/statuses/Zen/PR-template materialization, and the
  context-scoped commit-guard install — moved out of `shelbi-cli`'s
  `init`/`project_root` so both the CLI and `shelbi-tui` call one engine, no
  second implementation. Human-readable progress now flows through a
  `ScaffoldReporter` seam (CLI prints it; the TUI shows a single status line),
  so a caller inside a ratatui alt-screen doesn't corrupt its display while the
  CLI keeps byte-identical output. Adds `validate_add_project` (the form's
  validation) shared by both front ends.
- **In-process overlay** (`shelbi-tui/src/overlay/add_project.rs`): the form's
  state machine + rendering, with the same fields, labels, and keys the tmux
  dialog shipped. Pure over a snapshot (unit-tested without a terminal); the
  legacy `shelbi __palette` dialog renders + decides through it too.
- **Shell wiring** (`shelbi-tui/src/shell`): `Effect::AddProject` opens the
  overlay; submit validates on the UI thread and scaffolds + switches off it
  (`create_job` / `poll_create_job`), matching the Zen-toggle off-thread pattern.
- **CLI** (`commands/palette.rs`, `commands/init.rs`, `project_root.rs`):
  `shelbi init`, the palette add-project dialog, and `run_pick_up` all call the
  shared engine; `project_root.rs` keeps only the `inquire` prompt loop and
  re-exports the moved validators. `shelbi init`'s output and prompts are
  unchanged (guarded by a new test that captures the engine's init output).

## Notes for review

- The decision to fully unify project creation into one shared engine (rather
  than a smaller, surgical extraction of just the form) was the orchestrator's
  call during planning.
- No shipped-default template changed: the rendered project YAML and every
  scaffolded file are byte-identical to before, so no config-upgrade sniffer is
  needed.
- `shelbi guard install` and the `-y` wizard's guard step still live in
  `shelbi-cli`, but share the one disclosure text via
  `githook::hub_branch_guard_disclosure`.
- `Cargo.lock` only gains `anyhow`/`dirs` edges on `shelbi-orchestrator` (both
  already in the tree); MSRV 1.88 `cargo check --locked` is green.
