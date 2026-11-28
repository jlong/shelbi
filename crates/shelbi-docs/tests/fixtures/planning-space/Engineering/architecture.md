# Architecture

How Shelbi actually works, verified against the code as of July 2026. Every claim here traces to a crate in the workspace: `shelbi-core` (shared model), `shelbi-state` (on-disk state and paths), `shelbi-ssh` (remote transport), `shelbi-tmux` (pane plumbing), `shelbi-agent` (runner command construction), `shelbi-orchestrator` (dispatch, worktrees, git actions, zen primitives), `shelbi-tui` (sidebar and poller), `shelbi-palette` (fuzzy matcher), and `shelbi-cli` (the `shelbi` binary). Where behavior depends on project config, that is said explicitly.

The one-sentence version: Shelbi is a set of cooperating processes in tmux panes on machines you own, coordinating entirely through plain files under `~/.shelbi/` and a newline-delimited JSON socket, with git worktrees as the isolation boundary and GitHub (`gh`) as the merge machinery.

## 1. Process topology

Running `shelbi` (or `shelbi orchestrate`) boots the hub: a tmux session named `shelbi-<project>` whose dashboard window holds two panes. The left pane runs `shelbi __sidebar <project>`, a ratatui process (`shelbi-tui`) that renders the Chat/Tasks/Activity navigation and, crucially, hosts the poller. The right pane runs the orchestrator: the user's agent CLI (for example `claude`) wrapped in a zen crash-recovery lifecycle (`shelbi __zen-orch-start`, a 60-second `__zen-heartbeat` background loop, and `__zen-orch-exit` on exit), launched with the composed orchestrator instructions via `--append-system-prompt`.

The poller (`shelbi-tui/src/poller.rs`) is a supervisor thread on a fixed 5-second cadence that spawns one persistent thread per declared workspace and respawns any that die. Each workspace thread polls on `workspace_poll_interval_secs` (project config, default 5s) and per cycle: checks the ready marker (`<worktree>/.claude/shelbi-ready`), checks the agent-transition marker, checks pane liveness, runs supervision, captures a pane sample for stall/usage-limit/dialog detection, and parses the pane title for a `shelbi:<state>` marker (working, idle, blocked, review). File markers are checked before the pane title so a workspace's UI cannot hide file-based signals. One thread per workspace means a hung SSH connection blocks only that workspace.

The daemon (`shelbi daemon`, implemented in `shelbi-cli/src/commands/daemon/serve.rs`) is a separate hub-side process listening on a Unix socket at `~/.shelbi/hub.sock` (overridable via `$SHELBI_HUB_SOCK`). It takes an exclusive flock on `hub.sock.lock` so only one daemon runs, binds the socket with 0600 permissions, and accepts newline-delimited JSON with four verbs: `event` (timestamp and append a line to events.log), `request-clarification` (worker question), `message-pushed`, and `message-ack` (delivery tracking for orchestrator-to-worker messages, with a reaper thread that synthesizes `ack=timeout` events after 60 seconds, `$SHELBI_ACK_TIMEOUT_SECS` to override). Every accepted line is answered with a literal `ok` on the same connection; frames are capped at 64 KB.

Worker panes come in two shapes. A local workspace is a window in the `shelbi-<project>` session whose pane runs the lifecycle wrapper `shelbi open <ws> --as-pane`: it cds into the worktree, execs the agent runner, waits, and logs the exit reason to events.log. Environment is pinned at window creation via `tmux new-window -e` (`TASK_ID`, `PROJECT`, `SHELBI_HUB_SOCK`). A remote workspace gets its own tmux session `shelbi-w-<workspace>` with a window named `agent` on the remote machine's tmux server; there is no shelbi binary on the remote side, so the full agent command (prefixed with `SHELBI_HUB_ADDR=...` and, for Unix forwards, `SHELBI_HUB_SOCK=...`) is delivered into the pane over SSH via the tmux paste buffer, and the user's login shell rc runs first.

