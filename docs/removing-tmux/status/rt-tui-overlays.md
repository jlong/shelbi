# rt-tui-overlays — In review

Phase 4d: the five popup processes become in-process overlays in the
single-process TUI, sharing one implementation with the tmux popups (which stay
until cutover). Behind the hidden `session_backend` dev flag; the tmux runtime
is unchanged.

## What landed

- **New shared home `crates/shelbi-tui/src/overlay/`** — one implementation per
  overlay (`palette`, `error_log`, `review_confirm`, `review_reject`,
  `zen_intro`): the pure state machine + a `render` into a caller-supplied
  `Rect`, no terminal ownership / event loop / result IO.
- **`shelbi-cli` popups are now thin wrappers** over those types
  (`commands/{palette,error_log,review_confirm,review_reject,zen_intro}.rs`):
  they keep their terminal setup, event loop, and temp-file/exit-code contract,
  but render and decide through the shared code. `shelbi __palette`, the tmux
  `bind-key`, `to_tmux_key`, `tmux_palette_key`, and `shelbi popup` are all
  intact (deleted at cutover). The palette's render stack (`render`,
  `render_projects_column`, `selection_style`, the project indicator/pulse
  helpers) moved to `overlay::palette`; the tmux palette renders through a small
  adapter that projects its `State` into the shared `PaletteView`.
- **Shell integration `crates/shelbi-tui/src/shell/overlays.rs`** — the runtime
  `ActiveOverlay` holder: routes keys/mouse to the open overlay, draws it over a
  dimmed main area, and yields an `OverlayEvent`. Builds the palette's
  `CommandModel` from the live sidebar model + light disk reads, so the palette
  lists/runs the shelbi-app command registry (typed args, availability,
  `hidden_until_query`), not string-prefix dispatch.
- **`shell/mod.rs` wiring** — Ctrl+Space (∪ the configured `OpenPalette` chord)
  opens the palette from anywhere, incl. a focused terminal view; in the palette
  Esc returns to the agent and Tab moves focus to the sidebar. The interim
  Ctrl+Space focus-toggle from rt-tui-shell is removed. Blocking work (the Zen
  toggle) runs off the UI thread and reports on a status line.

## Scope boundaries (deliberate, for later Phase 4 subtasks)

- The **review confirm / reject** overlays are complete and unit-tested (they
  return the chosen slot / typed reason as a *value*, no temp file), but the
  shell has no trigger to open them yet — the review interface opens them in
  **rt-tui-review (4e)**. Marked `#[allow(dead_code)]` with a pointer.
- Palette commands whose home is **4e/4f** — load-review, switch/add/quit
  project, open-in-editor, focus legacy session — produce the correct effect and
  surface a status note rather than silently doing nothing.

## Palette-open chord decision

Plan decision table says "One chord, Ctrl+Space, opens the palette", but the
shipped `global.open_palette` default is `ctrl-p` (it drives the tmux
`bind-key`). Changing the embedded default would change tmux behavior, which is
off-limits on this branch. Resolution: the shell opens the palette on the
**union** of {configured `OpenPalette` chord} ∪ {Ctrl+Space}; the embedded
default stays `ctrl-p` so tmux is untouched; a cutover subtask flips the default
and drops the union.

## Tests

Per-overlay unit tests moved with the code; added shell-level tests for the
palette open/close/focus model, the registry-sourced entry list, and the review
confirm/reject value-returning path. `cargo clippy --workspace --all-targets --
-D warnings` and `cargo build --workspace` are green.
