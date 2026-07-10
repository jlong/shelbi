# Adversarial review: session + UX commands (shelbi-cli)

Reviewed:

- crates/shelbi-cli/src/commands/palette.rs (1619 lines)
- crates/shelbi-cli/src/commands/open.rs (171 lines) and commands/open/pane.rs (1115 lines)
- crates/shelbi-cli/src/commands/init.rs (839 lines) and commands/init/heuristic.rs (160 lines)
- crates/shelbi-cli/src/commands/agent.rs (663 lines)
- crates/shelbi-cli/src/commands/config.rs (616 lines)

Supporting code read for context (not in scope, no findings reported against it):
`crates/shelbi-cli/src/project_root.rs`, `crates/shelbi-agent/src/lib.rs`
(`shell_escape`, `launch_command`, `with_permission_mode`),
`crates/shelbi-state/src/workspace_status.rs` (expected-teardown markers),
`crates/shelbi-state/src/agent_workspaces.rs` (materialize / self-heal),
`crates/shelbi-state/src/lib.rs` (path helpers), `crates/shelbi-cli/src/commands/mod.rs`
(`require_project`).

tmux behaviors cited below (name prefix-matching, `=` exact-match prefix,
`new-window -S`) were verified live against tmux 3.5a on a private test server
(`tmux -L …`), not just from the man page.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | `init --pick-up` trusts the committed `name:` — path traversal + YAML injection into `~/.shelbi` | high | certain | hardening |
| F2 | `shelbi open` exists-check is not atomic with creation — concurrent opens create duplicate panes | medium | certain | failure-scenario |
| F3 | tmux window targets use prefix matching — `open web` focuses `web-api` and never creates `web` | medium | certain | bug |
| F4 | `scaffold_project` early-return: partial scaffold is not re-runnable; "Re-initialize?" confirm is a silent no-op | medium | certain | bug |
| F5 | `agent edit` breaks for any `$EDITOR` containing arguments; error is never annotated despite comment claiming it is | medium | certain | bug |
| F6 | `customized_marker` byte-compare conflates "user customized" with "stale prior default" — upgraded defaults mislabel untouched users | low | certain | assumption |
| F7 | palette `run()` error paths leak raw mode / alt-screen and silently swallow `picker_loop` errors | low | certain | bug |
| F8 | init scaffolds project YAML by string interpolation — unescaped name/work_dir produces broken or wrong YAML | low | certain | bug |
| F9 | `exists()` guard in `scaffold_project` claims race safety it doesn't provide | low | certain | hardening |
| F10 | `shelbi init --root <path>` at a TTY is treated as fully non-interactive and hard-errors demanding `--mode` | low | certain | bug |
| F11 | pane wrapper signal windows: pre-install gap loses the lifecycle event; post-wait forwarding and `kill_task_tail` can signal a recycled PID | low | likely | hardening |
| F12 | `run_local_tmux` / `run_tmux` collapse all failures to `false` and discard stderr — undiagnosable errors | low | certain | best-practice |
| F13 | `run_pick_up` duplicates the init scaffolding (already drifted: `repo:` field differs) | low | certain | simplification |
| F14 | palette dead code: identical match arms in `entry_from_row`, duplicated `run_tmux` helper, code after the test module | low | certain | simplification |
| F15 | `agent show`/`edit` skip the name validation `new` enforces; `read_skills` silently skips symlinked skills | low | certain | best-practice |
| F16 | init heuristic runs unbounded `git log --format=%ae` — slow first-run prompt on huge repos | low | certain | best-practice |
| F17 | `config dump-keybindings -o` silently overwrites the file it tells you to maintain by hand | low | certain | best-practice |

---

## F1: `init --pick-up` trusts the committed `name:` — path traversal + YAML injection into `~/.shelbi`

