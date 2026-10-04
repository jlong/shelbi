# rt-daemon-retire-units-not-hermetic — In review

Make the daemon's supervisor-unit retire step hermetic so a `cargo test` daemon
can never uninstall the developer's live hub daemon.

## What changed

- **Default-root gate** (`crates/shelbi-cli/src/commands/daemon/lifecycle.rs`):
  `retire_supervisor_units()` now returns early unless `should_retire_units()` is
  true. The gate allows retire only when the resolved shelbi root is the default
  installed one — `RootSource::CompileTime` (install-time bake) or
  `RootSource::HomeFallback` (`~/.shelbi`). Any explicit `--root` / `$SHELBI_ROOT`
  / `$SHELBI_HOME` (every test, and the confirmed offenders below, all set a temp
  `SHELBI_HOME`) resolves to a different source and makes retire a no-op: no
  `launchctl`/`systemctl` call, no unit file deleted. This is the single
  mechanism that covers every daemon spawn site automatically.
- **Opt-out env** `SHELBI_NO_RETIRE_UNITS`: forces the skip regardless of root,
  as belt-and-braces. Set in the three test harnesses that spawn `shelbi daemon`.
- **Testable seam**: unit-file paths + the deactivate/reload invocation now sit
  behind a `RetireHost` trait. `retire_units_with(host, labels)` holds the
  deactivate → remove → reload logic; `RealLaunchd`/`RealSystemd` are the real
  impls, and tests drive it against a temp dir + recording fake.

## Tests

- Seam: `retire_units_with` removes present files, deactivates every label, runs
  the post-removal reload once; idempotent no-op when files are absent.
- Gate: skipped under `$SHELBI_ROOT`, under `$SHELBI_HOME`, and under
  `SHELBI_NO_RETIRE_UNITS`; allowed on the default root. (Gate tests assert only
  the boolean — they never call `retire_supervisor_units()` — so they can't touch
  the live unit.)
- Regression: with a fake `$HOME` holding a sentinel
  `dev.shelbi.daemon.plist` and a temp `$SHELBI_ROOT`,
  `retire_supervisor_units()` returns empty and leaves the sentinel untouched.

Verified the real `~/Library/LaunchAgents/dev.shelbi.daemon.plist` survived
running `control_socket`, `daemon_lifecycle`, and `session_restarts_daemon` — it
would have been deleted before this change.

## Spawn sites covered (orchestrator-confirmed offenders)

All set a temp `SHELBI_HOME` but inherit the real `$HOME`, so `dirs::home_dir()`
resolved the live plist. Each now also carries `SHELBI_NO_RETIRE_UNITS=1`:

- `tests/control_socket.rs` (`start_daemon`)
- `tests/daemon_lifecycle.rs` (`Home::spawn`)
- `tests/session_restarts_daemon.rs` (`start_session` — inherited by the
  watchdog-spawned daemon — and the explicit second-daemon spawn)

## Audit of other daemon-startup side effects

The only real-path (non-root-scoped) side effect in the daemon startup path
(`serve::run_foreground`) was `retire_supervisor_units()`. The remaining startup
steps — `prune_stale_control_masters`, `reconcile_forward_modes`,
`config_upgrade::run_startup_pass`, `apply_login_shell_env` — operate on the
resolved `$SHELBI_HOME` (isolated in tests) or only read env. `dirs::home_dir()`
appears nowhere else under `crates/shelbi-cli/src/commands/daemon/`. No other
leak found.
