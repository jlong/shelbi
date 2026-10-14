# dt-palette-overlays: the command palette and the overlays

## What landed

- **One overlay layer** in the project window (`src/views/overlay/mod.rs`):
  the window holds at most one `ActiveOverlay`, mirrored into its
  `ClientState` as a `shelbi_app::nav::Overlay` (which gains `AddProject`).
  It draws over the whole window on a dimmed backdrop that swallows the
  mouse; while it is open it takes every key except the app's own ⌘
  shortcuts (⌘W, ⌘Q, ⌘,). Esc closes any overlay and focus goes back where
  it was (the main view, e.g. the board, or the sidebar). A click on the
  backdrop closes the palette and the error log; the dialogs holding typed
  input or a decision ignore a stray click.
- **Palette** (`src/views/palette.rs`): ⌃Space, ⌘K, or the `open_palette`
  binding from `keys.yaml` (default ⌃P), and the sidebar's search box. It
  lists `CommandRegistry::entries` for the window's `CommandModel` through
  `shelbi_palette::search`, with the TUI's look: top-aligned 40px below the
  window top, 70% wide, as tall as the results (scrolling inside once it
  reaches the window's height), the muted border, the `❯` line with the green
  block cursor or the dim placeholder, rows of icon / 211px label / dim
  description / shortcut with the selected row on the `selection` pill, the
  Projects column (the active project's disc breathing, open projects green,
  closed gray, "+ Add project"), and the footer hint naming the user's chord.
  Enter (or a click) runs the entry: its id goes through
  `CommandKind::from_id` into the window's normal `run`, so the relaunch and
  bootstrap gates apply. The chord that opened it closes it; Tab hands focus
  to the sidebar.
- **Error log** (`OpenErrorLog`, also the sidebar's unread button): the TUI's
  rows (newest first, bold `●` on entries unread when it opened, local
  timestamp, wrapped message), read and marked read off the UI thread, with
  Copy (button or ⌘C) and Clear (button or `c`). The sidebar's unread count
  clears on the next refresh.
- **Load for review** (`overlay/review_confirm.rs`): `LoadReview` (a palette
  `review:<id>` entry, a review row in the sidebar) resolves off the UI thread
  whether the task is serving or queued. Queued: the TUI's dialog in its three
  shapes (no review workspace / every slot busy, the one-slot confirm, the
  slot picker); Load sends the daemon's `ReviewSessionOp::Load`. Serving: the
  review view is `dt-review-view`'s, so it still shows that stub.
- **Reject reason** (`overlay/review_reject.rs`): the multi-line prompt
  (Enter newline, Ctrl-D or `[ Reject ]` submits, a blank reason never does),
  sending `RejectReview { reason }`. ⌘V pastes, line breaks included.
- **Approve**: `ApproveReview` runs straight from the palette, with no dialog,
  as the TUI's review panel does.
- **Zen**: `ToggleZen` checks off the UI thread whether the first-run intro is
  due (off-to-on and `zen_intro_seen` unset); the intro (`overlay/zen_intro.rs`)
  persists "Don't show this again" and toggles on Enable. Either way the toggle
  runs off the UI thread and the sidebar's Zen band follows on the refresh it
  requests.
- **Add project** (`overlay/add_project.rs`): the TUI's form (name, repo path
  prefilled with the working directory, or the home directory when the app was
  opened from Finder, the in-repo / global radio, the "Detected:" line, the
  "Folder / id:" preview, the inline error). Submit builds
  `Mutation::AddProject { name, root, mode }` and runs it off the UI thread
  (`commands::run_add_project`); a validation error goes back on the form, and
  success opens the new project's window.

### Shared crates

- **`shelbi_app::overlay`** (new): the six overlays' state machines, moved
  out of `shelbi-tui/src/overlay` and keyed on `shelbi_state`'s `KeyChord`
  instead of crossterm's `KeyEvent`: `palette::Palette` (plus
  `ProjectIndicator` / `ProjectEntry`), `error_log::Viewer` (plus `wrap` /
  `format_ts`), `review_confirm::Dialog`, `review_reject::RejectPrompt`,
  `zen_intro::{IntroState, step_intro}`, `add_project::Form`. Clicks are
  methods (`click_item`, `click_load`, ...); the hit-testing stays with each
  renderer. Also `palette_open_chords` / `opens_palette`: the configured
  `open_palette` chords plus the reserved ⌃Space and ⌘K.