- **Where:** crates/shelbi-cli/src/commands/init.rs:421 (read), :432–443 (alias), :458–475 (write)
- **Category:** hardening
- **Severity / Confidence:** high / certain
- **Evidence:** `run_pick_up` reads the canonical name straight out of a file committed to the repo being picked up:

  ```rust
  let canonical_name = read_in_repo_name(&config_path)?.ok_or_else(|| { … })?;
  ```

  `read_in_repo_name` (init.rs:363–382) only trims whitespace and rejects empty — no charset validation. The name then flows into every path helper unmodified:

  ```rust
  let yaml_path = projects_dir.join(format!("{}.yaml", local_alias));
  …
  std::fs::write(&yaml_path, yaml)?;
  ```

  plus `write_workspace_settings_template(&local_alias)` → `shelbi_state::project_dir(project)` which is a bare `projects_dir()?.join(project)` (shelbi-state/src/lib.rs:175–177), `materialize_default_agents(&local_alias)`, and `statuses_path(&local_alias)`. `PathBuf::join` happily traverses `..` and absolute components.
- **Failure scenario:** clone a repo whose committed `.shelbi/project.yaml` contains `name: ../../.config/foo` (a plain YAML scalar — no quoting tricks needed) and run `shelbi init --pick-up`. shelbi writes `<home>/.shelbi/projects/../../.config/foo.yaml` — i.e. an attacker-positioned file outside `~/.shelbi/projects/` whose leading content (`name:`, `machines:`, `work_dir:`) the attacker partially controls — and then creates agent/workflow directory trees at the traversed location. Separately, a name containing YAML-significant text (the double-quoted form `name: "x\nmachines: …"` decodes an embedded newline) is interpolated verbatim into the generated local YAML (init.rs:459–474), injecting keys into the user's own project registry. `--pick-up` is *exactly* the flow you run on somebody else's repo, so this is hostile-input-facing by design.
- **Recommendation:** validate the canonical name (and the `--project` override) with the same rule `validate_agent_name` uses (init.rs should get a `validate_project_name`: non-empty, no leading `.`, chars limited to `[A-Za-z0-9_-]`). Reject, don't sanitize. Fresh-init names derived from directory basenames should go through the same check (see F8).
- **Effort:** S

## F2: `shelbi open` exists-check is not atomic with creation — concurrent opens create duplicate panes

- **Where:** crates/shelbi-cli/src/commands/open.rs:93 (check), :101–110 (create)
- **Category:** failure-scenario
- **Severity / Confidence:** medium / certain
- **Evidence:** the module doc says "the 'exists?' check lives here so callers don't have to branch on it" (open.rs:3–4), and the check is a classic TOCTOU:

  ```rust
  if run_local_tmux(["select-window", "-t", &target]) {
      return Ok(());
  }
  match host {
      Host::Local => {
          …
          if !run_local_tmux(["new-window", "-t", &format!("{project_session}:"), "-n", &workspace.name, "sh", "-c", &pane_cmd]) {
  ```

  tmux allows any number of windows with the same name, so `new-window` never fails on a duplicate.
- **Failure scenario:** the sidebar click path and the dispatch path (or two rapid clicks — each click spawns a fresh `shelbi open` process) race: both run `select-window` before either window exists, both fail, both run `new-window`. Result: two windows named `alpha`, two lifecycle wrappers, two agent processes sharing one worktree — concurrent `claude` sessions mutating the same checkout, and two `pane_alive=false` emitters for one workspace. Nothing downstream detects or reaps the duplicate.
- **Recommendation:** collapse check+create into one tmux call: `new-window -S -t <session>: -n <name> …` selects the existing window instead of creating a second one (verified on tmux 3.5a; `-S` exists since tmux 3.2 — gate on version or document the minimum). Combine with the `=` exact-match fix from F3.
- **Effort:** S

## F3: tmux window targets use prefix matching — `open web` focuses `web-api` and never creates `web`

