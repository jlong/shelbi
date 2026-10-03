# `rt-spike-runtime`

Phase 0 spike for process survival and nesting (plan items 5 and 6, plus the
`portable-pty` checks). Throwaway; **not part of the cargo workspace**. Findings
live in [`docs/removing-tmux/phase0/runtime.md`](../../../docs/removing-tmux/phase0/runtime.md).

```sh
# all checks
cargo test --manifest-path spikes/remove-tmux/runtime/Cargo.toml

# nesting observations, printed and written to target/nesting-findings.txt
cargo test --manifest-path spikes/remove-tmux/runtime/Cargo.toml --test nesting -- --nocapture
```

## What is where

- `src/session.rs` - the `session` subcommand: a `shelbi __session` stand-in. One
  PTY via `portable-pty`, a child in its own process group, a status file and a
  liveness marker, and a `SIGTERM` handler that kills the child's whole group.
- `src/detach.rs` - the `detach-spawn` subcommand: the launcher. `setsid(2)` plus
  stdio to `/dev/null`, then returns at once (models a launcher, or `ssh host
  ...`, walking away from a session that must keep running).
- `src/probe.rs` - the `probe` subcommand: a tiny TUI stand-in that queries the
  kitty keyboard protocol, reports the `TERM`/`COLORTERM` it was handed, and
  emits OSC 52, for the nesting test to run inside tmux and Screen.
- `tests/survival.rs` - launcher-exit survival, the SSH-safe stdio redirect, and
  group kill.
- `tests/pty.rs` - `portable-pty` controlling terminal, process group, group
  kill, and descriptor-leak behavior, exercised against the library directly.
- `tests/nesting.rs` - runs `probe` inside tmux and Screen with the test
  emulating a kitty + truecolor outer terminal; a no-multiplexer baseline
  validates the harness.

Unix-only, by design (the effort targets macOS and Linux). The nesting test uses
a private tmux socket and a dedicated Screen session, so it never touches a live
multiplexer server.
