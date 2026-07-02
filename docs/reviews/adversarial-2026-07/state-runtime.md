# Adversarial review: runtime state probes + keymap (shelbi-state)

Reviewed:

- crates/shelbi-state/src/workspace_status.rs (1573 lines)
- crates/shelbi-state/src/agent_workspaces.rs (1277 lines)
- crates/shelbi-state/src/ssh_control.rs (468 lines)
- crates/shelbi-state/src/keymap/loader.rs (1334 lines)
- crates/shelbi-state/src/keymap/chord.rs (555 lines)
- crates/shelbi-state/src/keymap/actions.rs (470 lines)

Supporting context read (not in scope, no findings reported against them):
`crates/shelbi-ssh/src/lib.rs`, `crates/shelbi-tui/src/poller.rs`,
`crates/shelbi-tui/src/app.rs`, `crates/shelbi-state/src/lib.rs`
(`atomic_write`, `read_state`/`write_state`, `agents_dir`),
`crates/shelbi-core/src/model.rs` (`validate_workspaces`).

| #   | Finding                                                                                     | Severity | Confidence  | Category         |
|-----|---------------------------------------------------------------------------------------------|----------|-------------|------------------|
| F1  | events.log record injection: identifier fields bypass `sanitize_reason`                     | medium   | certain     | hardening        |
| F2  | Pane-title marker is spoofable by pane content; `rfind` also matches inside longer words    | medium   | certain     | assumption       |
| F3  | keys.yml empty-list "unbind" silently reverts to built-in defaults instead                  | medium   | certain     | bug              |
| F4  | Collision revert can reintroduce collisions that are never re-checked → nondeterministic binding | medium | certain  | bug              |
| F5  | Daemon PID-file check accepts *any* live process — PID reuse permanently skips CM cleanup   | medium   | certain     | assumption       |
| F6  | Stale remote `/tmp/shelbi-hub.sock` silently breaks the reverse forward after a network drop | medium  | likely      | failure-scenario |
| F7  | Agent divergence detection: false positive on shelbi upgrade, false negative on repeat edits | medium  | certain     | assumption       |
| F8  | `write_bundled_agent` uses non-atomic `fs::write`; a crash mid-write is then preserved as a "customization" | medium | certain | failure-scenario |
| F9  | `state.json` read-modify-write races between concurrent shelbi processes                    | medium   | likely      | bug              |
| F10 | Duplicate chord inside one action's list fires a spurious self-collision and discards the binding | low  | certain     | bug              |
| F11 | Fixed, predictable remote socket path in world-writable `/tmp` (squat / cross-user exposure) | low     | likely      | hardening        |
| F12 | Global↔mode chord collisions are not detected; global silently shadows mode overrides       | low      | likely      | best-practice    |
| F13 | One wrong-typed scalar in keys.yml throws away the whole file with a vague error            | low      | certain     | best-practice    |
| F14 | Workspace / agent / project names are joined into paths unvalidated (traversal-capable)     | low      | certain     | hardening        |
| F15 | Chord parser edge cases: `ctrl-` reports `MultiKeyNotSupported("")`; stale doc claims       | low      | certain     | best-practice    |
| F16 | Loader hardcodes the mode list instead of using `MODE_NAMES`; misc dead guards              | low      | certain     | simplification   |

---

## F1: events.log record injection — identifier fields bypass `sanitize_reason`