- **Where:** crates/shelbi-cli/src/commands/open.rs:88–93; also crates/shelbi-cli/src/commands/palette.rs:534, :538
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  let target = format!("{project_session}:{}", workspace.name);
  if run_local_tmux(["select-window", "-t", &target]) {
      return Ok(());
  }
  ```

  tmux resolves a window-name target as exact match, then *prefix*, then fnmatch. Verified live (tmux 3.5a): with only a window named `web-api` in the session, `select-window -t probe:web` exits 0 and lands on `web-api`.
- **Failure scenario:** project declares workspaces `web` and `web-api`. `web-api`'s pane is up; `web`'s is not. `shelbi open web` "succeeds" by focusing `web-api` and returns without ever creating `web`'s pane — the user (or the dispatch path) is now targeting the wrong workspace's agent. The same mis-targeting applies to the palette's `select-window -t shelbi-{project}:{id}` dispatches (palette.rs:534, :538) and, worse, to anything that later `send-keys`es at that target. Workspace names that are fnmatch metacharacters (`*`, `?`) widen the blast radius.
- **Recommendation:** prefix the window-name part with `=` to force exact matching (`format!("{project_session}:={}", workspace.name)`) — verified: `select-window -t 'probe:=web'` fails when only `web-api` exists and hits on the exact name. Apply to every `-t` built from a workspace/agent name in open.rs and palette.rs.
- **Effort:** S

## F4: `scaffold_project` early-return: partial scaffold is not re-runnable; "Re-initialize?" confirm is a silent no-op

- **Where:** crates/shelbi-cli/src/commands/init.rs:284–287; crates/shelbi-cli/src/project_root.rs:215–226 (the confirm it defeats)
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  if yaml_path.exists() {
      println!("(project YAML already exists at {})", yaml_path.display());
      return Ok(());
  }
  ```

  Everything else the scaffold owns — the in-repo config (`write_in_repo_config`, :308–310), the workspace-settings template (:312), `materialize_default_agents` (:314–318), and the statuses catalogue (:325–334) — sits *after* this return.
- **Failure scenario:** (a) first `shelbi init` writes the YAML then dies mid-scaffold (crash, Ctrl-C at :305–314, disk error from `materialize_default_agents`). Re-running prints "(project YAML already exists …)" and exits 0 — the project is permanently half-scaffolded (no agents, no statuses, no template) while init reports success. This is the "partial scaffold on failure — re-runnable?" question, and the answer is no. (b) The interactive collision path in `prompt_loop` explicitly asks "a shelbi project named `X` already exists … Re-initialize?" — the user confirms, and `scaffold_project` then refuses to touch anything because the YAML exists. The confirm is a lie.
- **Recommendation:** make each scaffold step individually idempotent (they already mostly are — `write_in_repo_config`, the template writer, and the statuses writer all self-guard) and drop the whole-function early return so a re-run completes missing pieces. Thread a `reinit: bool` from the prompt's confirm if overwriting the YAML is intended, or change the prompt copy to match reality.
- **Effort:** M

## F5: `agent edit` breaks for any `$EDITOR` containing arguments; error is never annotated despite comment claiming it is

- **Where:** crates/shelbi-cli/src/commands/agent.rs:210–214, :233–236
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  let editor = resolve_editor()?;
  let status = std::process::Command::new(&editor)
      .arg(&instructions_path)
      .status()?;
  ```

  `Command::new` treats the entire string as the program name. `EDITOR="code --wait"`, `"subl -w"`, `"emacsclient -t"`, `"vim -u NONE"` — all conventional values honored by git, crontab, etc. — fail with "No such file or directory" because no binary named `code --wait` exists. Also, `resolve_editor`'s comment says "`Command::status` will surface a clear 'No such file or directory' if vim is missing, **and we annotate it below**" (:234–236) — but `.status()?` propagates the raw `io::Error` with no annotation; the user sees `No such file or directory (os error 2)` with no hint that shelbi was trying to launch an editor, let alone which one.
- **Failure scenario:** user with `EDITOR="code --wait"` runs `shelbi agent edit developer` → `Error: No such file or directory (os error 2)`. Feature unusable, cause opaque.
- **Recommendation:** run the editor through the shell — `sh -c "$EDITOR \"$1\"" sh <path>` (or split on whitespace as a lesser fallback) — and add `.with_context(|| format!("launching editor `{editor}`"))` so the comment stops being aspirational.
- **Effort:** S

## F6: `customized_marker` byte-compare conflates "user customized" with "stale prior default"

- **Where:** crates/shelbi-cli/src/commands/agent.rs:110–126
- **Category:** assumption
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let Some(default_body) = shelbi_state::default_agent_body(agent) else { return Ok("-"); };
  …
  Ok(if current == default_body { "no" } else { "yes" })
  ```

  The comparison is against the default bundled in the *currently running* binary. The self-heal pass (`self_heal_default_agents`, shelbi-state/src/agent_workspaces.rs) applies the same test and preserves anything divergent. The implicit assumption: "differs from the current bundle" ⇒ "the user edited it". That doesn't hold across upgrades.
