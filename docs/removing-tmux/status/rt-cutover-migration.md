# rt-cutover-migration (Phase 6: Cutover, part 1)

**Status:** ready for review.

Flip the runtime default to the session-process backend and migrate existing
installs safely, workspace by workspace.

## What landed

- **Defaults flipped (verifiable both ways).** `session_backend_enabled()` and
  `daemon_mutations_enabled()` now default **on**; the single-process TUI is
  selected by the same `session_backend` flag, and the daemon poller was already
  on. `DevConfig` gets a hand-written `Default` (both `true`) plus
  `#[serde(default = "default_true", skip_serializing_if = "is_true")]` so an
  untouched `shelbi.yaml` stays clean and an explicit `false` still restores the
  tmux path for a bisect. The hidden setting stays until `rt-cutover-delete`.
- **Per-workspace migration state** (`shelbi-state::migration`): a
  `MigrationState { Pending, Migrated }` persisted in the project's `state.json`
  (`State.workspace_migration`, omitted when empty). New `append_migration_event`
  logs every step on `events.log`.
- **Migration logic** (`shelbi-orchestrator::migration`), all behind a
  `MigrationProbe` seam (prod = exact-match `shelbi-tmux` / `shelbi-ssh`; tests
  stub it, so nothing touches the real tmux server):
  - `ensure_project_openable` — refuses to open a project with a surviving local
    `shelbi-<p>` / `_shelbi-<p>` tmux session, telling the user to `shelbi quit`
    first (prevents a second agent in the hub worktree and two pollers at once).
    Wired at the top of `run_main`.
  - `run_migration_pass` — walks every workspace: a local workspace migrates
    (the open gate proved its session gone; tmux-absent also migrates), a remote
    migrates only once the hub reaches its machine and confirms `shelbi-w-<ws>`
    is absent — killing it first only with the user's agreement and
    **re-checking afterwards** (today's teardown reports a remote kill as done
    even when it failed). Unreachable / declined / unverified-kill → pending.
    Run from `run_main` with a `[y/N]` consent prompt (pre-alt-screen).
  - `ensure_workspace_dispatchable` — refuses dispatch onto a pending workspace
    with a why/how-to-fix message. Wired as the backstop in
    `start_workspace_on_task` + `resume_workspace_on_task` (every dispatch path
    funnels through them, so the poller's supervised redispatch is covered too)
    and surfaced synchronously in `mutate::start::start` for `task start`.
- **`shelbi workspace list`** prints a `migration pending: …` summary line
  (session backend only, only when something is pending).
- **State tolerance:** a persisted `Agent` record carrying a `tmux:` address
  still loads (regression test); `Agent` has no `deny_unknown_fields`, so the
  address is preserved and the new backend simply ignores it.

## Tests

- Hermetic throughout: migration tests use a stub `MigrationProbe` + temp
  `SHELBI_HOME` under `test_lock`; nothing queries or kills the default tmux
  server. Production kill targets the exact `=<name>` (never a prefix).
- `use_private_tmux_server()` now also pins `SHELBI_SESSION_BACKEND=0` so the
  crate's real-tmux round-trip tests keep driving the tmux backend after the
  flip (re-asserted each call to survive sibling env churn).
- Covers: refuse-to-open, remote kill+verify, unverified/declined kill →
  pending, unreachable → pending + dispatch refused + others OK, in-flight
  redispatch once migrated, tmux-runtime no-op, tmux-addr load.
- `cargo test --workspace` green; `tmux ls` still shows the hub's
  `shelbi-shelbi` session after the full run (verified). No `Cargo.lock` change.

## Out of scope (deferred, per the task's Technical Details / plan)

- **`rt-cutover-delete`** removes the hidden setting and the migration
  scaffolding.
- **Agent-instruction template rewrites** (tmux → `shelbi session` commands) and
  their config-upgrade sniffer, **packaging** (drop tmux from deb/formula),
  **docs**, and the **CI tmux/Screen smoke job** are separate Phase 6 bullets,
  not part of "flip the default + migrate." No shipped `*.template` / default
  config changed here, so no config-upgrade sniffer is required for this change.

## Notes for review

- Dispatch gate default: an **absent** migration entry reads as *not*
  dispatchable (safe), but the open-time pass writes an explicit entry for every
  declared workspace, so after a normal open only genuinely-pending remotes are
  blocked.
- The seam-gate allowlist (`tests/session_op_seam_gate.rs`) gains
  `migration.rs`: the pass must query the *legacy* tmux runtime directly, since
  the `SessionBackend` seam now resolves to the session-process backend. I did
  not edit the `docs/removing-tmux/README.md` allowlist table to avoid
  parallel-subtask conflicts; it should gain the `migration.rs` row at merge.