- **The TUI's overlays are now thin**: each keeps its crossterm-facing API and
  its ratatui render, converts the key with `keymap::chord_from_event`, and
  delegates. Their tests still pass unchanged in intent (the few that poked
  private fields now go through the public API; the decision-table unit tests
  that moved are covered in `shelbi-app`).
- **`shelbi_app::command_model`** (new): `build_command_model`,
  `build_projects_column`, `edit_targets`, `nav_glyph`, moved from
  `shelbi-tui/src/shell/overlays.rs` (which now calls them), split into IO and
  pure halves (`command_model_from`, `projects_column_from`) so the desktop
  reads off its UI thread and the order is tested against fixtures.
- **`shelbi_app::zen`** (new): `should_show_intro`, `toggle`, `apply_intro`,
  moved from the TUI shell.
- **Registry**: per review task, `action:approve-review:<id>` ("Approve
  <title>") and `action:reject-review:<id>` ("Reject <title>", an empty
  reason the host collects), hidden until typed. The TUI handles both too:
  approving the review open in its panel goes through the panel, any other
  runs off the UI thread; reject opens its reason prompt.
- **`Mutation::AddProject`** gains `mode: ConfigMode`.
- **`shelbi_orchestrator::project_create`**: `detect_root` (the form's
  "Detected:" line) and `add_project` (validate + scaffold, returning the slug
  or `AddProjectError::{Invalid, Failed}`).

## Decisions

- **Shared state machines, not a port.** The TUI's overlays were half state
  machine, half crossterm; the state machines moved to `shelbi-app` behind
  `KeyChord`, which both clients already produce, so the palette's filtering,
  ordering and every overlay's keys are one implementation.
- **The desktop drives the overlays' own text buffers**, not gpui-kit inputs,
  so the reject prompt and the form edit exactly as in the TUI.
  `keys::overlay_chord` turns a keystroke that types a character into that
  character; ⌘V pastes (a single-line field drops line breaks so a paste
  can't submit). There is no IME composition in these fields.
- **"Review confirm" is the TUI's Load-for-review dialog** (confirmed with the
  orchestrator; the task body named it the approve dialog): the TUI approves
  with no confirmation, so the desktop does too.
- **Palette inputs are read off the UI thread.** The palette opens at once
  from the last read (other projects, edit targets, the Projects column,
  whether another client is attached) and updates when a fresh read lands; it
  also follows each sidebar snapshot while open, keeping the query.
- **Results go to the status note, failures to the banner.** A transient
  "Zen Mode enabled", "Approved T-1." or "Loading T-1 onto review…" shows for
  four seconds; a failure stays in the yellow banner.
- **⌘K is reserved alongside ⌃Space** rather than added to the shipped
  `open_palette` default: macOS takes ⌃Space for input-source switching on
  many setups, and changing a shipped keymap default would need a
  config-upgrade sniffer for existing installs. `keys.yaml` still rebinds the
  configured chord.
- **`OpenEditor` stays a stub**: the TUI doesn't open editors from the palette
  yet either.

## What's left

- The review view (`dt-review-view`): a serving review's `LoadReview` still
  shows the stub, and after a queued load the window doesn't open the review
  on the daemon's `ReviewOpened` (the TUI does).
- `OpenEditor` (both clients).
- IME composition in the overlays' text fields.

## Rebase onto dt-chat-view