- **Failure scenario:** user inits a project on shelbi vN, never touches `agents/developer/instructions.md`. Shelbi vN+1 ships an improved default body. Now (a) `shelbi agent list` reports `CUSTOMIZED: yes` for a file the user never edited, (b) self-heal "preserves the customization" so the user is silently pinned to the vN prompt forever, and (c) the one-time "preserved your custom instructions.md" notice fires, telling the user they customized something they didn't. This is the template-drift conflict the agent-instructions self-heal note gestures at — there is no mechanism to distinguish the two cases.
- **Recommendation:** track provenance: compare against a set of *all shipped* default bodies (or record the SHA of the body that was materialized in project state). "Matches any prior shipped default" → safe to upgrade + report `no (outdated default)`; "matches nothing shipped" → genuinely customized.
- **Effort:** M

## F7: palette `run()` error paths leak raw mode / alt-screen and silently swallow `picker_loop` errors

- **Where:** crates/shelbi-cli/src/commands/palette.rs:43–44, :68, :72, :82, :93 (leaks); :106 (swallow)
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:** `setup_terminal()` (line 43) enables raw mode and enters the alt-screen; `restore_terminal` only runs at line 98. Every `?` between them — `State::new(&project, &keymaps)?` (:44), `run_project_picker(…)?` (:68), `run_quit_project_confirm(…)?` (:72), `handle_zen_intro_then_toggle(…)?` (:82), `run_quit_shelbi_confirm(…)?` (:93) — propagates out of `run` without disabling raw mode or leaving the alt-screen. Separately, when `picker_loop` itself errors the loop breaks with `chosen = Err(…)`, `restore_terminal` *does* run, but the error is then discarded:

  ```rust
  } else if let Ok(Some(entry)) = chosen {
  ```

  — an `Err` falls through every arm and `run` returns `Ok(())`, so a real draw/input failure exits 0 with no diagnostic. (Related smell: `app.refresh().ok()` at :131 silently renders an empty/stale palette when state reads fail.)
- **Failure scenario:** any `term.draw` or `event::read` error inside a sub-screen (:68–93) leaves the invoking terminal in raw mode. In the intended `tmux display-popup` host the pty is destroyed with the popup so damage is contained — but the binary is runnable directly (`shelbi __palette X`), where this garbles the user's shell.
- **Recommendation:** wrap the body so restore always runs (RAII guard struct calling `disable_raw_mode`/`LeaveAlternateScreen` on `Drop`, the standard ratatui pattern), and propagate the `chosen` error after restoring instead of pattern-matching it away.
- **Effort:** S

## F8: init scaffolds project YAML by string interpolation — unescaped name/work_dir produces broken or wrong YAML

- **Where:** crates/shelbi-cli/src/commands/init.rs:289–305 (fresh), :459–474 (pick-up)
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let yaml = format!(
      "name: {name}\n\
       repo: \n\
       …
       \x20\x20\x20\x20work_dir: {root}\n\
       …",
      name = resolved.name,
      root = resolved.path.display(),
  );
  ```

  `resolved.name` is the directory basename verbatim (`project_name_from_root`, project_root.rs:77–82 — no validation) and `root` is a raw path. Neither is YAML-escaped.
- **Failure scenario:** a checkout at `~/code/proj #1` yields `work_dir: /Users/me/code/proj #1` — YAML treats ` #` as a comment start, so the registered work_dir is silently `/Users/me/code/proj` (wrong directory, no error). A directory basename containing `: ` (legal on macOS/Linux) produces `name: foo: bar`, which fails to parse and bricks the just-created registry entry. `repo: \n` (trailing space, empty value) in the fresh-init template is also sloppy — `repo` parses as null and the trailing space survives in the file.
- **Recommendation:** build the document with `serde_yaml::to_string` on a small struct (or at minimum quote-escape interpolated scalars), and validate the derived name per F1's shared validator so `init` fails loudly on names that can't round-trip.
- **Effort:** S

## F9: `exists()` guard in `scaffold_project` claims race safety it doesn't provide

- **Where:** crates/shelbi-cli/src/commands/init.rs:273–276 (comment), :284–287 + :305 (code)
- **Category:** hardening
- **Severity / Confidence:** low / certain
- **Evidence:** the doc comment states: "We still guard the write with `exists()` so a race against a concurrent `shelbi init` doesn't blow away another invocation's freshly-written YAML." The guard is:

  ```rust
  if yaml_path.exists() { … return Ok(()); }
  …
  std::fs::write(&yaml_path, yaml)?;
  ```

  `exists()` → `write()` is itself a TOCTOU window; two concurrent inits can both observe `!exists` and the second `fs::write` clobbers the first — precisely what the comment says the guard prevents.
