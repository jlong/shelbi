# Phase 0 findings

Each Phase 0 spike writes its findings here, one file per spike:

- `emulator-replay.md` — `rt-spike-emulator-replay`: full-state serialize/replay
  across two emulators, and **the emulator-crate decision**
  (`alacritty_terminal` vendored vs. a `vt100`-family crate).
- `agents.md` — `rt-spike-agents`: Claude Code (full-screen + inline) and Codex
  in a ratatui-rendered PTY — keys incl. Shift+Enter, mouse forwarding, paste,
  wide characters, redraw cost under heavy output.
- `runtime.md` — `rt-spike-runtime`: process survival (outliving the launcher on
  macOS and Linux/logind; child group dies on kill), the startup query
  responder, and nesting inside tmux and Screen.

A findings file should state, per item tested: what worked, what did not, and
whether each failure is fixable (and how). The phase's exit criteria are that
written list plus the emulator decision.

The prototypes themselves live under `spikes/remove-tmux/`, which is excluded
from the cargo workspace. This whole effort's spike tree is deleted at cutover
(`rt-cutover-delete`).
