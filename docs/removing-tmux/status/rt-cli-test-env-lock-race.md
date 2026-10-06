# rt-cli-test-env-lock-race

Status: **done** — `cargo test -p shelbi --bin shelbi` now green across repeated
high-parallelism runs (was ~50% failing with 38 failures per bad run).

## Real root cause (differs from the task's hypothesis)

The task guessed a `SHELBI_HOME` lock split (board's `IsolatedHome` on `ENV_LOCK`
vs other modules on "a different lock"). That isn't what's happening: **every**
`set_var("SHELBI_HOME"…)` site in `crates/shelbi-cli` already serializes on the
one shared `crate::commands::test_support::ENV_LOCK` (audited via
`git grep -n 'set_var("SHELBI_HOME' crates/shelbi-cli` + the `ENV_LOCK`/`TEST_LOCK`
references). There is no second env lock.

The actual seed was a **process-global `umask` race**, not an env/lock race:

- `daemon::control::bind()` (and the hub-socket bind in `daemon::serve`) tighten
  the process umask to `0o177` around `UnixListener::bind()` so the socket inode
  lands `0600` before the follow-up `chmod`. `umask(0o177)` clears the owner
  **execute/search** bit.
- `umask` is process-global. The control-socket test helper `serve_bg` calls
  `bind()` **without** holding `ENV_LOCK`, so its umask window overlaps other
  tests running in parallel. Any directory a concurrent thread creates in that
  window is born `0o600` — no search bit — so the next `create_dir`/write inside
  it fails `EACCES (os error 13)`.
- The board daemon tests create `<home>/projects/<proj>/…` on every refresh, so
  they were the ones that tripped (`…/projects/proj: Permission denied`). The
  seed test panics **while holding `ENV_LOCK`**, poisoning it; every later test
  that acquired the lock with a bare `.unwrap()` then failed with `PoisonError`
  — that's the 1-real-plus-37-cascade signature.

Confirmed by bisection: board tests alone = 0/10 fail; board + control together
= 2/12 fail; full suite ≈ 50%.

## Fix

1. **Root cause** — `umask(0o177)` → `umask(0o077)` in `daemon/control.rs::bind`
   and `daemon/serve.rs` (the hub bind). `0o077` masks only group/other, so the
   socket is still created `0600` (identical security outcome; owner-x is
   meaningless for a socket) while directories created concurrently in the same
   process keep their `0700` search bit. This removes the race for *all*
   concurrent filesystem work, not just `ENV_LOCK` holders.
2. **Poison tolerance (task req 2)** — the 51 remaining
   `TEST_LOCK.lock().unwrap()` acquisitions (agent/issue/status/workflow/
   workspace) now use `.lock().unwrap_or_else(|p| p.into_inner())`, matching the
   rest of the binary. Defense-in-depth: a single real failure under the lock
   now reports as **one** failure, never a 38-wide `PoisonError` cascade.

Task req 1 (one shared lock) was already satisfied; verified, not changed.

## Does the same race exist on `main`?

- The `daemon::control::bind()` path (the test trigger via `serve_bg`) is
  **remove-tmux-only** — the daemon control socket does not exist on `main`, so
  `main` does **not** exhibit this flake.
- `daemon/serve.rs`'s `umask(0o177)` *does* exist on `main`, but only on the
  production `run_foreground()` path (not unit-test-reachable there), so it does
  not flake `main`. The `0o077` change still lands on `main` when remove-tmux
  merges and is a harmless improvement.
- The poison-fragile `TEST_LOCK.lock().unwrap()` pattern also exists on `main`
  (latent — no current seed to trigger a cascade). Porting the
  `unwrap_or_else(into_inner)` hardening to `main` is a reasonable follow-up for
  the orchestrator to file, but `main` is not currently flaky from it.

## Verification

- `cargo build -p shelbi`, `cargo clippy -p shelbi --all-targets` clean.
- `cargo test -p shelbi --bin shelbi --test-threads=16` run ≥16× in a row, all
  green (was failing ~half the time before).