- **Failure scenario:** two `shelbi init` runs for the same name (e.g. a wizard retry racing a scripted init) interleave between :284 and :305; the loser's YAML silently overwrites the winner's. Low practical likelihood, but the comment sells a guarantee the code doesn't have.
- **Recommendation:** `OpenOptions::new().write(true).create_new(true)` and treat `AlreadyExists` as the "already scaffolded" branch — that's the atomic version of the same intent, one line more. Same pattern applies to the pick-up write (:475).
- **Effort:** S

## F10: `shelbi init --root <path>` at a TTY is treated as fully non-interactive and hard-errors demanding `--mode`

- **Where:** crates/shelbi-cli/src/commands/init.rs:160, :203–218
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let interactive = std::io::stdin().is_terminal() && args.root.is_none();
  …
  if !interactive {
      bail!("shelbi init: pass `--mode in-repo` or `--mode global` — non-interactive callers must choose explicitly …");
  }
  ```

  `--root`'s own help text says it merely "Skips the interactive 'Project root?' prompt" (:65–67), and `--mode`'s says it may be omitted "in interactive contexts" (:72–74). Passing `--root` doesn't make a session non-interactive, but the conjunction at :160 treats it that way.
- **Failure scenario:** a user at a real terminal runs `shelbi init --root ~/code/app` (a perfectly natural "init that repo over there") and gets the "non-interactive callers must choose explicitly" error instead of the mode picker the docs promise. The error message even mis-describes them as non-interactive.
- **Recommendation:** decouple the two: `interactive = stdin().is_terminal()`; keep `args.root` only as "skip the root prompt". The mode picker then still runs for TTY users who passed `--root`.
- **Effort:** S

## F11: pane wrapper signal windows: pre-install gap loses the lifecycle event; post-wait forwarding and `kill_task_tail` can signal a recycled PID

- **Where:** crates/shelbi-cli/src/commands/open/pane.rs:151–168 (install after spawn), :170–176 (close after wait), :333–351 (`kill_task_tail`)
- **Category:** hardening
- **Severity / Confidence:** low / likely
- **Evidence:** (a) the child is spawned (:151–161) *before* `install_signal_listener` runs (:164–168). A SIGTERM/SIGHUP delivered in that window hits the wrapper's default disposition: the wrapper dies immediately, no `pane_alive=false` event is written, and no signal is forwarded — the exact failure mode this file exists to prevent. (b) After `child.wait()` returns (:170) the listener thread is still live until `signal_handle.close()` (:176); a signal in that window forwards via `libc::kill(child_pid, sig)` to a PID the kernel has already reaped and may have recycled. (c) `kill_task_tail` SIGTERMs whatever PID is in `<worktree>/.shelbi/messages/<task>.tail.d/pid` with no liveness/identity check:

  ```rust
  if let Ok(pid) = pid_text.trim().parse::<libc::pid_t>() {
      unsafe { libc::kill(pid, libc::SIGTERM); }
  }
  ```

  The comment at :353–355 acknowledges stale pid files from crashed tails exist — but the stale pid still gets signalled before the dir is removed. The file lives in the worktree the agent controls, so a confused (or hostile) agent can point it at any same-user process.
- **Failure scenario:** dispatch tears down a pane at the same moment a fresh wrapper is starting (restart flows do exactly this): the signal lands pre-install, the wrapper dies silently, and the orchestrator never sees a pane-death event for a pane that is in fact gone. For (c): tail crashes, its PID is recycled by an unrelated user process, next pane exit SIGTERMs it.
- **Recommendation:** install the signal listener *before* spawning (the handler only needs `child_pid` for forwarding — stash it in an `Arc<AtomicI32>` written post-spawn, forwarding only when non-zero); call `signal_handle.close()` immediately after `wait()` returns, before any other work; in `kill_task_tail`, verify the target before signalling (e.g. compare `/proc`-style process start time where available, or at minimum check the pid is a descendant / matches the expected command name via `libproc`/`ps`).
- **Effort:** M

## F12: `run_local_tmux` / `run_tmux` collapse all failures to `false` and discard stderr — undiagnosable errors

- **Where:** crates/shelbi-cli/src/commands/open.rs:153–165; crates/shelbi-cli/src/commands/palette.rs:557–567, :534, :1614–1617
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  std::process::Command::new("tmux")
      .args(args)
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status()
      .map(|s| s.success())
      .unwrap_or(false)
  ```

  "tmux binary not on PATH", "server not running", "session doesn't exist", and "window doesn't exist" are all `false`. `focus_or_create` then interprets any `false` from `select-window` as "window missing" and proceeds to `new-window`; when *that* fails the user gets `couldn't create tmux window for workspace `alpha`` with the actual tmux stderr thrown away (dead server? missing session `shelbi-alpha`? bad name?). The palette additionally ignores the return value outright at :534 (`run_tmux(["select-window", …]);`) and `switch_to_project` ignores the attach/switch status (:1614–1617) — a failed project switch reports nothing.