`dt-chat-view` (#1695) landed on the umbrella while this was in flight. The
one conflict was `app.rs`: both branches added window fields (kept both:
`chat` and the overlay state), and the chat branch had added interim
"Not implemented yet: action:palette" arms for the palette chord and
Ctrl+Space, which the real palette replaces. The chat view's capture-phase
`takes_from_terminal` now asks `shelbi_app::overlay::opens_palette`, so every
palette chord (⌘K included) is kept from a focused terminal and opens the
palette; its test covers `cmd-k`.

## Rebase onto dt-board-dnd

`dt-board-dnd` (#1696) landed next. Its conflict was `commands.rs`: the board
arm gained `PrioritizeIssue` while this branch replaced the review stubs; both
kept (`PrioritizeIssue` runs on the board, approve / reject go to the window).

## Rebase onto dt-activity-machines and dt-packaging

`dt-activity-machines` (#1699) conflicted only by both sides adding lines
(`shelbi_app` module list, `app.rs` fields and imports, the `commands.rs`
module doc); all kept. `dt-packaging` (#1700) rebased cleanly and touches no
shared crate.

## Verification

Run locally on an arm64 Mac (Apple M4), 2026-10-14, at a hub load average of
55 to 105. Final base: `b67eaed5` (dt-packaging). On it: the desktop's
`cargo build`, `cargo clippy --all-targets -- -D warnings` and `cargo test`
(180 passed), and the root `cargo clippy --workspace --all-targets -- -D
warnings` and `cargo check --workspace --locked`, all clean. The root suite's
last full run was on `812381dd` (dt-activity-machines; dt-packaging changes no
crate): see below.

From `desktop/`:

- `cargo build`: clean (only the upstream `block 0.1.6` note).
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 180 passed on the final base (151, 159 and 170 on the
  earlier ones, as each sibling's tests arrived). New: the overlay holder's key routing for every
  overlay (`views::overlay::tests`), gpui render tests
  (`views::overlay::render_tests`: the palette draws a fixture state's
  registry entries in order with hidden-until-typed ones absent, the selection
  on the first row, the Projects column and footer, top-aligned; a click runs
  a row; the backdrop closes the palette but not a form; each dialog draws its
  controls; the error log marks unread rows and Clear resolves; clicking the
  Zen and Load buttons resolves them), the palette's geometry and icon rules
  (`views::palette`), overlay key conversion and paste (`keys`), and the
  updated dispatch table (`commands`).

From the repository root (shared crates changed):

- `cargo build --workspace`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo check --workspace --locked`: resolves; root `Cargo.lock` unchanged.
- `cargo +1.88 check --workspace --all-targets --locked`: clean.
- `cargo test --workspace --no-fail-fast` (pane env stripped), on
  `812381dd`: 3965 passed, 4 failed, every one a `shelbi-cli` test that
  spawns a real `shelbi __session` and waits for its socket or snapshot:
  `old_session_compat`, `session_survival`, `remote_session_e2e`,
  `session_backend_e2e`. On `a2790602` it was 3957 passed / 1 failed
  (`old_session_compat`); on `259540b6` 3946 / 2, and those two still failed
  when rerun alone. They wait 10s for the session's socket; a hand-run session in a scratch home logged "interactive login-shell
  env capture failed or timed out after 10s" before binding, so the login
  shell's env capture, not this branch (which touches no session or
  login-env code), eats the window under this load. Before the rebase the
  same suite gave 3939 passed with `remote_session_e2e` and `session_survival`
  failing the same way, and both passed on a rerun. Left to CI. Every
  `shelbi-app`, `shelbi-tui`, `shelbi-orchestrator` test passes, including the
  new ones: the shared overlay machines (`shelbi_app::overlay`, 43 tests), the
  fixture palette order (`shelbi_app::command_model`), the registry's review
  decisions, the TUI's palette reject entry, and
  `project_create::add_project` / `detect_root`.

By hand, the real app (`desktop/target/debug/shelbi-desktop`) against a
contained `SHELBI_HOME=/tmp/shpo` (one scratch project, sockets unset,
`desktop.cli_path` at this worktree's `shelbi`), driven with CGEvent keys:
⌃P opened the palette with the default list in the TUI's order and the
Projects column; typing `err` filtered to "Open error log" and Enter opened
the error log ("No errors logged."); Esc, then `zen` + Enter showed the
first-run intro, and Enable turned Zen on (`state.json` `zen_mode: on`, the
sidebar's ZEN MODE ON band, the "Zen Mode enabled" note); "Add project" opened
the form, Shift-typed "Beta", a ⌘V-pasted repo path was detected as a git
repo with the `beta` slug preview, and renaming to the taken `alpha` showed
the inline "a project already lives at `alpha`" error. (A real create was left
to the unit test: the scaffold's default runner would start a real agent.)
The backdrop was too faint in the light theme, so it now dims with black.
Closed by killing the app's own pid; no sandbox daemon had started.
