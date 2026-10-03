# rt-session-process — status

**Landed.** The `shelbi __session` process: one detached process per session
owning one PTY and one headless `alacritty_terminal` emulator.

## What shipped

- New crate `crates/shelbi-session`:
  - `spawn::spawn_detached` — launch a session detached (setsid, or
    `systemd-run --user --scope` on Linux/logind, stdio to `/dev/null`) with an
    explicit scrubbed environment.
  - `session::run` — the `shelbi __session` body: own the PTY + emulator, answer
    terminal queries with no client attached, serve clients on a Unix socket
    (frozen-core subset: hello / attach / input / resize / snapshot / kill), and
    on child exit write `exit.json` + `final.txt`.
  - `layout` (`~/.shelbi/sessions/<short-id>/` with `sock`, `lock`, `meta.json`,
    `exit.json`, `final.txt`, `raw.log`; short-id keeps the socket path < 104),
    `lock` (lifetime flock → liveness), `responder` (ported from the Phase 0
    `rt-spike-agents` responder), `emulator` (vendored `alacritty_terminal`,
    kitty keyboard enabled, 10k scrollback), `history` (recent-bytes ring +
    optional raw log), `transport`.
- `shelbi __session` hidden CLI subcommand → `shelbi_session::run`.
- `shelbi_core::session_child_env` / `build_session_env` — the shared helper that
  scrubs `TMUX`/`TMUX_PANE`/`TERM_PROGRAM`/`STY` and sets
  `TERM=xterm-256color` / `COLORTERM=truecolor` / `TERM_PROGRAM=shelbi` on top of
  the cached login-shell env (`login_shell_env`). The daemon reuses this.
- `shelbi_core::SessionConfig` + `Project.session` — the per-project
  `raw_output_log` toggle (default off). Added to `SHARED_PROJECT_FIELDS`.

## Out of scope (owned elsewhere)

- Attach **replay** (reconstructing full emulator state) → `rt-replay`.
- The full protocol + additive capabilities → `rt-protocol-client`.
- The `SessionBackend` seam / production spawn call sites → Phase 2.
- The "session restarts the daemon" periodic check (plan "One process per
  session") → left to the daemon-lifecycle subtask; not wired here.

## Config-upgrade note

`Project.session` is a new **optional** config key that defaults to off when
absent. There is no shipped `*.template` or default-config change, so existing
installs need no config-upgrade sniffer — a project simply opts in by adding
`session: { raw_output_log: true }` to its YAML.

## Round 2 — Linux CI hang fix

Round 1 passed review but PR #1451's Linux `build` job hung in the Test step.
Cause: `spawn_detached` preferred `systemd-run --user --scope` whenever
`systemd-run` was merely present on `$PATH`. A GitHub `ubuntu-latest` runner has
`systemd-run` but no `systemd --user` manager, so `systemd-run --user` fails or
blocks and the survival tests waited forever.

Fixes:
- `spawn.rs`: the systemd path is now gated on a **reachable** user manager —
  `$XDG_RUNTIME_DIR/systemd/private` must exist (cheap, created only while
  `systemd --user` runs) **and** a `systemctl --user is-system-running` probe
  with a hard 2s deadline must answer anything but `offline`. Otherwise fall
  back to the always-safe `setsid`. The probe is killed/reaped on timeout, so it
  can never hang the spawn.
- `session.rs`: `kill_child_group` now additionally refuses our own process
  group (not just pgid ≤ 1), so a child that failed to `setsid` can never take
  the session — or a test harness — down.
- `tests/session_integration.rs`: the previously unbounded `run()` thread joins
  now have a 10s hard deadline (`join_run` panics loudly; the `Drop` backstop
  uses the non-panicking `join_within`), so a broken kill path fails fast with a
  clear message instead of hanging the job.

CI result (PR #1451 `build` job): round-2 fix did **not** resolve the hang.

## Round 3 — the real cause: the systemd detach path + an unguarded test kill

Round 2 still failed. Reproduced the full `cargo test --workspace` in a
`rust:latest` Linux container: it **completed** — so the failure is specific to
GitHub's `ubuntu-latest`, not the `setsid` path a plain container takes. A
temporary single-thread `--nocapture` CI diagnostic then named the exact
offender: the whole `cargo test` process was `Killed` (exit 137) the instant
`detached_session_outlives_its_launcher_and_keeps_serving` (the `session_survival`
test) started.

Root cause, two parts:

1. GitHub's `ubuntu-latest` **does** run a reachable `systemd --user` manager, so
   round 2's "reachable user manager" gate *passed* and took the
   `systemd-run --user --scope` path — the opposite of the intended CI behavior.
2. That path had **no `setsid`**, so the spawned `systemd-run` stayed in the
   launcher's process group. The survival test's cleanup
   `killpg(getpgid(spawned.pid))` therefore SIGKILLed the launcher's own group —
   i.e. the `cargo test` runner itself (exit 137). The container never hit this
   because it has no user manager and took `setsid`, where the pid is its own
   session leader.

Fixes:
- `spawn.rs`: the `systemd-run` scope path is now **opt-in**
  (`SHELBI_SESSION_SYSTEMD_SCOPE`) *and* still gated on a reachable manager —
  never auto-selected, because a reachable manager does not imply a host where
  the scope path is safe (CI is the counterexample). Default everywhere (tests,
  CI, macOS) is the always-safe `setsid` recipe. Both recipes now `setsid`, so
  the returned pid is always a session leader and `killpg(getpgid(pid))` can
  never reach the launcher's group. `spawn_detached` has no production callers
  yet (Phase 2 wires them), so defaulting to `setsid` has no runtime impact now.
- `tests/session_survival.rs`: the cleanup `killpg` now refuses pgid 0/1 **and**
  our own group, falling back to a single-pid kill — a mis-aimed group kill here
  is exactly what took the runner down.
- `login_env.rs` (defensive, not the cause here): `capture_login_shell_env` ran
  `$SHELL -l -i -c env` via `Command::output()`, which blocks on stdout EOF; a
  login rc that backgrounds a pipe-holding process could wedge it forever.
  Replaced with a hard-deadline `bounded_capture_stdout` (own process group,
  threaded read, channel with a 10s timeout → empty map on timeout, which
  callers safely overlay). Regression test
  `bounded_capture_does_not_hang_when_a_background_child_holds_the_pipe`.
- `.github/workflows/app-ci.yml`: each Test attempt now runs under a hard
  `timeout` so any future wedge fails the step fast instead of hanging the
  runner for the job's full length (permanent safety net; the single-thread
  diagnostic was removed).

CI result (PR #1451 `build` job): _pending push + watch._