- **Failure scenario:** tmux server died (host reboot, `kill-server`): `shelbi open alpha` reports only "couldn't create tmux window" — the one-line stderr tmux printed (`no server running on /private/tmp/…`) that would have made the fix obvious was routed to `/dev/null`.
- **Recommendation:** capture output instead of nulling it and include trimmed stderr in the `bail!` messages; make the palette's fire-and-forget dispatches at least `eprintln!` on failure.
- **Effort:** S

## F13: `run_pick_up` duplicates the init scaffolding (already drifted: `repo:` field differs)

- **Where:** crates/shelbi-cli/src/commands/init.rs:398–407 vs :146–157; :459–474 vs :289–304; :478–494 vs :312–334
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:** `run_pick_up` re-implements, line for line: `ensure_root_subdirs` + default-session seeding (:398–407 ≡ :146–157), the project-YAML template (:459–474 ≡ :289–304), and the template/agents/statuses tail (:478–494 ≡ :312–334). The two YAML templates have *already* drifted: fresh init writes `repo: \n` (empty) while pick-up writes `repo: {root}` — same struct, different data, so the next schema change must be made twice and one copy will be missed.
- **Failure scenario:** not a runtime failure; a maintenance trap. Any future field added to the scaffold (or the F8 escaping fix) has two homes.
- **Recommendation:** extract `seed_shelbi_home()` and `write_project_yaml(name, root, repo: Option<&str>)` (or the serde struct from F8) and a shared `finish_scaffold(name)` for template + agents + statuses; both flows call them.
- **Effort:** M

## F14: palette dead code: identical match arms in `entry_from_row`, duplicated `run_tmux` helper, code after the test module

- **Where:** crates/shelbi-cli/src/commands/palette.rs:433–436, :557–567, :1182–1183 + :1603
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let id = match view {
      View::Workspace(_) => format!("workspace:{name}"),
      _ => format!("workspace:{name}"),
  };
  ```

  Both arms are byte-identical — the match is dead. `run_tmux` (:557–567) duplicates `open.rs`'s `run_local_tmux` minus the stdio-nulling (so palette tmux failures splatter stderr into the alt-screen, a third variant of F12). And `switch_to_project` (:1603) sits *below* the `#[cfg(test)] mod tests`, papered over with `#[allow(clippy::items_after_test_module)]` (:1183) instead of moving one function.
- **Failure scenario:** n/a — cleanliness. (The un-nulled stderr in `run_tmux` can visibly corrupt the popover rendering when a dispatch fails, which is a minor real effect.)
- **Recommendation:** collapse the match to a plain `format!`; share one tmux-runner helper (with the F12 stderr capture) across palette.rs and open.rs; move `switch_to_project` above the test module and drop the allow.
- **Effort:** S

## F15: `agent show`/`edit` skip the name validation `new` enforces; `read_skills` silently skips symlinked skills