- **Where:** crates/shelbi-state/src/workspace_status.rs:363-383 (and 268-276, 284-300, 312-318, 390-395, 405-417, 487-499, 540-554, 666-680)
- **Category:** hardening
- **Severity / Confidence:** medium / certain
- **Evidence:** `sanitize_reason` (whitespace → `_`) is applied only to the "free text" parameters, never to identifiers. `append_task_event`:

  ```rust
  let reason = sanitize_reason(reason);
  ...
  append_event_line(&format!(
      "{ts} task={task_id} workflow={workflow_name} {from} -> {to} \
       reason={reason} from_category={from_category} to_category={to_category}"
  ))
  ```

  `task_id` and `workflow_name` are interpolated raw. The same pattern holds for `workspace` in `append_workspace_event` (line 275), `msg_id`/`task_id` in `append_message_event`/`append_message_ack_event`, `question_id`/`task_id` in `append_clarification_event`, `project` in `append_project_event`/`append_heartbeat_event`, `space`/`machine` in `append_contextstore_event`, and `task_id`/`workspace` in `append_dispatch_event`/`append_rebase_event`. The inconsistency is visible inside a single function — `append_rebase_event` sanitizes `branch`, `status`, `detail` but not `task_id` or `workspace` (lines 547-550). Unlike `emit_event_body` (lines 602-606), which rejects `\n`/`\r` up front, the shared sink `append_event_line` (lines 666-680) has no newline guard at all.
- **Failure scenario:** a task file whose id or `workflow:` frontmatter contains whitespace produces a line whose token positions shift, breaking every prefix/position-keyed parser (the round-trip test at lines 1192-1214 splits on single spaces). Worse, an embedded newline — task ids come from filenames, workflow names from user-editable YAML frontmatter, and task markdown can arrive from a checked-out repo — writes a *second, attacker-shaped* line into events.log, e.g. a forged `workspace=x pane_alive=false reason=...` record that the orchestrator's reaction rules act on.
- **Recommendation:** apply `sanitize_reason` (or a stricter `[A-Za-z0-9._:-]` allowlist) to every interpolated field in every `append_*` helper, and add the `\n`/`\r` rejection from `emit_event_body` to `append_event_line` as a last-line defense.
- **Effort:** S

## F2: Pane-title marker is spoofable by pane content; `rfind` matches inside longer words

- **Where:** crates/shelbi-state/src/workspace_status.rs:91-104
- **Category:** assumption
- **Severity / Confidence:** medium / certain (mechanics traced; exploitation requires hostile pane output)
- **Evidence:**

  ```rust
  pub fn parse_pane_title_marker(title: &str) -> Option<PaneMarker> {
      let idx = title.rfind("shelbi:")?;
      let tail = &title[idx + "shelbi:".len()..];
      let marker = tail.split(|c: char| c.is_whitespace()).next()?;
  ```

  Two problems. (a) `rfind("shelbi:")` is a substring match, so a title of `myshelbi:working` or a task name like `fix shelbi:review parser` inside the pane title parses as a live marker. (b) The pane title is written via OSC escapes interpreted from whatever bytes the pane's programs print. Anything the agent runs — a build script, `cat` of a hostile file, test output — can emit `\x1b]2;shelbi:review\x07` and overwrite the title. The poller (`crates/shelbi-tui/src/poller.rs:344-405`) trusts this parser to both record workspace state *and* trigger the one-shot kanban move to review on `shelbi:review`.
