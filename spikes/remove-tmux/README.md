# remove-tmux spikes

Throwaway prototypes for the ["Removing tmux"](../../docs/removing-tmux/README.md)
work. **Not part of the cargo workspace** (the repo root `Cargo.toml` lists
`spikes` under `exclude`), so nothing in here affects `cargo build --workspace`,
`cargo test --workspace`, or `cargo clippy --workspace`.

This directory exists for the Phase 0 spike subtasks to prove out the real risks
before the production crates commit to a design:

- `rt-spike-emulator-replay` — serialize full emulator state and rebuild it in a
  second emulator; decides the emulator crate.
- `rt-spike-agents` — Claude Code (full-screen and inline) and Codex in a PTY
  rendered by a ratatui widget: keys (incl. Shift+Enter), mouse, paste, wide
  characters, redraw cost.
- `rt-spike-runtime` — process survival (outliving the launcher on macOS and
  Linux/logind; the child group dying when it should), the query responder, and
  nesting inside tmux and Screen.

## Conventions

- One subdirectory per spike (e.g. `emulator-replay/`, `agents/`, `runtime/`).
- A spike may carry its own `Cargo.toml`; because `spikes` is excluded from the
  workspace it builds standalone (`cargo build --manifest-path
  spikes/remove-tmux/<spike>/Cargo.toml`).
- Keep it throwaway. Findings go to `docs/removing-tmux/phase0/<spike>.md`, not
  here. This whole tree is deleted at cutover (`rt-cutover-delete`).
