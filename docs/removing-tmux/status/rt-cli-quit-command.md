# rt-cli-quit-command

**Status:** complete.

Restored `shelbi quit` as a thin client over the daemon control socket, after
`rt-cutover-delete` removed the tmux-era command.

- New `crates/shelbi-cli/src/commands/quit.rs`: `shelbi quit` sends
  `ClientMsg::QuitProject` for the resolved current project; `shelbi quit --all`
  sends `QuitShelbi`. Both go through `shelbi_client::ControlClient` and block on
  the daemon's ack.
- The orchestrator handoff and close-before-end ordering are **not**
  reimplemented — the daemon's control handler already invokes
  `shelbi_orchestrator::quit::{quit_project, quit_shelbi}`, which do the handoff
  and mark projects closed.
- No daemon running → `daemon_lock_held()` is false → reports "nothing is
  running" and exits 0, for both forms.
- IO behind a `QuitOps` seam; 5 unit tests cover current-project quit, `--all`,
  the nothing-open gate (both forms, no project resolution, no socket call), and
  error propagation.
- Wired into `main.rs` (`Cmd::Quit { all }`); `shelbi --help` lists `quit`.

No `Cargo.lock` change (no new deps). `cargo build`/`clippy -D warnings` green on
the cli crate; quit tests pass.