- **Failure scenario:** a repository under review contains a test that prints an OSC title sequence ending in `shelbi:review`. The hub poller observes the marker and promotes the in-progress task to the review column while the agent is mid-task; conversely `shelbi:working` emitted by stray output masks a stuck agent as busy indefinitely. No privilege is needed beyond producing output in the workspace pane — which is exactly what untrusted checked-out code does.
- **Recommendation:** anchor the match (require the marker at end-of-title with a known delimiter, e.g. `title.rsplit(...)` on a token boundary so `myshelbi:` doesn't match), and treat title markers as a *state hint* only — require the existing file-based review marker (which `maybe_promote_to_review` already checks independently) as the sole trigger for board moves, rather than also moving on `PaneMarker::Review`.
- **Effort:** M

## F3: keys.yml empty-list "unbind" silently reverts to built-in defaults

- **Where:** crates/shelbi-state/src/keymap/loader.rs:231-241 and 350-358
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:** the documented contract:

  ```rust
  /// `None` for "fall through to the lower layer", `Some(vec)` for an
  /// explicit override (which may be empty — meaning "unbind").
  fn to_chords(&self) -> Option<Vec<String>> {
  ```

  But in `load_keymaps`, after parsing the staged chord strings:

  ```rust
  if ok.is_empty() {
      // All overrides failed — fall back to built-ins. ...
      ok = action.default_chords().iter()
          .filter_map(|s| KeyChord::parse(s).ok())
          .collect();
  }
  ```

  An explicit `nav_up: []` stages an empty list, which is indistinguishable at this point from "every override failed to parse", so the fallback re-installs the built-in defaults. There is no other unbind mechanism — `null` means "fall through", per lines 458-461.
- **Failure scenario:** a user writes `defaults.kanban.move_card_left: []` to disable accidental card moves. The load silently restores `H`; no diagnostic is emitted, so the user has no way to discover why the binding is still live.
- **Recommendation:** distinguish "explicit empty override" from "all parses failed" — e.g. keep a `bool explicitly_unbound` alongside the staged list, or only fall back to defaults when at least one chord string existed and failed to parse.
- **Effort:** S

## F4: Collision revert can reintroduce collisions that are never re-checked → nondeterministic bindings

- **Where:** crates/shelbi-state/src/keymap/loader.rs:363-399 and 483-492
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:** per mode, `by_chord` is built once from `parsed`, then colliding actions are reverted to defaults *by mutating `parsed`*:

  ```rust
  for a in &actions {
      let defaults: Vec<KeyChord> = a.default_chords()...
      parsed.insert(*a, defaults);
  }
  ```

  The mode is not re-scanned after the revert, so a reverted default that now collides with a *third* action's surviving override is never detected. `build_keymaps` then iterates `parsed` (a `HashMap`, randomized order per process) and `insert_into` (line 489) silently overwrites `bindings.insert(*c, action)` — last write wins.
- **Failure scenario:** keys.yml contains `sidebar: { nav_up: x, nav_down: x, refresh: k }`. The `x` collision reverts `nav_up` to its defaults `["k", "up"]`. Now `k` is bound to both `nav_up` (reverted default) and `refresh` (user override). No diagnostic fires, and which action `k` dispatches to differs from run to run with the `HashMap`'s random seed.
- **Recommendation:** loop the collision pass per mode until a fixed point (reverts converge — defaults are finite), or detect collisions after all reverts in a second pass; at minimum make `insert_into` refuse to overwrite an existing binding for a different action.
- **Effort:** M

## F5: Daemon PID-file check accepts any live process — PID reuse permanently skips CM cleanup

- **Where:** crates/shelbi-state/src/ssh_control.rs:120-130 and 155-159 (doc contract at 10-13)
- **Category:** assumption
- **Severity / Confidence:** medium / certain
- **Evidence:** the module doc promises: "The cleanup pass refuses to touch the directory when the PID file points at a still-running *shelbi* process other than us." The implementation checks only that *some* process exists:

  ```rust
  pub fn is_process_alive(pid: libc::pid_t) -> bool {
      ...
      let rc = unsafe { libc::kill(pid, 0) };
      if rc == 0 { return true; }
      io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
  }
  ```

  and in `cleanup_stale_control_masters`:

  ```rust
  if prev != self_pid && is_process_alive(prev) {
      return Ok(CmCleanupOutcome::SkippedAnotherDaemon { pid: prev });
  }
  ```

  There is no check that the PID belongs to shelbi (no procfs/`sysctl` comm comparison, no start-time token in the pid file). `EPERM` → alive makes it worse: a recycled PID now owned by *another user's* process also blocks cleanup.
- **Failure scenario:** the daemon dies via SIGKILL/power loss, leaving `shelbi.pid` behind. After reboot (or normal PID churn) the recorded PID is reused by an unrelated long-lived process (a login shell, a browser helper). Every subsequent daemon startup returns `SkippedAnotherDaemon` and the stale ControlMaster sockets under `~/.shelbi/ssh/` are never pruned — the cleanup feature is disabled until someone deletes the pid file by hand.
- **Recommendation:** store `pid + process start time` (or a random token) in the pid file and verify both, or verify the process name/argv contains `shelbi` before treating it as an owner; treat `EPERM` as "not ours" for this purpose.
- **Effort:** M

## F6: Stale remote `/tmp/shelbi-hub.sock` silently breaks the reverse forward after a network drop

- **Where:** crates/shelbi-state/src/ssh_control.rs:64-73 and 237-245 (consumed by `crates/shelbi-ssh/src/lib.rs:45-111`)
- **Category:** failure-scenario
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  pub fn remote_hub_socket_path() -> PathBuf {
      if let Some(p) = std::env::var_os("SHELBI_REMOTE_HUB_SOCK") { ... }
      PathBuf::from("/tmp/shelbi-hub.sock")
  }
  ```

  `reverse_forward_spec` hands this to `ssh -R`. When a master dies without a clean shutdown (network loss, laptop sleep, remote reboot of sshd only), sshd's cleanup of the bound socket file is not guaranteed — and OpenSSH refuses to bind a `-R` unix-socket forward over an existing file unless the *remote* `sshd_config` sets `StreamLocalBindUnlink yes` (grep confirms no `StreamLocalBind*` option anywhere in the repo). The client opts explicitly are `ExitOnForwardFailure=no` and `LogLevel=ERROR` (`shelbi-ssh/src/lib.rs:62-65`), so the failed rebind neither fails the connection nor prints a visible warning.
- **Failure scenario:** hub↔remote link drops; the leaked `/tmp/shelbi-hub.sock` file survives on the remote. The next poll re-opens a fresh ControlMaster; the `-R` bind fails silently; every remote worker's `nc -U $SHELBI_HUB_SOCK` emit (pane_alive events, acks, clarifications) now hits a dead socket file for the lifetime of the new master. Events are lost with no diagnostic anywhere on the hub.
- **Recommendation:** before (or when) opening a master, run a cheap remote `rm -f` of the socket path (e.g. as part of the first command dispatched over a fresh master), or probe that the forward is actually live after master open and surface a diagnostic; document the `StreamLocalBindUnlink` requirement for remote hosts.
- **Effort:** M

## F7: Agent divergence detection — false positive on shelbi upgrade, false negative on repeat edits

- **Where:** crates/shelbi-state/src/agent_workspaces.rs:521-539
- **Category:** assumption
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  if current == agent.instructions {
      if state.notified_diverged_agents.remove(agent.name) { ... }
      ...
  } else {
      let first_notice = state.notified_diverged_agents.insert(agent.name.to_string());
  ```

  Divergence is a byte-compare against the *currently compiled* default, and the acknowledgment set stores only the agent *name*. Two consequences. (a) **False positive:** when a new shelbi release ships an updated `default_developer.md.template`, every untouched install now has `current != agent.instructions` — the user is told they "customized this agent" (they didn't), and the improved default prompt is never rolled out; the stale prompt is preserved forever. (b) **False negative:** the docstring (lines 424-426) promises `first_notice` is true "the first self-heal pass to observe the *current* divergence", but once the name is in the set, editing the file to *different* divergent content never re-fires the notice — only passing through byte-equality (line 522) clears it.
- **Failure scenario:** (a) user upgrades shelbi; `shelbi reload` reports orchestrator+developer as `Preserved` with a customization notice, and the shipped prompt fixes (e.g. the Phase-5 socket-emit paragraph the tests at lines 833-849 pin) never reach existing installs.
- **Recommendation:** track a content hash of the last-noticed divergent body (re-fire when it changes), and separately keep a hash of the *previous* shipped default so "file == old default, binary has new default" can be distinguished from user customization and safely upgraded.
- **Effort:** M

## F8: `write_bundled_agent` uses non-atomic `fs::write`; a crash mid-write is then preserved as a "customization"

- **Where:** crates/shelbi-state/src/agent_workspaces.rs:553-566 (also 509, 582)
- **Category:** failure-scenario
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  fs::write(
      agent_instructions_path(project, agent.name)?,
      agent.instructions,
  )
  .map_err(Error::Io)?;
  ```

  The crate has `atomic_write` (tmp + `sync_all` + rename, `lib.rs:1498-1514`) and uses it for `status.yaml` and `keys.yml`, but the agent scaffolding paths (`write_bundled_agent`, the missing-instructions self-heal at line 509, `ensure_agent_settings_present` at line 582) all use plain `fs::write`, which truncates then writes.
- **Failure scenario:** power loss / SIGKILL mid-`shelbi init` leaves a truncated `instructions.md`. On the next `shelbi reload`, `self_heal_default_agents` reads it, finds `current != agent.instructions`, classifies the torn file as a user customization (`Preserved`, F7), and *keeps it forever* — the agent then dispatches with a half prompt. The self-heal machinery designed to fix exactly this instead entrenches it.
- **Recommendation:** route these writes through `atomic_write`.
- **Effort:** S

## F9: `state.json` read-modify-write races between concurrent shelbi processes

- **Where:** crates/shelbi-state/src/agent_workspaces.rs:476-546 and 281-317 (via `read_state`/`write_state`, lib.rs:933-950)
- **Category:** bug (race condition)
- **Severity / Confidence:** medium / likely
- **Evidence:** `self_heal_default_agents` does `read_state` → mutate `notified_diverged_agents` → `write_state`; `maybe_emit_claude_md_migration_hint` and `reset_claude_md_migration_hint` do the same for their latch. `write_state` is an atomic *replace* but there is no lock spanning the read-modify-write, and `state.json` also carries unrelated hot fields (`zen_mode`, `workspace_filter`, crash timestamps) written by `set_zen_mode` / `set_workspace_filter` from other processes (CLI, TUI, orchestrator).
- **Failure scenario:** `shelbi reload` runs `self_heal_default_agents` while the user hits the Zen hotkey. Interleaving: self-heal reads state (zen=off) → TUI writes zen=on → self-heal writes back its full snapshot with zen=off. The user's Zen toggle is silently lost; equivalently a diverged-agent acknowledgment can be lost, re-firing the notice. Whole-file last-writer-wins makes every field a casualty of any concurrent RMW.
- **Recommendation:** take an advisory `flock` on `state.json` (or a sibling lock file) around read-modify-write cycles, or split independent concerns into separate files so cross-field clobbering can't happen.
- **Effort:** M

## F10: Duplicate chord inside one action's list fires a spurious self-collision and discards the binding

- **Where:** crates/shelbi-state/src/keymap/loader.rs:366-399
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  for c in chords {
      by_chord.entry(*c).or_default().push(*action);
  }
  ...
  if actions.len() < 2 { continue; }
  ```

  The same action is pushed once per occurrence, and the length check counts occurrences, not distinct actions. `nav_up: [k, up, k]` yields `by_chord[k] = [NavUp, NavUp]`, len 2 → an `Error: chord `k` is bound to multiple actions (nav_up, nav_up)` diagnostic, and the user's whole override for that action is thrown away in favor of defaults.
- **Failure scenario:** as above — a harmless copy-paste duplicate in keys.yml produces a confusing "bound to multiple actions (nav_up, nav_up)" error and reverts the binding.
- **Recommendation:** dedupe each action's chord list before staging, or count *distinct* actions per chord (`actions.iter().collect::<HashSet<_>>().len() < 2`).
- **Effort:** S

## F11: Fixed, predictable remote socket path in world-writable `/tmp`

- **Where:** crates/shelbi-state/src/ssh_control.rs:64-73
- **Category:** hardening
- **Severity / Confidence:** low / likely
- **Evidence:** the default remote landing path is the constant `/tmp/shelbi-hub.sock`, shared `/tmp` on the remote host. Two exposures on multi-user remotes: (a) **squat/DoS** — any local user can pre-create a file or their own listening socket at that path; the hub's `-R` bind then fails (silently, per F6) or, if they bind a listener, remote workers connect to the *attacker's* socket and hand it their event lines (task ids, clarification text). (b) **collision** — two hub users (or two hubs) targeting the same remote both claim the same path; whoever binds first captures the other's workers. sshd's default `StreamLocalBindMask 0177` protects the socket's own perms in the legitimate case, but does nothing against pre-creation.
- **Failure scenario:** shared CI/dev box: user B runs `nc -lU /tmp/shelbi-hub.sock`. User A's hub connects; forward bind fails silently; A's remote workers write their pane_alive/ack/clarification traffic into B's listener.
- **Recommendation:** derive a per-user (ideally per-hub-instance) default, e.g. `/tmp/shelbi-hub-$(id -u).sock` or a path under the remote user's home / `$XDG_RUNTIME_DIR`, and keep `SHELBI_REMOTE_HUB_SOCK` as the override.
- **Effort:** S

## F12: Global↔mode chord collisions are not detected; global silently shadows mode overrides

- **Where:** crates/shelbi-state/src/keymap/loader.rs:363-399
- **Category:** best-practice
- **Severity / Confidence:** low / likely
- **Evidence:** collision detection is strictly intra-mode (`if action.mode() != mode { continue; }`). The TUI dispatches "through `keymaps.global` then `keymaps.sidebar`" (`crates/shelbi-tui/src/app.rs:156-157`), so a user override like `defaults.sidebar.refresh: ctrl-p` is legal per the loader but can never fire — `global.open_palette` consumes `ctrl-p` first — with no diagnostic pointing at the shadowing. The reserved-chord warning exists only for `ctrl-c`/quit (lines 405-418).
- **Failure scenario:** user binds a mode action to `ctrl-p`/`alt-z`; the binding is silently dead and the loader reports a clean load.
- **Recommendation:** after the merge, warn when a mode-level chord equals any bound global chord (same shape as the `ReservedChordRebind` warning).
- **Effort:** S

## F13: One wrong-typed scalar in keys.yml throws away the whole file

- **Where:** crates/shelbi-state/src/keymap/loader.rs:210-229 and 496-526
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:** `ChordSpec` is an untagged enum over `None`/`One(String)`/`Many(Vec<String>)`, deserialized as part of the whole-file `KeysFile`. A value that is neither null, string, nor string list — e.g. `nav_up: 5` or `zen_toggle: true` (a user meaning `f5` or getting YAML-boolean'd) — fails all three variants, which fails the *entire* `serde_yaml::from_str::<KeysFile>` in `read_keys_file`, dropping every override in the file with a single vague untagged-enum error ("did not match any variant…"). Contrast with unknown modes/actions, which are handled entry-by-entry with precise diagnostics (lines 434-453).
- **Failure scenario:** a one-character typo in one binding reverts the user's whole keymap to built-ins, and the diagnostic doesn't say which line is at fault.
- **Recommendation:** deserialize chord values as `serde_yaml::Value` and convert per entry, emitting a located `ParseError` for just that action (matching the unknown-action granularity).
- **Effort:** M

## F14: Workspace / agent / project names are joined into paths unvalidated

- **Where:** crates/shelbi-state/src/agent_workspaces.rs:114-116; crates/shelbi-state/src/workspace_status.rs:131-145
- **Category:** hardening
- **Severity / Confidence:** low / certain (mechanics; exploitability limited to config-controlled names)
- **Evidence:**

  ```rust
  pub fn agent_workspace_dir(project: &str, agent: &str) -> Result<PathBuf> {
      Ok(agents_dir(project)?.join(agent))
  }
  ```

  and

  ```rust
  pub fn workspace_status_path(workspace: &str) -> Result<PathBuf> {
      Ok(workspaces_dir()?.join(workspace).join("status.yaml"))
  }
  ```

  `Path::join` with a component containing `..` (or an absolute path, which *replaces* the whole prefix) escapes the intended directory. `Project::validate_workspaces` (`shelbi-core/src/model.rs:426-436`) validates machine/runner references only — never the name's shape. Workspace names come from project YAML, agent names from workflow YAML `agent:` fields and CLI args; `mark_expected_teardown`/`save_workspace_status` will then `atomic_write` at the escaped location, and `compose_agent_prompt` will read an arbitrary `instructions.md`.
- **Failure scenario:** a project YAML (potentially synced from a repo in in-repo config mode) declares a workspace named `../../../home/user/.ssh` — the status writer creates directories and files outside `~/.shelbi/workspaces/`. Mostly a self-footgun today, but it converts "bad config value" into "writes anywhere the user can write".
- **Recommendation:** validate names at the boundary (single path component, no `/`, not `.`/`..`, non-empty) — one shared `validate_name()` used by `workspace_status_path`, `agent_workspace_dir`, and `project_dir` callers.
- **Effort:** S

## F15: Chord parser edge cases and stale doc claims

- **Where:** crates/shelbi-state/src/keymap/chord.rs:141-150, 190-206, 289-296
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:** (a) `KeyChord::parse("ctrl-")` splits to `["ctrl", ""]`; the empty token reaches `parse_keyname`, where `"".chars().all(|c| c.is_ascii_alphabetic())` is vacuously true, producing `MultiKeyNotSupported("")` — the user sees ``multi-key sequences (``) are not supported`` for a trailing dash. (b) `from_event`'s comment says "the lookup map keeps both forms" — false; the loader stores only the parsed lowercase+SHIFT form and `ModeKeymap::dispatch` (loader.rs:104-123) papers over it with an uppercase fallback. (c) `canonical()` documents a lossless round-trip, but for event-derived chords like `Char('J')+SHIFT` it renders `shift-J`, which re-parses to the *different* chord `Char('j')+SHIFT` — fine today only because `canonical()` is applied to parsed chords; a future caller formatting event chords inherits the trap. Also line 96's `c.is_control() || c == '\u{0007}'` — BEL *is* a control char; the second clause is dead (workspace_status.rs).
- **Failure scenario:** confusing diagnostics and a latent round-trip trap rather than a live bug.
- **Recommendation:** reject empty segments in `split_chord` output with a dedicated error; fix the `from_event` comment; make `canonical()` normalize uppercase `Char` codes (lowercase + SHIFT) so the documented invariant holds for all chords.
- **Effort:** S

## F16: Loader hardcodes the mode list; misc duplication

- **Where:** crates/shelbi-state/src/keymap/loader.rs:364; crates/shelbi-state/src/keymap/actions.rs:111-113
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let modes = ["global", "sidebar", "kanban", "popover", "review", "activity", "palette"];
  ```

  duplicates `MODE_NAMES` in actions.rs. A future eighth mode added to `actions.rs` but not this array silently skips collision detection for that mode (nothing ties them together; the `all_actions_have_a_known_mode` test checks actions against `MODE_NAMES`, not against the loader's copy). Same theme: the collision pass could derive modes from `Action::all()` directly.
- **Failure scenario:** new mode ships; its intra-mode collisions go undiagnosed and fall into the F4 nondeterministic-overwrite behavior.
- **Recommendation:** use `MODE_NAMES` (or derive the distinct set from `Action::all()`) in the collision pass.
- **Effort:** S

---

## Seed-question notes (non-findings)

- **Prefix conflicts (`ctrl+a` vs `ctrl+a b`):** cannot occur — the chord grammar is single-key only; multi-key sequences are rejected at parse (`ChordParseError::MultiKeyNotSupported`, chord.rs:44-49). Conflict detection therefore only needs (and has) exact-equality comparison; see F4/F10/F12 for where the equality-based pass still misfires.
- **Malformed chords / unknown actions in keys.yml:** no panics found; both produce diagnostics and fall back to defaults (loader.rs:334-359, 428-465). The gaps are the granularity issue (F13) and the unbind contract (F3).
- **Polling without backoff:** the poll loops live outside this slice (shelbi-tui/poller.rs). Within scope, the only sleep is `emit_event_body`'s single 500 ms retry, which correctly skips the sleep on `NotFound` (workspace_status.rs:608-622) — verified by the timing test at line 961.
- **`emit_event_body` at-most-once caveat (speculative, low):** a successful socket `write_all` proves the daemon *received* the line, not that it appended it; a daemon crash between read and append loses the event and the file fallback never fires. Acceptable for an event stream, but worth knowing when debugging a missing line.
