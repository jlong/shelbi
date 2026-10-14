# dt-parity-audit: parity proved, the stub gone, the docs finished

## What landed

- **No not-implemented path.** The foundation's stub is gone from the
  desktop crate: `CommandHost::not_implemented`, `not_implemented_message`,
  `view_stub_id`, `HostCall::NotImplemented`, and the main area's
  "Not implemented yet" stub (`render_main` now matches `View`
  exhaustively and draws each view natively). The last two stub arms:
  - `OpenEditor` is real in **both** clients (the TUI only printed "lands in
    a later subtask"). The path both open is the new shared
    `shelbi_app::command_model::edit_target_path` (project settings, an
    agent's `instructions.md`, `zenmode.md`, the `workflows/` directory),
    with the `editor` setting, then `$EDITOR`, then vim. The TUI hands its
    terminal to the editor and waits (`run_editor`: leave raw mode and the
    alternate screen, run it, restore, repaint); the desktop launches it
    through `views/external_edit.rs` (Terminal.app for a terminal editor, a
    GUI editor directly) off the UI thread and flashes "Opened <file> in
    <editor>.".
  - `FocusSession` is **retired upstream**: `CommandKind::FocusSession`,
    `Effect::FocusSession`, `AgentItem`, `CommandModel::legacy_agents`, the
    `agent:` id, and the desktop's Go ▸ Agents submenu. It focused tmux-era
    "legacy spawned agents"; nothing has created one since tmux left, and
    neither client ever filled `legacy_agents`.
- **Wildcard-free in both clients.** The TUI's `dispatch_effect` had an
  `Effect::Mutate(other) => "not reachable from the palette"` catch-all; every
  `Mutation` now has its own arm (the board mutations and a reasoned reject
  go to the daemon off the UI thread, `AddProject` runs the form's
  validate-and-scaffold).
- **Headless scenario tests** (`crates/shelbi-app/tests/command_scenarios.rs`,
  13 tests): one per command, against a full `CommandModel` fixture. Each
  finds the command by its registry id, invokes it, and asserts the `Effect`
  and the model change through a reference executor that applies the
  client-local half to a `ClientState` and records host and daemon work. The
  input-collecting flows run through the shared machines (the review loader,
  the reject prompt, the add-project form). `scenario_for` matches
  `CommandKind` with no wildcard, so a new command doesn't compile until it
  has a scenario.
- **Smoke test per front end**, both in their workspace's `cargo test`:
  - TUI: `every_registry_command_runs_from_the_palette`
    (`crates/shelbi-tui/src/shell/mod.rs`): every registry command through
    Ctrl+Space, its title typed, the arrows onto it, Enter, with the frame
    drawn on `TestBackend` (the entry must be on screen), and the effect
    asserted at the executor (a test tap in `dispatch_effect`). The palette's
    disk reads moved behind a `PaletteInputs` seam so the test runs on a
    fixture, and `draw` split into `draw_frame<B: Backend>`.
  - Desktop: `src/app_smoke.rs`, a headless window in gpui's test context
    (`ShelbiWindow::build`, new: the window with no feed, bootstrap, or
    daemon) running every registry command through the palette (keystrokes)
    and through its menu item (`MenuInvoke`), asserting the host calls that
    reached the executor (a test tap in `run`).
- **The view-model field checklist**: `docs/parity.md`, a table per
  `shelbi_app::view` model naming where each client displays every public
  field (141 fields), guarded by `crates/shelbi-app/tests/parity_doc.rs` (in
  the root workspace, so CI enforces it). The tables came from a read of
  both renderers, field by field.
- **The hand walk** of every command in both clients, recorded in
  `docs/parity.md` with the platform differences and the gaps it found.
- **Fixes the audit made:**
  - TUI: the bare "Switch Project" switched the client to a project named
    `""`; it now opens the palette with its Projects column focused (the
    shared `Palette::focus_projects`).
  - TUI: the Machines view's workspace rows show what the desktop's do (the
    agent, the assignment with the task's title, the branch, a review slot's
    server state and URL) instead of only the observed task.
  - TUI: the review panel shows the slot's live server state and location
    (from the task's sidebar row on every refresh), as the desktop's Review
    view does.
  - Desktop: a done dependency gets a `✓` in the task detail (the TUI colors
    it green; the desktop is monochrome).
  - Desktop, debug builds: a window that drew its shell on its first frame
    (a second project's window, opened after bootstrap) tripped a gpui
    `debug_assert` in `is_action_available`; it now asks only after the
    first draw.
  - Desktop: the session-title map existed only for the stub's caption;
    removed.
- **Docs.** `desktop/README.md` is complete: status, every settings key, the
  parity checks, accessibility scope, build, run, signing, and release
  (packaging was already there). `desktop/docs/README.md` is the new status
  index, by phase. The repo `README.md` mentions the desktop app (install,
  and its build/test commands under Contributing), and `AGENTS.md` adds
  `desktop/`, its verification commands, and the parity rules for a new
  command or view-model field.

## Decisions

- **Retire `FocusSession` rather than give it an arm.** An arm would have
  had to attach a session by name with no source of names; the honest state
  is that the concept is gone. Removing it changes the shared API (a variant,
  a struct, a model field); nothing outside the two clients used it.
- **The TUI runs the editor in its own terminal.** That's what the tmux
  palette's `edit:*` openers did, and the run loop's input reader is on the
  same thread, so nothing competes with the editor for keys. Running the
  editor in a daemon session shown in the main area was the alternative; it
  would need a new view kind in both clients for little gain.
- **The scenario executor lives in the test, not the library.** Both clients
  already turn effects into `ClientState` changes in their own hosts; a
  library `apply_effect` neither calls would be test-only API. The test
  documents the contract; the smoke tests prove each client's path reaches
  the same effects.
- **The field guard reads source text**, not `syn` (no new root dependency,
  the root lockfile stays put). It parses top-level `pub struct Name {`
  blocks in `view.rs` and the structs `view.rs` re-exports from `review.rs`.
- **Gaps recorded, not all fixed.** The audit fixed what was cheap and
  clearly a client falling behind (above). The larger ones (the TUI board's
  missing Start/Assign/Edit/New, the TUI workspace header, the TUI board's
  missing expected state, the desktop's back-to-back mutation refusal) are
  listed in `docs/parity.md` as follow-ups. The desktop's monochrome look and
  the TUI's first-run hint are documented differences, not gaps.

## What's left

- The follow-ups under "Known gaps" in `docs/parity.md`.
- A daemon issue found during the walk: quitting the last open project from
  the TUI marked it closed, but the sandbox daemon idle-exited during the
  orchestrator-handoff wait and left the orchestrator session running (Quit
  Shelbi cleaned it up). The daemon's idle exit should wait for an in-flight
  quit.
- The desktop's Quit Shelbi wasn't pressed in the hand walk (the TUI's had
  already stopped the sandbox); the smoke test covers its path and its sheet
  is Quit Project's.
- The plan's Phase 7 also rewrites `Product/vision.md`, `principles.md`, and
  `positioning.md` in the ContextStore, and the public site ships with the
  release; both are separate tasks.
- Desktop CI stays manual (`desktop-ci.yml`), so "required" for the desktop
  smoke test means the desktop `cargo test` every desktop change runs before
  handoff; the TUI smoke test, the scenario tests, and the field guard run
  in the root CI on every PR.

## Verification

From `desktop/`:

- `cargo build`: ok.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 284 passed (includes `app::smoke`, 3 tests).

From the repository root:

- `cargo check --workspace --locked`: ok; `Cargo.lock` and
  `desktop/Cargo.lock` unchanged.
- `cargo build --workspace`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `env -u TASK_ID -u PROJECT cargo test --workspace`: 4071 passed, 0 failed
  (51 suites; includes `command_scenarios` 13, `parity_doc` 2, and the TUI
  smoke test). The pane's `TASK_ID`/`PROJECT` are unset because a CLI test
  reads them.

By hand: the command walk in `docs/parity.md`, both clients built from this
branch against a sandboxed Shelbi root (its own daemon, sessions, and tmux
server, the hub's socket variables unset; the live hub's daemon was checked
alive before and after). Screens were captured with `tmux capture-pane` (TUI)
and `screencapture -l` (desktop).