Review workspaces are not a separate process kind. They are declared workspaces tagged `review` in the project YAML; the review agent is loaded (not dispatched to write code) when a task reaches a handoff status, and its job is to install, build, and serve the branch so a human can click through it. When a project declares review workspaces, zen never auto-merges review-routed tasks.

```
                              hub machine
+--------------------------------------------------------------------------+
| tmux session: shelbi-<project>                                           |
|                                                                          |
|  dashboard window                          one window per local worker   |
|  +-----------------+---------------------+  +--------------------------+ |
|  | sidebar pane    | orchestrator pane   |  | shelbi open <ws> --as-pane| |
|  | shelbi __sidebar| agent CLI (claude)  |  |  cd .shelbi/wt/<ws>       | |
|  |  ratatui UI     |  zen lifecycle wrap |  |  exec agent CLI           | |
|  |  poller threads |  events tail/drain  |  |  env: TASK_ID, PROJECT,   | |
|  |  (1/workspace)  |                     |  |       SHELBI_HUB_SOCK     | |
|  +-----------------+---------------------+  +--------------------------+ |
+--------------------------------------------------------------------------+
        |  direct append / read                    |  ready marker,
        v                                          v  pane title, exit event
+--------------------------------------------------------------------------+
|  plain files under ~/.shelbi/                                            |
|    events.log   hub.sock   state.json   projects/<p>/{tasks,workflows,   |
|    agents,state.json}   workspaces/<ws>/status.yaml   ssh/  logs/        |
+--------------------------------------------------------------------------+
        ^
        |  JSON lines, verbs: event | request-clarification |
        |  message-pushed | message-ack   (daemon replies "ok")
+-------+-----------------+
|  shelbi daemon          |<---------------------------------------+
|  (~/.shelbi/hub.sock)   |                                        |
+-------------------------+                                        |
                                                                   |
   remote machine (one tmux session per remote workspace)          |
  +-----------------------------------------------+                |
  | tmux session: shelbi-w-<workspace>, window    |   ssh -R       |
  |  "agent": worker CLI in a login shell         |   reverse      |
  |  worktree: <work_dir>/.shelbi/wt/<workspace>  |   forward -----+
  |  env: SHELBI_HUB_ADDR=unix:<sock>|tcp:127...  |   (unix sock or
  +-----------------------------------------------+    TCP loopback)
```

## 2. State model

There is no database anywhere in the system. Everything is plain files, written atomically (temp file plus rename, `shelbi-state`), with advisory flocks on sibling `.lock` files serializing read-modify-write sequences. State structs carry an `extra` catch-all map so unknown fields survive round-trips across binary versions.

The root is `~/.shelbi/` (overridable via `$SHELBI_HOME`, resolved in `shelbi-state/src/root.rs`):

- `events.log`: the hub-global, append-only event stream (section 3).
- `hub.sock` and `hub.sock.lock`: the daemon's socket and single-instance lock.
- `state.json`: global UI state (palette key binding, zen intro seen, sidebar collapse).
- `projects/<name>/`: per-project state (below).
- `workspaces/<ws>/status.yaml`: the poller's view of each workspace's state.
- `ssh/`: ControlMaster sockets, mode 0700 (section 4), plus `forward-modes.json` recording per-host forward decisions.
- `agents/`, `sessions/`, `logs/`: hub-shared agent definitions, session records, TUI and orchestrator logs.

Per project, under `projects/<name>/`:

