# rt-daemon-detects-a-lost-macos-login-session-and-restarts-itself-or-warns

Status: implemented.

The hub daemon now detects when it has lost its macOS GUI login session (user
logged out/in, or the session restarted after a crash/update) and recovers or
warns instead of silently starting agents with a dead bootstrap context (no DNS,
no user lookups, a stale `SSH_AUTH_SOCK`).

Signals: `launchctl managername != Aqua` is the decisive, debounced signal (local,
never flaps on a network blip); DNS resolution and the inherited `SSH_AUTH_SOCK`
are corroborating only. Declared lost after 3 consecutive bad readings.

On loss: emit `daemon session-lost reason=…`, block new agent spawns
(`shelbi_orchestrator::session_guard`), write a durable marker, and re-exec into
the live GUI session via `launchctl asuser <uid> <self> daemon` (detached session
processes survive, so running work is not killed). If the re-exec can't run (or
was already tried), fall back to the warning. `shelbi status`, `shelbi doctor`,
and a persistent red top banner in every TUI view surface the condition.

Linux/other: no-op. Tests inject the probe + recovery seams; the real monitor is
only spawned under the default installed root, so no test touches real
`launchctl`, `HOME`, or `~/Library/LaunchAgents`.

Rework (2026-10-09): Linux CI clippy flagged the macOS-only items as dead code.
Fixed by splitting `session_health.rs` into cfg'd submodules instead of a blanket
`#[allow]`: the platform-agnostic decision core (`RecoveryMode`, the
`SessionRecovery` seam, `SessionHealthMonitor`) now lives in a
`#[cfg(any(target_os = "macos", test))]` `monitor` module, so the unit tests still
exercise it on every OS; the real launchd plumbing (env config, `RealSessionRecovery`,
the spawn loop, the enablement gate) lives in a `#[cfg(target_os = "macos")]` `imp`
module; the only cross-platform item left at file scope is the no-op-on-Linux
`spawn_session_health_monitor` entry point. Verified green with
`cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings`
and native macOS clippy + the 7 unit tests.