- **Where:** crates/shelbi-cli/src/commands/agent.rs:135–137, :200–203 (no validation); :254 (symlinks)
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:** `new` calls `validate_agent_name(name)?` first (:179); `show` and `edit` go straight to `agent_workspace_dir(project, name)` — a bare `join` (agent_workspaces.rs:114–115) — so `shelbi agent show ../../other-project/agents/x` resolves and reads outside the project's agents dir. Local CLI + same user, so not a security boundary, but the inconsistency means the reserved-prefix rule (`_shared`) and charset rule are only enforced on one of three verbs. Separately, `read_skills` filters on `entry.file_type()?.is_file()` (:254) — `DirEntry::file_type` does **not** follow symlinks, so a skill checked in as a symlink (shared skills dir, dotfiles managers) silently vanishes from `show` with no hint.
- **Failure scenario:** user symlinks `skills/review.md` from a shared location; `shelbi agent show developer` lists `(none)` and the user concludes the skill isn't wired up.
- **Recommendation:** call `validate_agent_name` at the top of `show`/`edit` (message can stay "not found"-shaped); use `entry.metadata()?.is_file()` (follows symlinks) or `fs::metadata(&path)` for the skill filter.
- **Effort:** S

## F16: init heuristic runs unbounded `git log --format=%ae` — slow first-run prompt on huge repos

- **Where:** crates/shelbi-cli/src/commands/init/heuristic.rs:67–85
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  Command::new("git").args(["log", "--format=%ae"]).current_dir(cwd).output()
  ```

  buffers one line per commit for the entire history into memory, then dedupes — just to answer "are there ≥ 2 distinct emails?". On a monorepo-scale history (10⁶+ commits) this stalls the interactive mode prompt for seconds and allocates tens of MB, all before the user has answered anything.
- **Failure scenario:** `shelbi init` inside a large monorepo pauses noticeably between "shelbi setup" and the mode question with no indication why.
- **Recommendation:** bound it: `git log --format=%ae -n 200` is more than enough signal for a ≥2 threshold (or `git shortlog -se -n` piped through `head`). Short-circuit as soon as two distinct emails are seen if reading incrementally.
- **Effort:** S

## F17: `config dump-keybindings -o` silently overwrites the file it tells you to maintain by hand

- **Where:** crates/shelbi-cli/src/commands/config.rs:157–165
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  Some(path) => std::fs::write(&path, &yaml)
      .with_context(|| format!("writing {}", path.display()))?,
  ```

  No existence check, no prompt. The dump's own header (:141–155) and the subcommand help (:32–35) actively steer users toward `~/.shelbi/keys.yml` as the destination — the same file that holds their hand-edited overrides.
- **Failure scenario:** a user who customized `keys.yml` months ago runs `shelbi config dump-keybindings -o ~/.shelbi/keys.yml` to "see the defaults again" and irreversibly clobbers every customization with the pristine dump. `check` can't help; the file is valid.
- **Recommendation:** refuse to overwrite an existing file without `--force` (or write `keys.yml.default` and say so). One `Path::exists` + `bail!`.
- **Effort:** S

---

## Seed-question outcomes not already covered above

- **Palette actions that shell out — quoting of user-controlled values:** clean. Every tmux invocation in palette.rs goes through `Command::new("tmux").args(…)` with an argv vector — no shell interpolation anywhere in the dispatch path. The one shell string in scope, pane.rs's `shell_cmd` (:111–114) and `wrapper_invocation` (:53–60), routes every user-controlled segment through `shelbi_agent::shell_escape`, whose implementation (single-quote wrapping with `'\''` escaping, conservative pass-through charset) is correct for POSIX `sh`. The `--append-system-prompt "$(cat <escaped-rel>)"` construction (:99–106) is also safe: command-substitution output inside double quotes is not re-parsed by the shell.
- **Behavior outside tmux / `$TMUX` unset:** no panics found. `switch_to_project` (palette.rs:1608–1613) branches on `TMUX` and falls back to `attach`; `shelbi open` outside tmux degrades to the F12 "couldn't create tmux window" error (clean but uninformative). Outside a git repo, init's heuristic cleanly lands on `Global` (heuristic.rs:49–54) and `validate_root` warns rather than rejects.
- **Dead command paths from the legacy spawn flow:** the palette still builds and dispatches `agent:{id}` entries for legacy spawned agents (palette.rs:466–478, :537–539) and init's `next:` copy still tells users to run `shelbi spawn TASK --on hub --runner claude` (init.rs:112, :125). These are live code, not dead — but they keep the legacy flow load-bearing in two UX surfaces; if spawn is being retired, these are the two references to unwind first. No provably-dead paths were found in the scope files beyond F14's dead match.