- The project YAML. In global mode this is one file holding everything; with `config_mode: in-repo` the shareable parts (name, workflows, agents, runners, zen and git config) live in `<repo>/.shelbi/project.yaml` and the user-local parts (repo path, machines, workspaces, editor) in `local.yaml`. Schema is `shelbi-core/src/model.rs`.
- `tasks/<task-id>.md`: one markdown file per task, YAML frontmatter fenced by `---` plus a free markdown body. Frontmatter fields: `id`, `title`, `column` (the status id; legacy snake_case wire forms are normalized to kebab-case on read), `priority` (position within column), `assigned_to`, `workflow`, `branch`, `depends_on` (cycles rejected at save), `prefers_machine`, `zen` (per-task overrides), `created_at`/`updated_at`, and any additional free-form keys, which are flattened into a params map and drive `{{var}}` substitution (section 5).
- `workflows/`: `statuses.yaml` (the project-wide status catalogue) plus one YAML per workflow (section 5).
- `agents/<name>/instructions.md` for each agent (`orchestrator`, `developer`, `review` ship bundled as templates in `shelbi-state`, materialized on `shelbi init` and self-healed on `shelbi reload`), `agents/_shared/preamble.md` for project-wide context, and optional per-agent `skills/`. `compose_agent_prompt()` (`shelbi-state/src/agent_workspaces.rs`) prepends the preamble, a blank line, then the agent's instructions.
- `state.json`: runtime state such as the zen mode toggle and crash timestamp.
- `event-cursor`: the durable drain cursor for polling-only orchestrator runners.

Worktree-side, Shelbi writes into the checkout it manages: `.claude/agent-instructions.md` (the composed prompt, passed to the runner via `--append-system-prompt`), `.claude/shelbi-ready` (the worker's done signal, containing the task id), `.shelbi/messages/<task-id>.log` (orchestrator-to-worker messages, tailed by a hook on the worker side), and a transition marker for agent-driven status bounces.

## 3. Event flow

`~/.shelbi/events.log` is the single ledger. It is append-only, never truncated, one event per line, each line starting with an RFC3339 timestamp and containing space-separated `key=value` tokens (values are sanitized: whitespace folds to underscores). It is hub-global; lines carry `project=<name>` so each orchestrator filters to its own project.

Who writes: the poller and the CLI append directly through the `append_*_event` functions in `shelbi-state/src/workspace_status.rs` (workspace state transitions, task transitions, dispatch, rebase, supervision, heartbeats, zen toggles). Workers, local and remote, write through the daemon: one JSON `event` line to `hub.sock`, daemon timestamps and appends, replies `ok`. If the daemon is down, hub-side writers fall back to direct `O_APPEND` (POSIX guarantees appends of this size are atomic), trading away the single-writer property; that is the documented degraded mode.

The main line shapes, from the actual format strings:

- Task transition: `<ts> project=<p> task=<id> workflow=<wf> <from> -> <to> reason=<r> from_category=<c> to_category=<c>`. Reasons include `user:cli`, `workspace:ready-marker`, `workspace:agent-transition`, `zen:auto`, `dispatch`.
- Workspace state: `<ts> project=<p> workspace=<ws> <prev> -> <new>` with states none, working, idle, blocked, paused, review; pause carries `reason=usage-limit` with an optional `reset=` hint, dialogs carry `reason=dialog:<kind>`.
- Pane death: `<ts> project=<p> workspace=<ws> pane_alive=false reason=<r>`, emitted by the `--as-pane` wrapper on exit.
- Dispatch and rebase: `<ts> dispatch task=<id> workspace=<ws> status=<s> detail=<d>` and `<ts> rebase task=<id> workspace=<ws> branch=<b> status=ok|up-to-date|conflict|skipped detail=<d>`.
- Heartbeat: `<ts> project=<p> heartbeat zen_eligible=<n> idle_workspaces=<m>`.
- Messaging: `message=<id> task=<id> push=ok`, `ack=worker` or `ack=timeout`, and `question=<id> task=<id> kind=clarification text=<folded, 120-char truncated>`.
- Supervision and mode: `supervision=restart|restart-failed|gave-up` lines and `mode=zen <prev> -> <new> reason=<source>`.

Heartbeats use adaptive backoff (`maybe_emit_heartbeat()` in `poller.rs`). Defaults from `shelbi-core/src/model.rs`: standard interval 180s, cap 3600s, both configurable per project via the `heartbeat:` block. Any real event landing in the log resets the cadence to standard; when the board is quiescent (no task in a ready, active, or handoff category) the interval doubles up to the cap. Before emitting, the poller probes connectivity with a 1-second TCP connect to 1.1.1.1:443 and skips the beat when offline.

Who reads: the orchestrator, two ways. Runners that support background monitoring run `shelbi events tail --follow` (250ms file polling from a saved offset) and get woken on new lines; runners that cannot are instructed to run `shelbi orchestrator events drain` before every reply, which reads from the durable `event-cursor` and returns the delta. The sidebar's activity feed reads the same file for the human. The reaction policy (what to do about a pane death, a conflict, a clarification) lives in the orchestrator's instructions prose, not in Rust.

```
 writers                                            readers
 -------                                            -------
 poller threads (shelbi-tui) --- direct append --+
 shelbi CLI (task moves)     --- direct append --+-->  ~/.shelbi/events.log
 orchestrator (zen toggles)  --- direct append --+          |
 pane wrapper (exit events)  --- daemon, or      |          |
                                 O_APPEND if down|          +--> orchestrator:
 workers (local and remote)                      |          |     events tail --follow
   | {"verb":"event",...} over hub.sock          |          |     or events drain (cursor)
   v                                             |          +--> sidebar activity feed
 shelbi daemon --- timestamp + append -----------+          +--> heartbeat debounce
   ^ replies "ok" per accepted line                              (log mtime check)
```

## 4. Multi-machine

`shelbi-ssh` multiplexes everything through OpenSSH ControlMaster. Every SSH invocation carries `ControlMaster=auto`, `ControlPersist=600`, `ConnectTimeout=5`, `BatchMode=yes`, `ExitOnForwardFailure=no`, `LogLevel=ERROR`, with the control path templated as `~/.shelbi/ssh/%C` (`%C` is OpenSSH's connection hash, keeping the path under the 104-byte macOS `sun_path` cap). First contact pays roughly a second to open the master; every subsequent command rides the multiplexed connection in about 10ms.

Workers on remote machines reach the hub daemon through a reverse forward. The default is a Unix forward: the local `hub.sock` is forwarded to a remote landing socket `/tmp/shelbi-hub-<uid>-<pid>-<start>.sock`, opened with `StreamLocalBindUnlink=yes`. Tailscale SSH breaks this: it binds the remote landing socket root-owned, so `shelbi-ssh` probes writability (`test -w`) and watches cleanup stderr for `Operation not permitted`; on either signal it falls back to a TCP loopback forward, sweeping ports 47100-47115 with `ExitOnForwardFailure=yes` to detect bind collisions. The decision is persisted per host in `~/.shelbi/forward-modes.json`. The worker environment encodes the outcome with a scheme tag: `SHELBI_HUB_ADDR=unix:<path>` (plus legacy `SHELBI_HUB_SOCK`) or `SHELBI_HUB_ADDR=tcp:127.0.0.1:<port>`, and worker-side shell dispatches on the prefix.

The wire contract for remote execution is `shell_escape()` in `shelbi-core/src/shell.rs`: each argv element is escaped independently, then space-joined, because the remote shell re-tokenizes the string. Characters in `[A-Za-z0-9._/:=-]` pass through bare, everything else gets single-quoted with internal quotes escaped as `'\''`, empty strings become `''`, and a leading `=` always forces quoting because zsh (the macOS default remote shell) expands unquoted leading-`=` words as `=command` filename expansion. Session names like `shelbi-w-bob:agent` survive because of that last rule.

Machines are declared in the project YAML: `name`, `kind` (local or ssh), `work_dir`, `host` (required for ssh), optional `tags` and an optional `forward` override (unix or tcp; omitted means auto-detect). Every workspace's worktree lives at `<machine.work_dir>/.shelbi/wt/<workspace>`, created with `git worktree add` from the machine's checkout by `sync_worktree()` in `shelbi-orchestrator/src/workspace.rs`, which also heals stale worktree directories whose `.git` gitlink no longer points into the repo.

## 5. Workflows engine

Workflows are YAML under the project's `workflows/` directory, parsed by `shelbi-state/src/workflows.rs` with the model in `shelbi-core/src/workflow.rs`. Status identity is split from workflow structure: `statuses.yaml` is the project-wide catalogue giving each status its `id`, display `name`, `category`, and ordering, and it is mandatory once any workflow file exists (the loader hard-fails without it). Workflow files then carry statuses in reference form only: `id`, `owner` (`user` or `agent`, a closed enum), an optional `agent` name binding the status to an agent definition, and optional routing `tags`. Inline `name`/`category` in a workflow file is rejected.

Categories are the closed semantic vocabulary the rest of the system keys on: `backlog`, `ready`, `active`, `handoff`, `done`, `archived`. Dispatch, zen eligibility, heartbeat quiescence, and event `from_category`/`to_category` fields all reason in categories, so custom statuses get correct behavior for free. The bundled default workflow maps backlog, todo (ready), in-progress (active), review (handoff), done, and canceled (archived), with the orchestrator owning backlog and review, the developer owning in-progress.

Transitions are declared as `from`/`to` pairs with an `actions` list. The action enum is `push_branch`, `open_pr`, `merge`, `close_pr`, `delete_branch`, and `restack`, executed by the git/gh primitives in `shelbi-orchestrator/src/actions.rs` (`merge` auto-restacks dependent child branches). A transition can also carry `run` (shell commands executed in the worktree), a `ready` probe command polled until exit 0 (default timeout 90s, `ready_timeout` to override), and a `target` branch override.

Git configuration cascades. The project-level `git:` block sets `base_branch`, `branch_prefix`, and `merge_strategy` (`squash` is the default; `merge` and `rebase` map to the corresponding `gh pr merge` flags); a workflow's own `git:` block overrides it. `base_branch`, `branch_prefix`, and transition `target` accept `{{var}}` placeholders resolved from the task's flattened frontmatter params (string values only), so one workflow definition serves many tasks: a task with `component: tui` in its frontmatter can flow through `branch_prefix: "{{component}}"`. Resolution lives in `resolve_git()` / `resolve_transition_target()` in `shelbi-core/src/workflow.rs`.

Each workflow may also carry a `zen:` block overriding the project's zen `checks`, `ci_timeout`, and `danger_paths` for tasks flowing through it.

## 6. Zen mode

Zen mode is the auto-merge escalation, and its architecture is deliberately split: the mechanics are Rust, the judgment is prose. The per-project mode (`off`, `paused`, `on`) lives in `state.json` and every toggle is logged with its source (`user:cli`, `user:hotkey`, `user:palette`, `system:crash-recovery`).

The primitives live in `shelbi-orchestrator/src/zen.rs` and surface as CLI subcommands the orchestrator invokes:

- `zen scan`: lists mechanically eligible backlog tasks.
- `zen probe <task>`: runs on the assigned workspace (the branch only exists there until pushed), optionally rebasing onto the default branch first, and reports JSON: head SHA, local check results (exit code, duration, output tail), merge and rebase conflicts, diff size, and danger-path matches.
- `zen pr-create <task>`: pushes the branch and opens a PR via `gh pr create`; idempotent (returns the existing open PR).
- `zen ci-watch <pr>`: polls `gh pr checks --required`, falling back to all reported checks, and prints a verdict line: `green`, `red:<check>:<summary>`, or `timeout` (default budget 900s, `ci_timeout` config).
- `zen pr-merge <pr>`: merges with the configured strategy, optionally pinned with `--match-head-commit <sha>` so the merge lands exactly the probed commit, then deletes the remote branch (the local one stays checked out in the workspace).

Everything above is mechanism. The policy, meaning when to promote a backlog task and what clears the merge bar, lives in the orchestrator's instructions (`shelbi-state/src/default_orchestrator.md.template`, user-editable per project), not in Rust. The shipped prose requires local checks green, no conflicts, diff at most 30 files and 2000 changed lines, and no danger-path matches before running pr-create, ci-watch, pr-merge. Danger paths have a built-in base list (`.github/workflows/**`, `scripts/install.sh`, `*.yaml`, `*.yml`, `LICENSE`, lockfiles), extended by detected project shape (Cargo workspace, Node, Docker, and so on) and by config, with extend and override modes; overrides cascade project, then workflow, then task frontmatter. Two hard rails are enforced regardless of prose: an orchestrator crash auto-disables zen for a 1-hour recovery window (`zen_last_crashed_at` in state.json), and projects with review workspaces never auto-merge review-routed tasks.

## 7. Guard rails

Three mechanical rails protect the hub checkout and dispatch integrity.

The hub branch guard (`shelbi-orchestrator/src/githook.rs`) installs a pre-commit hook into the hub checkout at project open, identified by a `# shelbi-managed: hub-default-branch-guard` marker so it overwrites its own prior versions but never touches user-authored hooks, and honoring `core.hooksPath`. It rejects commits while HEAD is attached to a protected branch (`main`, plus `git.base_branch` when different), allows detached HEAD (the squash-merge temp worktree needs it), and can be bypassed explicitly with `SHELBI_ALLOW_DEFAULT_BRANCH_COMMIT=1`.

Dispatch verifies before it trusts. After `sync_worktree()` succeeds and before any pane is touched, `verify_worktree_on_branch()` runs `git rev-parse --abbrev-ref HEAD` in the worktree and hard-fails the dispatch if the output is not exactly the task's branch (detached HEAD fails too), pointing at manual repair or `shelbi task start --force`.

`sync_worktree()` itself hard-fails rather than guessing: a dirty worktree (user changes outside `.claude/` and `.shelbi/`), a failed fetch or worktree operation, or an invalid branch name aborts the dispatch, and the failure is recorded to events.log as a sync-failed event so the orchestrator and the activity feed see the stall.

Supervision closes the loop on crashes. The poller's supervision pass auto-restarts dead workspace panes and the orchestrator pane, emitting `supervision=restart`, `restart-failed`, or `gave-up` events, and the pane wrapper's own exit event (`pane_alive=false reason=...`) survives even a dead daemon via the direct-append fallback.

## 8. Release pipeline

Releases are tag-triggered: pushing a `v*.*.*` tag runs `.github/workflows/release.yml`. The `validate` job is the version guard: `scripts/release/check-version.sh` asserts the tag matches the workspace `Cargo.toml` version, then runs `cargo test --workspace` and a GoReleaser config check. `smoke` builds a release binary for `x86_64-unknown-linux-gnu` and runs `--version` (Linux only; macOS is exercised on the maintainer's machine). The `release` job runs on `macos-14` behind a GitHub `release` environment approval, verifies the tag points at the checked-out commit, and runs GoReleaser with `cargo-zigbuild` cross-compiling three targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`. Artifacts per `.goreleaser.yaml`: tar.gz archives, an amd64 `.deb` via nfpm (depending on tmux >= 3.2, git, openssh-client), a SHA256 `checksums.txt`, a GitHub release, and a build-provenance attestation over the checksums.

Two publication jobs fan out from `release`, each gated on repository variables so forks without secrets skip them cleanly. `homebrew-pr` downloads the release checksums, runs `scripts/release/update-homebrew-formula.rb`, and opens a PR against the tap named by `vars.HOMEBREW_TAP_REPOSITORY`. `apt-publish` downloads the `.deb`, imports the GPG signing key from secrets, builds a signed repository layout with `scripts/release/build-apt-repo.sh` (suite: stable), asserts the expected pool/dists/keyring layout, and pushes it to the repo named by `vars.APT_REPO` (defaulting to `<owner>/shelbi-apt`), which serves `apt.shelbi.dev` (`vars.APT_BASE_URL`). A final `apt-verify-install` job installs Shelbi from the freshly published hosted repository on a clean runner and runs `shelbi --version`, so a broken publish fails the workflow rather than the next user. `release-apt.yml` is a manual backfill path for republishing APT from an existing tag, and day-to-day CI is `app-ci.yml`: `cargo build`, `cargo clippy -D warnings`, `cargo test` across the workspace.
