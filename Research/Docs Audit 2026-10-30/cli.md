# Docs audit: CLI reference

Audited at origin/main a749b0d1, shelbi 0.11.0 (installed binary built 2026-10-29 14:22; no CLI-crate commits on origin/main since), 2026-10-29.

## Summary
Pages audited (24): every page in `site/content/docs/cli/` (agent, attach, changelog, config, daemon, detach, events, init, issue, machine, message, open, project, question, relay, reload, remote, send, session, status, wizard, workflow, workspace, zen). Each was diffed against `shelbi <cmd> --help` and every `shelbi <cmd> <sub> --help` (114 help screens), checked against source at origin/main, and run read-only where it was safe to do so.

Totals: 123 findings.
- wrong: 19
- outdated: 12
- missing: 57 (50 on existing pages, plus 7 top-level commands with no page)
- unclear: 34
- broken-link: 1

Findings per page (wrong / outdated / missing / unclear / broken-link):
- agent: 4 / 0 / 1 / 2 / 0
- attach: 0 / 0 / 1 / 2 / 0
- changelog: 0 / 0 / 1 / 2 / 0
- config: 0 / 0 / 2 / 1 / 0
- daemon: 1 / 2 / 3 / 2 / 0
- detach: 1 / 1 / 0 / 1 / 1
- events: 1 / 1 / 1 / 3 / 0
- init: 0 / 0 / 3 / 5 / 0
- issue: 2 / 0 / 5 / 1 / 0
- machine: 0 / 0 / 4 / 0 / 0
- message: 0 / 0 / 2 / 2 / 0
- open: 0 / 0 / 1 / 1 / 0
- project: 1 / 0 / 2 / 0 / 0
- question: 0 / 0 / 3 / 0 / 0
- relay: 0 / 0 / 0 / 1 / 0
- reload: 1 / 2 / 2 / 2 / 0
- remote: 1 / 0 / 2 / 0 / 0
- send: 1 / 2 / 1 / 0 / 0
- session: 1 / 0 / 3 / 3 / 0
- status: 0 / 0 / 3 / 0 / 0
- wizard: 0 / 0 / 1 / 3 / 0
- workflow: 1 / 2 / 1 / 2 / 0
- workspace: 3 / 1 / 2 / 1 / 0
- zen: 1 / 1 / 6 / 0 / 0

Biggest items:
- **Seven visible top-level commands have no page:** quit, doctor, orchestrate, orchestrator, msrv-check, guard, action.
- **reload.mdx is the most out of date.** Per-pane targets are now ignored, and every reload is a whole-project reload.
- **machine.mdx omits `machine check`.**
- **workspace.mdx omits `add`/`rm`** and says `init` leaves an empty pool. `init` actually creates `dev` and `review`.
- **zen.mdx** is out of date on `pr-merge` (it now uses GitHub's own squash merge when required-PR rules apply, and can print `sha-pending`) and on the "one line on stdout" claim.
- **agent.mdx** gives the wrong editor resolution order.
- **send.mdx** still describes the removed legacy registry fallback.

Unverified: the `shelbi detach --list` table format was checked against source only. Running it was skipped under the hub-safety rules.

## Findings

### Top-level commands with no page (`shelbi --help`)
`site/content/docs/cli/` has 24 pages. `shelbi --help` lists 31 commands, not counting `help`. `issue-store` is covered by the `## issue-store migrate` section of issue.mdx. Seven visible commands have no page. Hidden commands (`task`, `worker`, `__codex-orchestrator`, `zen-orch-start`, `zen-heartbeat`, `ensure-daemon`, `control-protocol` and the other `hide = true` entries in main.rs) are internal and correctly have no page. Note that `relay` is hidden too but does have a page; see relay.md.

- [severity: missing] "(no page)" `shelbi quit [--all]`
  - Reality: This quits the current project. It ends the project's sessions after the orchestrator writes its handoff, marks the project closed, and leaves worktrees and branches intact. `--all` closes every project, ends all sessions and stops the hub daemon. With no daemon running, it reports that and exits 0. detach.mdx L18 already links `shelbi quit` to `/docs/cli/open`, which never mentions quit (see detach findings).
  - Evidence: `shelbi quit --help`; crates/shelbi-cli/src/main.rs `Quit { all }` → `commands::quit::run(cli.project, all)` (main.rs:537)
  - Fix: Add `site/content/docs/cli/quit.mdx` with usage `shelbi quit [OPTIONS]`, the `--all` flag, and the behavior above. Point the detach.mdx L18 link at `/docs/cli/quit`.
- [severity: missing] "(no page)" `shelbi doctor`
  - Reality: Runs health checks on the project's GitHub API budget: the observed request rate, the remaining budget, the projected time to exhaustion, and a warning that names the top callers when a budget would run out within 30 minutes. It has no command-specific flags.
  - Evidence: `shelbi doctor --help`
  - Fix: Add `site/content/docs/cli/doctor.mdx` with the description and a sample of real output (run `shelbi doctor` in a project that has a github issue store).
- [severity: missing] "(no page)" `shelbi orchestrate [--status]`
  - Reality: Starts the orchestrator agent as the project's orchestrator session. `--status` prints attach instructions and exits, even when the orchestrator is already running.
  - Evidence: `shelbi orchestrate --help`; main.rs:534
  - Fix: Add `site/content/docs/cli/orchestrate.mdx`, or document it as a section of an orchestrator page.
- [severity: missing] "(no page)" `shelbi orchestrator events {next,drain,ack}`
  - Reality: "Machine-readable orchestrator transport primitives." It reads project-scoped events since a cursor. It isn't hidden, but it's plumbing for the orchestrator agent.
  - Evidence: `shelbi orchestrator --help`, `shelbi orchestrator events --help` (subcommands `next`, `drain`, `ack`); main.rs:266-270
  - Fix: Either add a short page that marks it as orchestrator plumbing, or hide it in clap (`#[command(hide = true)]`) like the other internal commands. This needs a product decision, so flag it for the fix task rather than writing a full reference.
- [severity: missing] "(no page)" `shelbi msrv-check`
  - Reality: Builds the workspace on the `rust-version` declared in Cargo.toml (`cargo +<rust-version> check --workspace --all-targets --locked`), mirroring CI's `msrv` job. It's meant as a `zen.checks.local` entry. It installs the toolchain once if it's missing, and exits 0 (skips) when the toolchain can't be provisioned.
  - Evidence: `shelbi msrv-check --help`
  - Fix: Add `site/content/docs/cli/msrv-check.mdx`, and link it from the `zen.checks.local` configuration docs.
- [severity: missing] "(no page)" `shelbi guard {install,uninstall,status}`
  - Reality: Manages the Shelbi-managed `pre-commit` hook in the hub checkout. The hook blocks default-branch commits, but only from inside a Shelbi-managed agent pane; your own shell is never governed. `install` discloses what the hook does before writing it, and `uninstall` never touches a user-authored hook. `init` and `wizard` install this hook automatically (see the init and wizard findings).
  - Evidence: `shelbi guard --help` and its `install`/`uninstall`/`status --help`
  - Fix: Add `site/content/docs/cli/guard.mdx`, and link it from init.mdx and wizard.mdx.
- [severity: missing] "(no page)" `shelbi action {push-branch,open-pr,close-pr,merge,delete-branch,restack,apply-transition}`
  - Reality: These are single-purpose, idempotent git/gh primitives that a workflow's `transitions:` block sequences. `open-pr` prints the PR number. `merge` prints `pr:<n>:<sha>` or `hub:<target>:<sha>` and auto-restacks dependent children. `delete-branch` prints `deleted`, `skipped:<reason>` or `not-present`. `restack` prints `skipped:<id>:already-restacked` when there's nothing to do. `apply-transition` fires a transition's declared `actions:` and prints `<action>\t<result>` for each.
  - Evidence: `shelbi action --help` and the `--help` of each subcommand
  - Fix: Add `site/content/docs/cli/action.mdx` with one section per subcommand. Link it from the configuration docs' workflow `transitions:` section. Separately, the `--help` text leaks internal references (`shelbi_orchestrator::actions::open_pr`, `Plans/workflows.md §12`); see the code notes below.

### site/content/docs/cli/agent.mdx
- [severity: wrong] "Open the agent's `instructions.md` in `$EDITOR` (falling back to `$VISUAL`, then `vim`)." (L104-105)
  - Reality: `$VISUAL` is tried first, then `$EDITOR`, and the fallback is `vi`, not `vim`. A blank value is skipped, and a value with arguments (`code -w`) is split on whitespace. The binary's own `--help` text repeats the wrong order (a source doc-comment bug too).
  - Evidence: crates/shelbi-cli/src/commands/mod.rs:130-147 (`resolve_editor_command`: `for var in ["VISUAL", "EDITOR"]` … `("vi".to_string(), …)`), test `splits_args_and_honors_visual_before_editor`; crates/shelbi-cli/src/commands/agent.rs:39-41 (stale doc comment).
  - Fix: "Open the agent's `instructions.md` in `$VISUAL`, falling back to `$EDITOR`, then `vi`."
- [severity: wrong] the `shelbi agent list` example table (L42-50)
  - Reality: (1) Rows are sorted by name, not listed in shipped order. (2) STATUSES lists status **ids** (`in-progress`, `todo`), sorted and comma-joined, not display names like `InProgress, Todo`. (3) On a fresh project the orchestrator owns `backlog, todo`, developer owns `in-progress`, and review owns `review` (shipped `task` workflow). (4) `review` shows SKILLS `0`: its bundled skill is `skills/load-run-detection/SKILL.md`, and the count only includes `*.md` files directly in `skills/`. Real output from a fresh `shelbi init -y` project:
    ```
    AGENT          STATUSES                   SKILLS  CUSTOMIZED
    adversarial    -                          0       no
    developer      in-progress                0       no
    orchestrator   backlog, todo              0       no
    qa             -                          0       no
    review         review                     0       no
    security       -                          0       no
    ```
  - Evidence: crates/shelbi-cli/src/commands/agent.rs:61-97; crates/shelbi-state/src/agent_workspaces.rs:568-593 (`list_agents` sorts), 194-197 (REVIEW_SKILLS rel_path `load-run-detection/SKILL.md`), 732-757 (`count_agent_skills` counts top-level files only); crates/shelbi-core/src/workflow.rs task_workflow (review → agent `review`); sandboxed `shelbi agent list` above.
  - Fix: Replace the table with the real output above. Change L37 to "the workflow status ids that reference it".
- [severity: wrong] "the count of skill files under its `skills/` dir" (L38-39) and "a `Skills:` section listing each `skills/*.md` file" (L66-67)
  - Reality: only `*.md` files directly in `skills/` are counted (`list`) or listed (`show`). Directory-style skills (`skills/<name>/SKILL.md`, the Claude Code layout the review agent ships) are not counted or listed, so `shelbi agent show review` prints `Skills:\n  (none)`. `show` prints each skill as `- <stem> — <description>`, or `(no description)`.
  - Evidence: crates/shelbi-state/src/agent_workspaces.rs:732-757; crates/shelbi-cli/src/commands/agent.rs:166-177, 221-258; `shelbi agent show review | tail -3` → "Skills:\n  (none)" while `~/.shelbi/projects/shelbi/agents/review/skills/load-run-detection/` exists.
  - Fix: Say "the number of `*.md` files directly in `skills/` (directory-style skills such as `skills/<name>/SKILL.md` aren't counted)", and add the same caveat to `show`. Separately, flag the code gap: the shipped review skill is invisible to both commands.
- [severity: wrong] "may only contain `a-z`, `0-9`, `-`, `_`" (L90)
  - Reality: the validator uses `is_ascii_alphanumeric`, so uppercase `A-Z` is accepted too (the CLI's own error message also says `a-z`). `show` and `edit` run the same validator.
  - Evidence: crates/shelbi-cli/src/commands/agent.rs:282-301.
  - Fix: "may only contain ASCII letters, digits, `-` and `_`" (or tighten the code to lowercase if lowercase-only is the intent).
- [severity: unclear] "The defaults are seeded on first load and never clobbered on upgrade." (L24-25)
  - Reality: the defaults are written at project creation, and `shelbi init`/`shelbi reload` self-heal them. An `instructions.md` you edited is kept. An unedited default from an older release ("pristine-stale") is replaced with the new bundled default. Required orchestrator sections (`## Polling-only event drain`) are appended even to a customized orchestrator prompt.
  - Evidence: crates/shelbi-state/src/agent_workspaces.rs:28-36, 177-180; crates/shelbi-cli/src/commands/agent.rs:101-128.
  - Fix: "Shelbi writes the defaults when it creates the project. On `shelbi reload` it updates any default you haven't edited to the current release's version, and leaves edited ones alone (except that runner-required orchestrator sections are appended if missing)."
- [severity: missing] "Each has an `instructions.md` (its prompt) and an optional `skills/` directory." (L21-22)
  - Reality: each shipped agent directory also holds `agent.yaml` (runner manifest: `preferred_runner`, per-runner `model`/`reasoning_effort`, `permissions_mode`) and `settings.json` (Claude Code hooks). `agent new` creates only `instructions.md` and an empty `skills/`.
  - Evidence: crates/shelbi-state/src/agent_workspaces.rs:199-215, 276, 369-377; `ls ~/.shelbi/projects/shelbi/agents/developer` → agent.yaml, instructions.md, settings.json, skills.
  - Fix: Add "Shipped agents also carry `agent.yaml` (which runner and model the agent uses) and a `settings.json` for Claude Code hooks; `agent new` scaffolds only `instructions.md` and `skills/`."
- [severity: unclear] "To see the shared preamble, `cat ~/.shelbi/projects/<name>/agents/_shared/preamble.md`." (L72-73)
  - Reality: that path is right only in global mode. In in-repo mode it's `<repo>/.shelbi/agents/_shared/preamble.md` (agents_dir is config-mode aware). Also, `<name>` here means the project, while `<NAME>` elsewhere on the page means the agent.
  - Evidence: crates/shelbi-state/src/lib.rs:773-778.
  - Fix: "`cat` the project's `agents/_shared/preamble.md` (`~/.shelbi/projects/<project>/agents/_shared/preamble.md` in global mode, `<repo>/.shelbi/agents/_shared/preamble.md` in in-repo mode)."

### site/content/docs/cli/attach.mdx
- [severity: unclear] "`attach` connects *your* terminal to that session directly, from any terminal on the hub or over SSH." (L20-21)
  - Reality: `attach` only finds sessions in the local `<root>/sessions/` directory; it doesn't go through the relay. A remote workspace's session runs on its own machine, so from the hub `shelbi attach delta` fails ("no sessions on this machine" / "no session matching"). To attach to a remote workspace you SSH to that machine and run `shelbi attach` there.
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:127-131 (`discover` reads `sessions_dir()`), 317-330, 349-350, 422
  - Fix: "… from any terminal on the machine where the session runs. For a workspace on a remote machine, SSH to that machine and run `shelbi attach` there."
- [severity: unclear] "Shelbi shows the detach hint on a one-line bar." (L25-26)
  - Reality: The hint is painted on the bottom row only for 4 seconds after attaching, then disappears.
  - Evidence: crates/shelbi-cli/src/commands/session_attach.rs:22-23, 60-61 (`HINT_SECS = 4`), 205-249
  - Fix: "Shelbi shows the detach key on a one-line hint along the bottom row for a few seconds after you attach."
- [severity: missing] "(not documented)" — selector matching, dead sessions, `--detach-key` syntax (L32, L38)
  - Reality: `<WORKSPACE>` is a session selector, tried in order: exact short id, exact session name, workspace (last `/` segment of the name), unique short-id prefix, unique name substring. Ties prefer sessions under `<project>/` (this is what `-p` affects), then the single live one. A dead session is refused ("… is dead; `shelbi session snapshot` shows its final screen"). `--detach-key` takes `-`/`+`-separated modifiers (`ctrl`, `alt`/`opt`/`meta`, `shift`, `super`/`cmd`) plus one key (a character, `enter`, `esc`, `tab`, `space`, `backspace`, `delete`, `fN`), case-insensitive.
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:334-434; crates/shelbi-cli/src/commands/session_attach.rs:76-133
  - Fix: `<WORKSPACE>` row: "Workspace name (or a session name / short id from `shelbi session ls`) to attach to (required). Must be a live session." `--detach-key` row: "Key that detaches, written as modifiers plus a key joined by `-` or `+`, e.g. `ctrl-]`, `ctrl-b`, `alt-d`, `esc`, `f12`." `-p` row: "Project used to break ties when several sessions match."

### site/content/docs/cli/changelog.mdx
- [severity: unclear] "~/.shelbi/projects/<project>/changelog/2026-10-28-213005-changelog-lands.md" (L28; same id at L155, L172, L182)
  - Reality: the slug is built from the whole title (lowercase ASCII words joined by `-`, capped at 48 chars, `entry` if nothing survives). The example entry's title "Changelog lands in the sidebar" would be stored as `2026-10-28-213005-changelog-lands-in-the-sidebar.md`, not `...-changelog-lands.md`. Long titles get cut at a word boundary (real id: `2026-10-31-020827-desktop-project-menu-is-inset-12px-from-the`).
  - Evidence: crates/shelbi-state/src/changelog.rs:620-649 (`file_stem`, `slug`); `shelbi changelog list --limit 3` output above.
  - Fix: Use `2026-10-28-213005-changelog-lands-in-the-sidebar` everywhere the example id appears (L28, L155, L172, L182), and extend L31-32 to: "Files are named `<date>-<time>-<slug>.md` from the entry's date in UTC, so they sort by date. The slug is the title in lowercase words joined by `-`, cut to at most 48 characters. The file name without `.md` is the entry's id."
- [severity: missing] (not documented) (L119-120, add table rows or a note after the `add` table)
  - Reality: `add` checks its link flags before it writes anything. `--issue` must be a valid issue id (`--issue Bad Id` → `--issue Bad Id: ...`). `--pr` must be a number, `#number` or URL (`--pr twelve: expected a number or a URL`). `--link` must start with `http://` or `https://` (`--link x.dev: expected an http(s) URL`). An unreadable `--date` fails with `--date: unreadable date ... (try YYYY-MM-DD or RFC 3339)`. A title or author with a newline is rejected.
  - Evidence: crates/shelbi-cli/src/commands/changelog.rs:198-216; crates/shelbi-state/src/changelog.rs:575-586.
  - Fix: Add after the `add` table: "`add` refuses an id that isn't a valid issue id, a `--pr` that isn't a number or URL, a `--link` that isn't an `http(s)` URL, an unreadable `--date`, and a title or author longer than one line. It writes nothing when one is refused."
- [severity: unclear] "**text** is a compact list: the date and title, then the id, the author and the links." (L150-151)
  - Reality: Correct, but the empty cases aren't documented. `list` prints `No changelog entries yet.` and `search` prints ``No changelog entries match `<q>`.`` (text format only; JSON gives `"entries": []`). Files that fail to parse are skipped with a stderr warning `shelbi changelog: skipping <path>: <why>`.
  - Evidence: crates/shelbi-cli/src/commands/changelog.rs:177-183, 251-254; `shelbi changelog search zzzqqq` → "No changelog entries match `zzzqqq`."
  - Fix: Add: "With nothing to show, text prints `No changelog entries yet.` (or ``No changelog entries match `<query>`.`` for `search`). An entry file that can't be read is skipped with a warning on stderr."

### site/content/docs/cli/config.mdx
- [severity: unclear] "A staged snapshot is only valid for the `shelbi` build that produced it: linting a snapshot whose version a newer or older binary does not recognize is refused rather than misread." (L182-L185); also "A newer staged snapshot is rejected by an older `shelbi`" (L59)
  - Reality: `lint --staged` checks only `schema_version` against `INVENTORY_SCHEMA_VERSION` (currently 1), using strict equality. It never compares `shelbi_version`. So another build with the same schema version lints the snapshot fine, and the rejection works in both directions: a newer binary also rejects an older-schema snapshot. The error reads `unsupported staged inventory schema version N (this shelbi supports M)`.
  - Evidence: crates/shelbi-cli/src/commands/config_surfaces.rs:24 (`INVENTORY_SCHEMA_VERSION: u32 = 1`), :184-190.
  - Fix: L59: "A snapshot whose `schema_version` differs from the running `shelbi`'s is rejected (see [Compatibility](#compatibility))." L182-L185: "A staged snapshot is only valid for a `shelbi` that supports the same `schema_version`. Linting a snapshot with any other schema version is refused (`unsupported staged inventory schema version …`) rather than misread. When in doubt, take a fresh `inventory` with the `shelbi` you are running."
- [severity: missing] Lint scope bullets (L89-L95) omit surfaces that `inventory` stages and `lint` checks
  - Reality: Inventory also includes the per-project `pr-template` and `changelog-template` (markdown), each agent's `agent.yaml` manifest, and every workspace's `.claude/settings.local.json` (`project.<p>.workspace.<ws>.settings-local`). Lint checks each of these by format: JSON must parse, Markdown must not be empty.
  - Evidence: config_surfaces.rs:466-529 (template, settings-local, pr-template, changelog-template entries), :1150-1157 (format dispatch); real `shelbi config inventory` lists `project.shelbi.pr-template`, `project.shelbi.changelog-template`, `project.shelbi.agent.*.manifest`, `project.shelbi.workspace.alpha.settings-local`.
  - Fix: Extend the third bullet: "per-project Markdown (Zen Mode policy, agent instructions, the shared preamble, PR and changelog templates), per-agent `agent.yaml`, `settings.json` and skills, and each workspace's `.claude/settings.local.json`."
- [severity: missing] "Print every action with its mode, name, description, and the chord(s) bound to it **after** the `keys.yaml` merge" (L147-L150)
  - Reality: The merge includes the `projects.<name>` override layer for the resolved project (`-p`, `$SHELBI_PROJECT`, or the cwd project). Outside a project it shows the global merge only. `check` resolves the project the same way. Output is a fixed-width table: `MODE  ACTION  DESCRIPTION  CHORDS`.
  - Evidence: config.rs:316-331 (`resolve_project` → `load_keymaps(project)`); test `list_actions_honors_per_project_override` (config.rs:649); real output header `MODE      ACTION                    DESCRIPTION …  CHORDS`.
  - Fix: Append: "Per-project overrides for the resolved project (`-p`, `$SHELBI_PROJECT`, or the current directory) are applied too."

### site/content/docs/cli/daemon.mdx
- [severity: outdated] "`shelbi daemon` runs the hub-side daemon that listens on `~/.shelbi/hub.sock` (overridable via `$SHELBI_HUB_SOCK`) for worker messages and appends `event`-verb payloads to [`~/.shelbi/events.log`]" (L11-13; same framing in frontmatter `summary`, L4)
  - Reality: Accurate but incomplete. The same process also binds the mutation control socket `~/.shelbi/control.sock` (overridable via `$SHELBI_CONTROL_SOCK`). It runs one workspace poller per open project (session liveness, the reverse-forward recheck described at L68+), one board-index refresh loop per open project, the unacked-message reaper, and the idle-exit monitor. The page's own sandbox example unsets `SHELBI_CONTROL_SOCK` but never says what it is.
  - Evidence: crates/shelbi-state/src/workspace_status.rs:297-307 (`control_socket_path`: "The daemon binds both in one process"); crates/shelbi-cli/src/commands/daemon.rs:13-25 (serve/board/poller/lifecycle submodules); crates/shelbi-cli/src/commands/daemon/poller.rs:1-13. `ls ~/.shelbi` shows `hub.sock`, `hub.sock.lock`, `control.sock`, `shelbi.pid`.
  - Fix: After L13 add: "The same process also listens on `~/.shelbi/control.sock` (overridable via `$SHELBI_CONTROL_SOCK`) for the board and issue mutations clients send. For each open project it runs the workspace poller and the board-index refresh."
- [severity: missing] "it exits once no project is open" (L16; also L24, L31)
  - Reality: Idle exit has a minimum lifetime of 15 s after start, then the daemon checks every 5 s and exits once the open-project set is empty. Both timings can be overridden in milliseconds with `SHELBI_DAEMON_IDLE_GRACE_MS` and `SHELBI_DAEMON_IDLE_POLL_MS`, which exist mainly for tests. `daemon status` prints `open:       (no open projects — will exit when idle)` in that state.
  - Evidence: crates/shelbi-cli/src/commands/daemon/serve.rs:412-421 (`IDLE_GRACE` 15s, `IDLE_POLL` 5s, env names), :319-324; crates/shelbi-cli/src/commands/daemon/lifecycle.rs:315.
  - Fix: Change L16 to "…and it exits shortly after no project is open (it stays up at least 15 seconds after starting, then checks every 5 seconds)."
- [severity: missing] (not documented) — `status` output shape (L25, L99-103)
  - Reality: Running prints a header plus `lock held`, `listening`, `version`, `protocol`, `pid`, `pid record` and `open` lines. Not running prints `shelbi daemon: not running` / `  (started on demand when you open a project)`. Both exit 0.
  - Evidence: crates/shelbi-cli/src/commands/daemon/lifecycle.rs:280-318. Real `shelbi daemon status`:
    ```
    shelbi daemon: running
      lock held:  yes
      listening:  yes
      version:    0.11.0
      protocol:   1
      pid:        64129
      pid record: 0.11.0
      open:       shelbi
    ```
  - Fix: Add that sample output under the `shelbi daemon status` example at L101-103.
- [severity: missing] (not documented) — `stop`/`restart` messages and the refusal error (L42-48)
  - Reality: `stop` prints `✓ stopped the hub daemon (pid N, from <root>/shelbi.pid)` or `shelbi daemon: not running under <root>`, and exits 0 in both cases. A refusal exits non-zero with `not stopping the hub daemon: <reason>`, or `not restarting the hub daemon: <reason>` for `restart`. `restart` also retires any leftover launchd/systemd supervisor unit first (`✓ retired leftover supervisor unit <path>`). It waits up to 10 s for the new daemon, then prints `✓ hub daemon restarted on the current binary`.
  - Evidence: crates/shelbi-cli/src/commands/daemon/lifecycle.rs:218-275, :41 (`RESTART_START_DEADLINE` 10s).
  - Fix: After L48 add: "A refusal exits non-zero with `not stopping the hub daemon: <reason>`. A `stop` with nothing running prints `shelbi daemon: not running under <root>` and succeeds."
- [severity: unclear] "`-p, --project <PROJECT>` | string | env / cwd lookup | Project to operate on." (L66)
  - Reality: The daemon is hub-global. `daemon::run` takes no project, so `-p` has no effect on any daemon subcommand. The daemon serves every open project, and `stop`/`restart`/`status` act on the root's daemon.
  - Evidence: crates/shelbi-cli/src/commands/daemon.rs:56-62 (`pub fn run(cmd: Option<DaemonCmd>)`, no project parameter).
  - Fix: Drop the `-p` row, or replace it with "`-p, --project` — accepted (global) but ignored; the daemon serves every open project."
- [severity: outdated] "The TUI poller rechecks that forward on a slow cadence and repairs it if it has gone stale." (L71-72)
  - Reality: The workspace poller now runs inside the daemon, one per open project, not in the TUI. The recheck interval is 120 s.
  - Evidence: crates/shelbi-cli/src/commands/daemon/poller.rs:1-9; crates/shelbi-orchestrator/src/poller.rs:102 (`FORWARD_RECHECK_INTERVAL` 120s); crates/shelbi-ssh/src/lib.rs:71 ("runs on a slow (120s) cadence in its own per-workspace thread").
  - Fix: "The daemon's per-project workspace poller rechecks that forward every 120 seconds and repairs it if it has gone stale."
- [severity: wrong] "only an outage that survives the whole retry budget logs `detail=master_open_failed attempts=N status=failed`" and "`detail=loopback_port_exhausted band=<lo>-<hi>`" (L80-83)
  - Reality: The real lines are `ssh reverse-forward host=<h> mode=tcp status=failed detail=master_open_failed attempts=N` (status comes before detail) and `… mode=tcp status=failed detail=loopback_port_exhausted band=<lo>-<hi> tried=<n>`. Anyone grepping for the documented field order won't match.
  - Evidence: crates/shelbi-ssh/src/lib.rs:1482-1486, :1576-1583.
  - Fix: Quote `status=failed detail=master_open_failed attempts=N` and `detail=loopback_port_exhausted band=<lo>-<hi> tried=<n>`, or quote only the `detail=` tokens.
- [severity: unclear] "`SHELBI_FORWARD_RETRY_BACKOFF_MS` | `250` | … Clamped to `<= 5000`." (L93)
  - Reality: Correct. The page doesn't say that `SHELBI_TCP_FORWARD_PORT_BASE`/`_SPAN` (and all four variables) are read from the daemon's environment. Daemons are started on demand and inherit the env of whichever client started them, so setting a variable in a shell has no effect until the daemon is restarted from that environment.
  - Evidence: crates/shelbi-ssh/src/lib.rs:82-93 (read via `std::env::var` at call time inside the daemon process); crates/shelbi-state/src/ssh_control.rs:492-515.
  - Fix: Add under the table: "These are read by the daemon process. Export them, then run `shelbi daemon restart` from that shell."

### site/content/docs/cli/detach.mdx
- [severity: broken-link] "[`shelbi quit`](/docs/cli/open) ends a project's sessions" (L18)
  - Reality: /docs/cli/open documents `shelbi open` and never mentions `quit`, and there is no `/docs/cli/quit` page in slugs.txt. `shelbi quit` (with `--all`) is a real, user-facing command.
  - Evidence: `git show origin/main:site/content/docs/cli/open.mdx` (headings: Arguments, Flags, Examples, See also; no "quit"); slugs.txt has no `/docs/cli/quit`; `$SCR/help/quit.txt`.
  - Fix: Either add a `site/content/docs/cli/quit.mdx` page and link `/docs/cli/quit`, or unlink it: "`shelbi quit` ends a project's sessions (and `shelbi quit --all` stops the whole hub)".
- [severity: outdated] "two terminal windows today, and later the desktop app attaching to a running CLI session or a client attaching to a remote one" (L12-L14)
  - Reality: The desktop app already registers as an attached client of kind `desktop`, with no project, so `--list` shows it (PROJECT `-`) and `shelbi detach` detaches it. Only the remote client is still in the future.
  - Evidence: desktop/src/platform/control.rs:173-179 (`ClientInfo { kind: ClientKind::Desktop, project: None, … }`); crates/shelbi-proto/src/control.rs:313-320 (`Tui`, `Desktop`, `Remote`).
  - Fix: "terminal windows and the desktop app today, and later a client attaching to a remote session."
- [severity: wrong] "`shelbi detach` always exits `0`, whether or not any client was attached" (L45-L46)
  - Reality: It exits 0 when no daemon is running, when no clients are attached, and when a named client isn't attached. That last case prints `No client '<id>' attached.`, which the page doesn't mention. If the daemon is running but its control socket can't be reached within 10s, or the list/detach round-trip fails, it exits 1 with `could not reach the hub daemon's control socket at …`, `listing attached clients: …`, or `detaching clients: …`.
  - Evidence: crates/shelbi-cli/src/commands/detach.rs:50-70 (report → messages, `Ok(())`), :153-170 and :176-195 (errors propagate via `?`/`anyhow!`).
  - Fix: "`shelbi detach` exits `0` whether or not any client was attached (with no running session it reports `No clients attached.`; an id that isn't attached reports `No client '<id>' attached.`). It exits non-zero only if it can't reach the running daemon's control socket."
- [severity: unclear] "`-p, --project <PROJECT>` | string | env / cwd lookup | Project to operate on." (L43)
  - Reality: `detach` ignores the project. It always targets every attached client of the one hub session, across all projects, or the named client. `main.rs` calls `commands::detach::run(list, client_id)` without passing `cli.project`.
  - Evidence: crates/shelbi-cli/src/main.rs:520.
  - Fix: Remove the `-p` row (it's a global flag), or change its description to: "Accepted as a global flag but ignored: detach is hub-wide, not per-project."

### site/content/docs/cli/events.mdx
- [severity: wrong] "Every subcommand accepts the global `-p / --project <PROJECT>` flag." (L35)
  - Reality: `events tail` defines its own long-only `--project <PROJECT>` filter, which shadows the global. `-p` is rejected (`error: unexpected argument '-p' found`). `$SHELBI_PROJECT` and the cwd lookup are not applied: `events::run(cmd)` never sees `cli.project`. Without `--project` the tail is hub-global, covering every project.
  - Evidence: crates/shelbi-cli/src/commands/events.rs:32-37 (`#[arg(long)] project: Option<String>`), :49-58; crates/shelbi-cli/src/main.rs:514 (`commands::events::run(cmd)`, no project). Real: `shelbi events tail -p shelbi -n 1` → `error: unexpected argument '-p' found`.
  - Fix: Replace L35 with: "`events tail` is hub-global by default. Pass `--project <name>` (long form only; `-p` isn't accepted here, and `$SHELBI_PROJECT` is ignored) to scope it to one project."
- [severity: missing] (not documented) — `--project <PROJECT>` row in the `tail` flag table (L48-53)
  - Reality: `--project <PROJECT>` keeps only lines carrying `project=<name>`. Lines without a `project=` token are dropped, even legacy or hub-level ones such as `merge task=…` and `ssh reverse-forward …`. The `-n` window and the `--since` cutoff count the project's own lines. It is non-consuming: it never claims from the durable delivery queue, so it can run alongside `shelbi orchestrator events next`.
  - Evidence: crates/shelbi-cli/src/commands/events.rs:61-72 (`line_in_project`), :94-113; `shelbi events tail --help`.
  - Fix: Add row: "| `--project <PROJECT>` | string | — (all projects) | Only show lines with `project=<PROJECT>`. Non-consuming: safe to run alongside `shelbi orchestrator events next`. |"
- [severity: outdated] "The orchestrator follows it as its trigger stream." (L12-13) and "Follow the log live, the same thing the orchestrator does on bootstrap: `shelbi events tail --follow`" (L67-71); "for orchestrator bootstrap (`--follow` in the background and watch with `Monitor`)" (L44-45)
  - Reality: The orchestrator's source of truth is now the consuming durable queue `shelbi orchestrator events next --follow --max-lifetime 2s` plus `… events ack <id>`. `events tail` is only a passive, non-consuming latency accelerator, and the orchestrator starts it as `shelbi events tail --follow --project <project>`.
  - Evidence: crates/shelbi-orchestrator/src/lib.rs:694-722 (bootstrap prompt: "start `shelbi events tail --follow --project {project_name}` … That tail is a *non-consuming* latency accelerator … Your source of truth is the self-driven drain … `shelbi orchestrator events next`").
  - Fix: L12-13 → "The orchestrator mirrors it with `events tail --follow --project <name>` as a low-latency wake-up. Its authoritative feed is the durable `shelbi orchestrator events next` queue." Change the example to `shelbi events tail --follow --project myapp`.
- [severity: unclear] "the append-only log of every column transition and workspace state change across the hub" (L11-12) and the three "most common line shapes" (L18-22)
  - Reality: The log also carries `dispatch …`, `merge …`, `ssh reverse-forward …`, `config-upgrade` and message-ack lines. Task transitions can carry a trailing `owner=<who>`. Heartbeats carry extra fields: real `<ts> project=shelbi heartbeat zen=on zen_eligible=0 idle_workspaces=4`, sometimes followed by Zen guidance text. Real task-transition status ids appear as e.g. `todo -> in_progress`.
  - Evidence: Real `shelbi events tail -n 12`:
    ```
    2026-10-30T02:18:02.528409+00:00 project=shelbi heartbeat zen=on zen_eligible=0 idle_workspaces=5 — Zen: auto-promote …
    2026-10-30T02:18:32.558516+00:00 dispatch project=shelbi task=docs-audit-cli workspace=hotel status=message-channel detail=mode=hooks_runner=claude
    2026-10-30T02:18:35.263572+00:00 project=shelbi task=docs-audit-cli workflow=cstore todo -> in_progress reason=orchestrator:auto-dispatch_workspace=hotel_agent=developer from_category=ready to_category=active owner=me
    2026-10-30T02:21:58.883837+00:00 merge task=dt-cli-path-readonly workspace=charlie base=main status=ok detail=pr:2013:…
    ```
  - Fix: L11-12 → "…the append-only log of task transitions, workspace state changes, heartbeats, and hub operations (dispatch, merge, forward health) across the hub." Change the heartbeat shape to `<ts> project=<name> heartbeat zen=<on|off> zen_eligible=<n> idle_workspaces=<n>`.
- [severity: unclear] "`--format <FORMAT>` … a normalized JSON envelope with `kind`, `project`, `timestamp`, and the original `line`." (L53)
  - Reality: The envelope also carries `version` (1) and `transport` (`"shelbi.events"`). `kind` values seen include `task` and `heartbeat`.
  - Evidence: Real `shelbi events tail --project shelbi -n 2 --format envelope` → `{"version":1,"transport":"shelbi.events","kind":"task","project":"shelbi","timestamp":"2026-10-30T02:22:02.475530+00:00","line":"…"}`; events.rs:186-197 (`shelbi_state::EventEnvelope::from_log_line`).
  - Fix: "…a normalized JSON envelope (`version`, `transport`, `kind`, `project`, `timestamp`, and the original `line`)."
- [severity: unclear] "`--since <SINCE>` | duration (e.g. `10m`, `2h`, `1d`)" (L51)
  - Reality: Units are `s`/`m`/`h`/`d`, and a bare integer means seconds. Lines whose timestamp doesn't parse are kept. A bad unit errors with `unknown duration unit `x` (use s/m/h/d)`.
  - Evidence: crates/shelbi-cli/src/commands/events.rs:200-237. Real: `shelbi events tail --since 5x` → `Error: unknown duration unit `x` (use s/m/h/d)`.
  - Fix: "duration: `<n>s`, `<n>m`, `<n>h`, `<n>d`, or bare seconds".

### site/content/docs/cli/init.mdx
- [severity: missing] "It uses the current directory by default, detects the Git root, branch, origin, and runner" (L18-20)
  - Reality: if the directory isn't a Git repository, `init -y` runs `git init -b main` there without asking (the plan approves it up front), then creates the project with default branch `main`. Interactive setup asks first.
  - Evidence: crates/shelbi-cli/src/wizard.rs:312-331 (`initialize_git = !git.inside_git`), 374-385, 966-969 (`git_init_root`), 404-412 (`already_approved` skips the confirm); sandbox: `shelbi init -y --root <non-git dir> --runner claude` → project created, `git -C <dir> branch --show-current` = `main`.
  - Fix: Add after L20: "If the directory isn't a Git repository yet, `-y` initializes one with `git init -b main` first."
- [severity: missing] (not documented) (after L20)
  - Reality: every creation path (`-y`, the interactive card, and legacy `init`) also installs a Shelbi `pre-commit` hook in `<repo>/.git/hooks/`. It blocks commits to a protected branch only inside Shelbi-managed agent panes. It prints a disclosure on first install, leaves a user-authored hook alone (with a warning), and is removed with `shelbi guard uninstall`.
  - Evidence: crates/shelbi-cli/src/wizard.rs:416-421 (`guard::install_at_init`); crates/shelbi-cli/src/commands/guard.rs:70-97; sandbox `init -y` stderr: "shelbi: installed a git pre-commit hook at …/.git/hooks/pre-commit. … Remove it anytime with `shelbi guard uninstall`."
  - Fix: Add: "Setup also installs a `pre-commit` hook that blocks commits to the default branch from inside Shelbi's agent panes only (your own shell is unaffected). An existing hook of your own is left untouched. Remove it with `shelbi guard uninstall`."
- [severity: missing] (not documented) (after L20)
  - Reality: if the root already belongs to a registered project, `init -y` prints `✓ Project <name> is already configured.` and exits 0 without changing anything.
  - Evidence: crates/shelbi-cli/src/commands/init.rs:258-264; sandbox re-run → "✓ Project repo-fmqd is already configured." rc=0.
  - Fix: Add: "Running `init -y` again in a directory that already belongs to a project prints `✓ Project <name> is already configured.` and changes nothing."
- [severity: unclear] "Detected-plan overrides require `-y`." (L98) together with the "Detected-plan options" table that lists `--project` and `--root` (L89-90)
  - Reality: only `--runner`, `--default-branch`, `--github-url`, `--orchestrator-runner`, `--issue-tracker` and `--github-repo` require `-y`. Without it they fail with ``detected-plan overrides require `shelbi init -y` ...``. `--project` and `--root` also work in the legacy flow, where `--root` is required when stdin isn't a TTY.
  - Evidence: crates/shelbi-cli/src/commands/init.rs:188-207; `shelbi init --runner claude </dev/null` → that error; `shelbi init </dev/null` → "Error: shelbi: not running interactively — pass --root <path> to set the project root."
  - Fix: L98: "`--runner`, `--default-branch`, `--github-url`, `--orchestrator-runner`, `--issue-tracker` and `--github-repo` require `-y`. `--project` and `--root` work on both paths." Add a `--root` row to the L113 table: "Project root. Skips the "Project root?" prompt; required when stdin is not a TTY."
- [severity: unclear] "| `--root <ROOT>` | path | current directory with `-y` | Repository Shelbi will manage. |" (L90)
  - Reality: only `--root` placed after `init` names the project root. Before the subcommand (`shelbi --root PATH init`) it is the global flag that sets the Shelbi state root (`~/.shelbi`). A relative path is resolved against the current directory and must exist.
  - Evidence: help/root.txt and help/init.txt global `--root` text ("With `init`, place this before the subcommand; `init --root PATH` names the project root instead"); crates/shelbi-cli/src/main.rs test `init_root_scope_distinguishes_global_state_from_project_root`; crates/shelbi-cli/src/commands/init.rs:353-368.
  - Fix: Append to the description: "Put it after `init`. `shelbi --root PATH init` sets the Shelbi state directory instead."
- [severity: unclear] "| `--project <PROJECT>` | string | project-root basename | Override the generated project name. |" (L89)
  - Reality: the name (given or derived from the basename) is normalized into the project-id charset (lowercase, other characters → `-`) and the change is announced, e.g. `Using project name 'my_thing' (normalized from 'My_Thing')`. A basename `repo.FMqD` became project `repo-fmqd`.
  - Evidence: sandbox `shelbi init -y --project My_Thing` output; crates/shelbi-cli/src/project_root.rs `normalize_project_name_announced`.
  - Fix: "Override the generated project name. Shelbi lowercases it and replaces characters a project id can't hold with `-`, and tells you when it does."
- [severity: unclear] "**File system** … This is the default." (L41-43) vs "It is pre-selected … when the origin remote is a GitHub repo" (L33-34)
  - Reality: interactive setup pre-selects GitHub when the origin is on GitHub and File system otherwise. File system is the unconditional default only for `-y` and other non-interactive callers.
  - Evidence: crates/shelbi-cli/src/issue_tracker_setup.rs:283-286; crates/shelbi-cli/src/wizard.rs:985-988.
  - Fix: Replace "This is the default." with "It's pre-selected when the origin isn't on GitHub, and it's what `-y` uses unless you pass `--issue-tracker github`."
- [severity: unclear] "use [`shelbi issue-store migrate`](/docs/cli/issue)" (L48)
  - Reality: the link resolves, but the section is on the issue page under `## issue-store migrate`.
  - Evidence: site/content/docs/cli/issue.mdx:335 at origin/main.
  - Fix: Link to `/docs/cli/issue#issue-store-migrate`.

### site/content/docs/cli/issue.mdx
- [severity: wrong] "Matching is case- and punctuation-insensitive (`InProgress` matches `in_progress`)" (L149-150)
  - Reality: Only a fixed alias table for the six core statuses is normalized (case-insensitive): `todo|to_do|to-do`, `in_progress|in-progress|inprogress|wip`, `review|ready_for_review|ready-for-review`, `done|complete|completed`, `canceled|cancelled`, `backlog`. Any other id, including every custom status, must match a declared status id exactly (surrounding whitespace is trimmed). The error lists all statuses the workflow declares (`valid: ...`), not only "reachable" ones.
  - Evidence: crates/shelbi-core/src/model.rs:2121-2131 (`normalize_status_id`); crates/shelbi-orchestrator/src/transition_move.rs:537-559 (`resolve_move_target`, error "`{to}` is not a status in workflow `{name}` (valid: …)")
  - Fix: Replace L146-151 with: "The destination is validated against the issue's workflow. A status the workflow doesn't declare errors with the list of statuses it does declare, and the issue stays put. The core statuses accept friendly spellings in any case (`InProgress`, `wip`, `to-do`, `cancelled`, `completed`, …); a custom status id must match exactly. An issue pinned to a workflow with no YAML on disk falls back to the built-in `default` (with a warning), so projects keep moving while the workflows directory is empty."
- [severity: missing] "(not documented)" — `issue move --skip-transition-actions` and the fact that `move` runs transition actions (L137-156)
  - Reality: `move` fires the workflow transition's actions for the edge it crosses (`merge`, `push_branch`, `open_pr`, `close_pr`, `delete_branch`, …). A `merge` edge is gated: the merge runs before the card moves, and a failed merge aborts the move. A move into `in_progress` cuts the issue's branch on the hub first. `--skip-transition-actions` advances the card without running any of these, stamped `actions=skipped` in the event log.
  - Evidence: help/issue_move.txt (`--skip-transition-actions`); crates/shelbi-orchestrator/src/transition_move.rs:310-380
  - Fix: Add a paragraph after L151: "Moving across an edge that declares [transition actions](/docs/configuration/workflow#actions) runs them. A `merge` action is gated, so the branch is merged before the card moves and a failed merge leaves the issue where it was. A move into `in_progress` also cuts the issue's branch." Add table row: "| `--skip-transition-actions` | flag | off | Recovery escape hatch: advance the card without running the destination transition's actions (`merge`, `push_branch`, `open_pr`, …). Use only when the git work already happened out of band, such as a PR merged by hand. The move is stamped `actions=skipped` in the event log. |"
- [severity: missing] "(not documented)" — `issue assign --force` (L168-170)
  - Reality: `assign` refuses a `review`-tagged workspace unless `--force`, and refuses a workspace already running a different `in_progress` issue.
  - Evidence: help/issue_assign.txt; crates/shelbi-cli/src/commands/issue.rs:438-452 (`ensure_workspace_dispatchable`)
  - Fix: Add row "| `--force` | flag | off | Assign onto a `review`-tagged workspace anyway (refused by default; review slots load handed-off branches through the review queue). The override is recorded in `~/.shelbi/events.log`. |" and the sentence "A workspace already running a different `in_progress` issue is refused."
- [severity: missing] "(not documented)" — `issue start --force` and `--take` (L197-201)
  - Reality: `--force` overrides the review-slot guard, as on `assign`. `--take` claims the issue on a remote issue tracker before starting: on a remote tracker Shelbi only starts issues assigned to you. The orchestrator never passes `--take`.
  - Evidence: help/issue_start.txt
  - Fix: Add rows "| `--force` | flag | off | Launch on a `review`-tagged workspace anyway (refused by default). Recorded in `~/.shelbi/events.log`. |" and "| `--take` | flag | off | On a remote issue tracker (e.g. GitHub), assign the issue to you on the tracker before starting. Needed for an unassigned issue or one you're taking over; Shelbi only starts issues assigned to you there. |"
- [severity: missing] "(not documented)" — `issue resume --take` (L235-238)
  - Reality: Same ownership gate as `start`; `--take` claims the issue on a remote tracker before resuming.
  - Evidence: help/issue_resume.txt; crates/shelbi-cli/src/commands/issue.rs:1144-1154
  - Fix: Add row "| `--take` | flag | off | Claim the issue on a remote issue tracker before resuming it (see `start --take`). |"
- [severity: missing] "(not documented)" — `issue add` body from stdin (L90)
  - Reality: The body can be piped on stdin (`shelbi issue add "Title" <<EOF … EOF`); passing both `-d` and stdin is an error.
  - Evidence: help/issue_add.txt (`-d`); crates/shelbi-cli/src/commands/issue.rs:494-521
  - Fix: Change the `-d` row description to: "Issue body. May instead be piped on stdin (`shelbi issue add "Title" <<EOF … EOF`); passing both is an error. When neither is given, the body defaults to the title; use `shelbi issue edit` to fill it in later."
- [severity: wrong] "`shelbi issue rm` deletes the issue's markdown file outright. There's no trash and no undo." (L318-319)
  - Reality: True on the `file_system` backend. On the `github` backend GitHub can't hard-delete an issue, so `rm` closes it as `not_planned` and clears the local assignment. On both backends the column's priorities are then renumbered. The page's intro callout (L26-34) says every subcommand drives either backend, so the rm section reads as file-only.
  - Evidence: crates/shelbi-state/src/github_store.rs:1591-1609; crates/shelbi-cli/src/commands/issue.rs:1426-1434
  - Fix: Replace L314 "Delete an issue file." with "Delete an issue. On the `file_system` backend this removes its markdown file; on a `github` board it closes the GitHub issue as *not planned* (GitHub can't delete issues)." Change the callout to "On the file system backend `shelbi issue rm` deletes the markdown file outright …" and suggest "move it to `canceled` instead" in place of "move it back to `backlog`" (the move help names `canceled` as a valid destination).
- [severity: unclear] "`--up` … `--set <N>`" (L252-256)
  - Reality: The five prio flags are mutually exclusive (clap `conflicts_with_all`).
  - Evidence: crates/shelbi-cli/src/commands/issue.rs:233-245
  - Fix: Add after the table: "Pass exactly one of these flags."

### site/content/docs/cli/machine.mdx
- [severity: missing] (not documented) — the `check <NAME>` subcommand (Subcommands table L21-24; needs its own `### check` section after L41)
  - Reality: `shelbi machine check <NAME>` checks that a machine is ready for this project: SSH connects, a compatible `shelbi` is present, Git is usable, and the machine's `work_dir` is a writable checkout root. On the remote it only reads; it installs and creates nothing. On success it prints `<name>: ready. <summary>` and records the verified binary in the hub's `~/.shelbi/machines.json`. Otherwise it prints one `<name>: <issue>` line per problem, adds "run `shelbi machine setup <name>` to install a compatible CLI." when an install would fix it, and exits non-zero with `<name>: not ready`. For a local machine it prints "<name>: local machine — nothing to check over SSH."
  - Evidence: crates/shelbi-cli/src/commands/machine.rs:30-37 (clap variant + doc), :175-206 (`check`); `help/machine_check.txt`; top-level help lists `setup`, `check`, `status`.
  - Fix: Add a table row "| `check <NAME>` | Verify a machine is ready for this project (SSH, compatible `shelbi`, Git, writable `work_dir`). Read-only on the machine; records the verified binary when everything passes. |" and a `### check` section with usage `shelbi machine check <NAME>`. Update the frontmatter `summary` (L4) and intro (L13-14) to "find, check, or install".
- [severity: missing] (not documented) — what "compatible" means (L13, L24, L28-31)
  - Reality: A remote binary is compatible when its `major.minor` equals the hub's; patch may differ. Older is "too old" and gets the managed install alongside. Newer is "too new". This is looser than the hub's own daemon handshake, which requires an exact match.
  - Evidence: crates/shelbi-orchestrator/src/machine.rs:250-266 (`classify_compat`).
  - Fix: Add after L14: "A remote binary is compatible when its major.minor version matches the hub's. Patch versions may differ."
- [severity: missing] (not documented) — `status` output, probe states and side effects (L42-50, L69-73)
  - Reality: `status` prints `hub version: <v>`, a blank line, then a table `MACHINE  REACHABLE  PATH  VERSION  COMPATIBLE`. REACHABLE is `local`, `yes`, `unreachable`, `auth-denied` or `host-key`. For an unreachable machine the last recorded path is shown with ` (last known)`. COMPATIBLE is `yes`, `no`, `not installed` or `-`. A local machine shows the hub's own executable path. Each remote is probed live over SSH, and a reachable probe refreshes `~/.shelbi/machines.json`.
  - Evidence: crates/shelbi-cli/src/commands/machine.rs:211-290 (`status`, `status_row`, `unreachable_row`); crates/shelbi-state/src/machine_state.rs:71-72 (`machines.json`). Real `shelbi machine status`:
    ```
    hub version: 0.11.0

    MACHINE  REACHABLE  PATH                                      VERSION  COMPATIBLE
    hub      local      /Users/jlong/Workspaces/tilde/bin/shelbi  0.11.0   yes
    devbox   yes        /home/jlong/.shelbi/bin/shelbi            0.11.0   yes
    ```
  - Fix: Add that sample under the `status` example and list the REACHABLE/COMPATIBLE values.
- [severity: missing] (not documented) — `setup` details (L26-32)
  - Reality: A `kind: local` machine is a no-op: "<name>: local machine — it runs the hub's own `shelbi` binary; no remote setup needed." The PATH probe runs through `$SHELL -l -i -c 'command -v shelbi'`. Install detects the remote's OS/arch, fetches the release matching the hub's version and verifies its checksum. If the machine has no outbound network, the hub fetches the release and copies it over SSH. Output is `<name>: already set up — compatible shelbi <v> at <path> (on PATH|shelbi-managed).` or `<name>: installed shelbi <v> to <path>[ (fetched on the hub and copied over SSH…)]`. The resolved path is recorded in the hub's `~/.shelbi/machines.json`. An unknown name errors: "no machine named `<name>` in project `<project>`".
  - Evidence: crates/shelbi-cli/src/commands/machine.rs:46-70, :77-141; crates/shelbi-orchestrator/src/machine.rs:9-17.
  - Fix: Append to L28-32: "Local machines need no setup. Installs are checksum-verified, and if the machine can't reach the internet the hub downloads the release and copies it over SSH. The resolved path is recorded in the hub's `~/.shelbi/machines.json`."

### site/content/docs/cli/message.mdx
- [severity: missing] "reports the current state (`delivered`, `queued`, or `unconfirmed`)" (L37-39) and "Prints `delivered`, `queued`, or `unconfirmed`" (L90-91)
  - Reality: there's a fourth outcome. An id with no `push=ok` line in the events log fails with ``Error: unknown message id `<id>` — no `push=ok` for it in the events log (never pushed, or the log has rotated the record out)``. Every non-delivered state is printed as an error on stderr with exit 1. Only `delivered` goes to stdout with exit 0.
  - Evidence: crates/shelbi-cli/src/commands/message.rs:132-152; `shelbi message status m-1785764991921-86377` → the unknown-id error, rc=1.
  - Fix: L90-92: "Prints `delivered` and exits 0 when the worker has acked. Otherwise it exits non-zero with `queued` (no ack yet), `unconfirmed` (the daemon's ack window passed with no ack; a late ack can still land), or `unknown message id` (never pushed, or rotated out of `events.log`)."
- [severity: unclear] "A message to a task that is already `done`, or to a workspace with no live reader, is reported as undeliverable rather than silently queued." (L41-42)
  - Reality: in both cases the record is still appended to the log (kept for archival/replay, and for no-reader it will be drained if that task's session restarts), and the command exits non-zero with an error naming the log path. Other hard errors, which write nothing, are: the issue isn't found, the task is unassigned, the assigned workspace or machine is no longer declared, and the worktree doesn't exist yet.
  - Evidence: crates/shelbi-cli/src/commands/message.rs:173-215, 272-299.
  - Fix: "A message to a task that is already `done`, or whose workspace has no live reader right now, is still written to the log but exits non-zero as undeliverable. A task that is unassigned, or whose worktree doesn't exist yet, is an error and nothing is written."
- [severity: missing] (not documented) (Flags section, after L83)
  - Reality: before writing, a push checks that the hub daemon runs the same version as the CLI. On a mismatch it offers to restart the daemon (interactive), restarts it without asking under the global `-y/--yes` (`SHELBI_YES=1`), or fails with ``hub daemon is X, CLI is Y — run `shelbi daemon restart` ...`` when non-interactive. `message status` does not run this check.
  - Evidence: crates/shelbi-cli/src/commands/message.rs:167-169; crates/shelbi-cli/src/commands/hub_version.rs:40-73, 99-117.
  - Fix: Add a note: "A push first checks that the hub daemon matches this CLI's version. On a mismatch it asks to restart the daemon; with `-y` it restarts without asking; non-interactively it stops and tells you to run `shelbi daemon restart`."
- [severity: unclear] "Each call appends one durable JSON record." (L18-19)
  - Reality: the record is one JSON line, `{"msg_id","ts","kind","in_response_to"?,"body"}`. `msg_id` looks like `m-<unix-millis>-<pid>` and `ts` is RFC 3339 UTC to the second. The plain push prints `queued <msg-id> → <task> (<kind>) — durable and being tailed by the worker, but NOT yet confirmed delivered…`. The page never says where the `msg-id` that `status` takes comes from.
  - Evidence: crates/shelbi-cli/src/commands/message.rs:81-91, 217-234, 309-316.
  - Fix: Add: "Each call appends one JSON line (`msg_id`, `ts`, `kind`, optional `in_response_to`, `body`) and prints `queued <msg-id> → <task> (<kind>)…`. Pass that `msg-id` (`m-<millis>-<pid>`) to `shelbi message status`."

### site/content/docs/cli/open.mdx
- [severity: missing] "For an idle workspace it starts a plain interactive login shell in the worktree, so there is something to focus." (L13-14)
  - Reality: The shell session is marked user-occupied, so dispatch skips the slot while it's open (`shelbi workspace list` shows `occupied (user shell)`). It is not a managed agent session: exiting the shell frees the slot and supervision never relaunches it. If any session is already live for the slot, `open` does nothing. The shell is `$SHELL -l` (fallback `/bin/sh`), started in the worktree or, when the worktree is missing (e.g. after handoff), the machine's `work_dir`.
  - Evidence: crates/shelbi-orchestrator/src/workspace.rs:2023-2075 (`open_user_shell`, `mark_user_shell`); crates/shelbi-cli/src/commands/workspace.rs:286-304 (`occupied (user shell)`)
  - Fix: Append: "That shell marks the workspace as occupied by you, so the orchestrator won't dispatch to it while it's open (`shelbi workspace list` shows `occupied (user shell)`). Exit the shell to free the slot. If the worktree is missing, the shell starts in the machine's `work_dir`."
- [severity: unclear] "For a workspace mid-task it is a no-op" (L12)
  - Reality: The check is "any open board issue has `assigned_to` = this workspace", which includes issues assigned but still in `todo` or serving in `review`, not only in-progress ones.
  - Evidence: crates/shelbi-cli/src/commands/open.rs:37-55 (`workspace_has_assigned_task`)
  - Fix: "For a workspace with an issue assigned to it, it is a no-op: dispatch owns that agent's session."

### site/content/docs/cli/project.mdx
- [severity: wrong] "and `-y, --yes` (accept the detected plan without prompts — meaningful for `add`) flags." (L18-19)
  - Reality: `project add` ignores `-y`. It always shows the interactive plan card: `ProjectCmd::Add` calls `wizard::run_one_project_and_launch()` with no yes argument, and only `init` receives `cli.yes`. The global `-y` just sets `SHELBI_YES=1` for the hub-version/daemon-restart offer. For a prompt-free setup the command is `shelbi init -y`. Also, `-y` is rejected after `migrate-to-in-repo`, because that subcommand defines its own long-only `--yes` that shadows the global: `shelbi project migrate-to-in-repo -y` → `error: unexpected argument '-y' found`. So "Every subcommand accepts … `-y, --yes`" is false for `migrate-to-in-repo`.
  - Evidence: crates/shelbi-cli/src/commands/project.rs:71-73, :62-67 (`#[arg(long)] yes: bool`, no short); crates/shelbi-cli/src/main.rs:461-467 (global yes → `SHELBI_YES` env), :531 (`init::run(args, cli.yes)` is the only consumer); crates/shelbi-cli/src/wizard.rs:272-278 (`setup_one_project` always uses `RealSetupUi`). Sandbox: `SHELBI_ROOT=$(mktemp -d) shelbi project migrate-to-in-repo -y --dry-run -p nope` → `error: unexpected argument '-y' found`.
  - Fix: Replace L18-20 with: "and `-y, --yes` flags. `project add` always shows the setup card; for a prompt-free setup use `shelbi init -y`. `migrate-to-in-repo` has its own long-only `--yes` (no `-y`) and renders the project flag's value name as `<NAME>`."
- [severity: missing] "Press Enter to create the project and launch its TUI, or press `c` to customize the prefilled values first." (L37-38)
  - Reality: The card's key legend is `Enter launch    c customize    q quit`. `q`/`Q` or Esc quits without writing anything, as does Ctrl-C. If the cwd is not inside a Git repo, the plan initializes one (`initialize_git = !git.inside_git`). On a clone that already carries committed in-repo config, `add` defers to the `init --pick-up` contract instead of creating a parallel global project. The project is written to `~/.shelbi/projects/<name>.yaml` (global mode).
  - Evidence: crates/shelbi-cli/src/wizard.rs:1184 (legend), :1223-1230 (key map), :280-288 (`preserve_in_repo_pickup_contract`), :318-322 (`initialize_git`); crates/shelbi-cli/src/commands/project.rs:20-23 (doc comment: "Writes `~/.shelbi/projects/<name>.yaml` after confirmation").
  - Fix: "…press `c` to customize the prefilled values first, or `q`/Esc to quit without writing anything. The new project is registered at `~/.shelbi/projects/<name>.yaml` (global mode). Run `migrate-to-in-repo` afterwards to commit it. On a clone that already has `.shelbi/project.yaml`, use `shelbi init --pick-up` instead."
- [severity: missing] (not documented) — `migrate-to-in-repo` output and the non-TTY `.gitignore` behavior (L55-58, L73)
  - Reality: The command prints a header (`Migrating project `<name>` to in-repo mode[ (dry-run)]` plus `repo:`, `state dir:` and `in-repo dir:` lines), any `⚠` warnings, then a `Plan:` list of actions. A real run ends with `✓ applied N actions` and the shared/local config paths. A dry run ends with `(dry-run — no files were written)` and the snippet. With no TTY and no `--yes`, the snippet is printed but not appended ("Re-run with `--yes` to auto-append"). An already-migrated project prints "Nothing to do — project `<name>` is already in in-repo mode." and still offers the snippet.
  - Evidence: crates/shelbi-cli/src/commands/project.rs:88-136, :173-200 (`maybe_offer_gitignore`: `is_tty` check).
  - Fix: Add under the flags table: "Without a TTY and without `--yes`, the `.gitignore` snippet is only printed. Re-run with `--yes` to append it."

### site/content/docs/cli/question.mdx
- [severity: missing] "Each file is markdown with frontmatter (`id`, `task`, `workspace`, `kind`, `asked_at`, and `answered_at` once answered)" (L20-L22)
  - Reality: The frontmatter also carries `project`, an optional `context` (the excerpt the worker sent with the question), and `answer_msg_id` once answered. `kind` is always `clarification`.
  - Evidence: crates/shelbi-state/src/questions.rs:21-31 (sample frontmatter with `project:`), QuestionMeta fields `project`, `answer_msg_id`, `context` (questions.rs ~L50-64).
  - Fix: "Each file is markdown with frontmatter (`id`, `task`, `project`, `workspace`, `kind: clarification`, `asked_at`, an optional `context`, and `answered_at` plus `answer_msg_id` once answered) followed by the question text, untouched."
- [severity: missing] "(not documented)": hub-level fallback store (after L18)
  - Reality: When the hub can't resolve the asking worker's project, the question is stored under `<root>/questions/<task-id>/<question-id>.md` (the hub root) instead of being dropped. `list` and `show` include these alongside the project's questions.
  - Evidence: questions.rs:16-17, :91-96 (`questions_root(None)`); crates/shelbi-cli/src/commands/question.rs `all_questions` (merges `list_questions(Some(project))` and `list_questions(None)`).
  - Fix: Add after L18: "If the hub can't tell which project the worker belongs to, it stores the question under `~/.shelbi/questions/<task-id>/` instead. `shelbi question` lists those too."
- [severity: missing] `show` options (L48-L53)
  - Reality: `show` accepts `--json`, which prints the question as a JSON object, and `--task <TASK>`. Both appear in the usage line, but unlike `list` there is no options table. The plain output prints `question <id> (task <task>)`, then `workspace:`, `asked:`, `answered: <ts> (<msg_id>)` or `status: open`, `file:`, an optional `context:`, the full text, and, while open, `answer with: shelbi message <task> reply --in-response-to <id> "<answer>"`.
  - Evidence: `shelbi question show --help`; question.rs `render_show`.
  - Fix: Add an options table under `## show`: "`--task <TASK>` | Task that asked it; needed only when two tasks used the same id." and "`--json` | Print the question as JSON."

### site/content/docs/cli/relay.mdx
- [severity: unclear] "| `-p, --project <PROJECT>` | string | env / cwd lookup | Project to operate on. |" (L30)
  - Reality: `relay` ignores the project. It serves every session under `<root>/sessions/` on the machine. `--root` does apply, via `sessions_dir()`. `relay::Args` has no fields of its own.
  - Evidence: crates/shelbi-cli/src/commands/relay.rs:20-34; crates/shelbi-state/src/lib.rs:364-366
  - Fix: Drop the `-p` row, or change its description to "Accepted (global flag) but ignored: the relay serves every session on the machine regardless of project."

### site/content/docs/cli/reload.mdx
- [severity: outdated] "It respawns the Shelbi-owned surfaces in place (the sidebar, the tasks / activity / machines views, and the orchestrator session) so a new binary takes effect…" (L11-15; frontmatter `summary` L4 too)
  - Reality: On the session runtime there are no panes to respawn. Reload runs four steps in order: (1) ask the live orchestrator for its handoff; (2) signal every attached client to re-exec (TUI) or prompt for relaunch (desktop); (3) restart the hub daemon on the current binary; (4) end and recreate the `<project>/orch` session, which ingests the handoff. Worker sessions are left running. The daemon restart is hub-wide, so it affects every open project's daemon-side pollers, not just this project.
  - Evidence: crates/shelbi-cli/src/commands/reload.rs:36-47, :95-106; crates/shelbi-cli/src/commands/reload_session.rs:1-27 (module doc), :57-73 (`run` order), :92-122 (`LiveReload`).
  - Fix: Replace L11-15 with: "`shelbi reload` is the 'I just installed a new binary, pick it up' command. It asks the orchestrator for a handoff, tells every attached TUI to re-exec (the desktop app prompts for relaunch), restarts the hub daemon on the current binary, and replaces the project's orchestrator session. Worker sessions keep running." Update `summary` to match.
- [severity: wrong] "Pass a target to reload just one part in place without bouncing the whole hub … A targeted reload respawns only the named pane and leaves every other pane and its state untouched." and the whole Targets table + examples (L30-56)
  - Reality: Targets are parsed and validated but then ignored. Any target other than `all` prints `note: targeted pane reloads no longer apply on the session runtime; reloading the whole project` to stderr and runs the full reload, including the daemon restart. `shelbi reload tasks` therefore does touch the orchestrator. `reload workspace <name>` does not respawn that worker with `--resume`. The name isn't even checked against the roster, so "A missing or unknown name is a clear error" only holds for a missing name.
  - Evidence: crates/shelbi-cli/src/commands/reload.rs:32-47 (`if !matches!(target, ReloadTarget::All) { eprintln!(…"reloading the whole project") } run_all(project_opt)`). crates/shelbi-orchestrator/src/lib.rs:159-198 (`ReloadTarget::parse` only checks syntax; `workspace` accepts any name).
  - Fix: Delete the Targets table, the three examples at L47-56, and L32-36. Replace them with: "`[TARGET] [NAME]` (`chat`, `tasks`, `activity`, `sidebar`, `workspace <name>`, `all`) are still accepted for compatibility. Any target other than `all` prints a note and runs the full reload." Keep L58-59: unknown targets still error with "unknown reload target `X`; valid targets: chat, tasks, activity, sidebar, workspace <name>, all".
- [severity: outdated] "The workspace panes are left alone: they re-shell into `shelbi` on every call and pick up the new binary on their next invocation, so there's nothing to restart there." (L23-25)
  - Reality: Worker sessions are left running untouched. Each is a long-lived session process pinned to the binary it started with, and it only picks up the new binary when that workspace's session is next relaunched. "Re-shell into shelbi on every call" describes tmux panes.
  - Evidence: crates/shelbi-cli/src/commands/reload_session.rs:22-25; crates/shelbi-session/src/daemon_watchdog.rs:22-26 ("A session is fixed at the version it started with").
  - Fix: "Worker sessions are left running, so a worker mid-task is never interrupted. Each keeps the binary it started with until that workspace is next relaunched."
- [severity: missing] (not documented) — reload's other repair work (around L61-75)
  - Reality: Before restarting, reload also: re-creates the root's standard subdirectories (`ensure_root_subdirs`). It migrates legacy projects to the `task`/`subtask` workflows, writing whichever of `task.yaml`/`subtask.yaml` is missing and rewriting `default_workflow:` from unset or `default` to `task` ("✓ set default_workflow: task (migrated from the legacy `default`)"). It writes a missing `workflows/statuses.yaml`. It runs the config validate-and-upgrade pass, auto-healing legacy forms (disclosed on events.log) and handing needs-judgment findings to the orchestrator. It self-heals all default agents, not just the orchestrator, and re-aligns the workspace-settings template with the shipped default.
  - Evidence: crates/shelbi-cli/src/commands/reload.rs:53-112, :168-178 (`print_default_workflow_migration`), :180-200 (`print_config_upgrade_summary`).
  - Fix: Add a "What else reload repairs" list naming these steps. Note that the workspace-settings template is always reset to the shipped default; point users who customize it at `workspace_settings_template`.
- [severity: missing] (not documented) — output
  - Reality: Prints a summary block:
    ```
    reload · <project> (session runtime)
      · handoff   <outcome>
      ✓ clients   signalled to re-exec
      ✓ daemon    restarted on the current binary
      ✓ orch      session replaced (workers left running)
    ```
    It fails with "restarting the hub daemon: <reason>" when this root's pid record doesn't verify.
  - Evidence: crates/shelbi-cli/src/commands/reload.rs:101-106; reload_session.rs:105-110.
  - Fix: Add the sample under Examples.
- [severity: unclear] "`-y, --yes` | flag | off | Skip confirmation prompts." (L83)
  - Reality: `reload` has no confirmation prompt, so `-y` has no effect. It is just the global flag.
  - Evidence: crates/shelbi-cli/src/commands/reload.rs (no prompt or `yes` parameter; `run(project_opt, target, name)`).
  - Fix: Drop the `-y` row, or mark it "accepted (global), no effect here".
- [severity: unclear] "Use it any time `scripts/install.sh` (or your own build) has produced a new binary" (L27-28) / "`./scripts/install.sh && shelbi reload`" (L90)
  - Reality: `scripts/install.sh` only exists in a Shelbi source checkout ("Install from source for development" on the install page). Most users install via Homebrew/APT, and the page never says to reload after `brew upgrade`.
  - Evidence: site/content/docs/guides/getting-started/install.mdx:140-170.
  - Fix: "Use it any time you upgrade Shelbi (`brew upgrade shelbi`, `apt upgrade`, or `./scripts/install.sh` from a source checkout)…", and link `/docs/guides/getting-started/install#install-from-source-for-development` for the script.

### site/content/docs/cli/remote.mdx
- [severity: wrong] "every registered project with its folder" (L33) and the example rows `shelbi  ~/Workspaces/shelbi` (L41-43)
  - Reality: the second column, and JSON `path`, is the project's `repo:` value with `$HOME` shown as `~`. It isn't necessarily a folder: when `repo:` is a clone URL it shows the URL, and it can be empty. Real output on this machine:
    ```
    Shelbi 0.11.0 as jlong (daemon running)
      shelbi             git@github.com:jlong/shelbi.git
      contextstore       ~/Workspaces/32pixels/ContextStore
      prbadge            
    ```
    The plain listing pads names to the longest one. With no projects it prints `No projects.`.
  - Evidence: crates/shelbi-app/src/launch/ssh.rs:98-115 (`path: display_path(&p.repo_path, …)`); crates/shelbi-state/src/hub_config.rs:101 (`list_projects`); `~/.shelbi/projects/shelbi.yaml` `repo: git@github.com:jlong/shelbi.git`; crates/shelbi-cli/src/commands/remote.rs:119-138.
  - Fix: Change L33 to "every registered project with its `repo:` location (usually its folder)". Either keep the example (it matches the unit test) and add "A project whose `repo:` is a clone URL shows the URL", or raise this as a code issue: a remote client gets a git URL or an empty string where it expects a folder.
- [severity: missing] "If Shelbi isn't running on the other machine, it asks before starting it (`--yes` starts it without asking)." (L25-26)
  - Reality: it asks only when stdin and stderr are terminals. Otherwise, without `--yes`, it stops with `Shelbi isn't running on <host>. Run again with --yes to start it.` `--yes` is the global `-y`, which sets `SHELBI_YES=1`, so that env var works too.
  - Evidence: crates/shelbi-cli/src/commands/remote.rs:65-73, 104-116; crates/shelbi-cli/src/main.rs:466; crates/shelbi-cli/src/commands/hub_version.rs:37.
  - Fix: Append: "When it can't ask (no terminal), it stops and tells you to rerun with `--yes`."
- [severity: missing] "`PROJECT` can be left out when the machine has just one project." (L16-17)
  - Reality: the errors aren't documented. With more than one project and no `PROJECT`: `Name a project on <host>. Its projects: a, b.` With an unknown name: `<host> has no Shelbi project named <name>. Its projects: ….` With none: `Shelbi on <host> has no projects.` `PROJECT` is matched against the project id (`name`), not the display label.
  - Evidence: crates/shelbi-cli/src/commands/remote.rs:82-102 and test `a_project_is_named_or_the_only_one`.
  - Fix: Add: "`PROJECT` is the project's id as `shelbi remote list` shows it on that machine. If you leave it out and the machine has several projects, the error lists them."

### site/content/docs/cli/send.mdx
- [severity: outdated] "`ID` is resolved against the project YAML's `workspaces:` block first, then falls back to the legacy spawn agent registry." (L28-29)
  - Reality: There is no fallback any more. `resolve_target` only matches a declared workspace; any other name errors with "unknown workspace `<id>` in project `<p>`" plus the list of declared workspaces (or "(no workspaces declared in project YAML)"). The `--help` text and the send.rs module comment still describe the legacy registry, but the code no longer has that path.
  - Evidence: crates/shelbi-cli/src/commands/send.rs:166-205 (`resolve_target`, `unknown_id_error`); test `unknown_workspace_errors_with_the_workspace_list`
  - Fix: "`ID` must name a workspace declared in the project YAML's `workspaces:` block. An unknown name errors and lists the declared workspaces."
- [severity: outdated] "Workspace name, resolved against `workspaces:` (then the legacy spawn agent registry) (required)." (L41)
  - Reality: Same as above.
  - Evidence: crates/shelbi-cli/src/commands/send.rs:166-191
  - Fix: "Workspace name from the project's `workspaces:` block (required)."
- [severity: wrong] "Codex and custom runners still receive the split text/Enter sequence, but Shelbi does not apply Claude's UI parser to them. Their result is explicit `status=unverified`" (L23-26)
  - Reality: Codex has its own submit verifier (`SubmitProfile::CodexUi`) and gets `submitted` / `queued` / `stuck` verdicts like Claude. Only generic (custom) runners get `status=unverified` (CLI prints "sent (unverified)"). The profile is picked from the project's baseline runner (`orchestrator.runner`), not the agent dispatched to the slot.
  - Evidence: crates/shelbi-orchestrator/src/submit.rs:27-30, 54-79; crates/shelbi-cli/src/commands/send.rs:180-185, test `workspace_send_uses_project_baseline_runner_for_submit_gating`
  - Fix: "Claude and Codex sessions are both verified this way. A custom runner still receives the split text/Enter sequence, but Shelbi has no parser for its screen, so the result is `status=unverified` (printed as `sent (unverified)`): delivered, with no runner-specific proof. Which parser applies follows the project's baseline runner (`orchestrator.runner`)."
- [severity: missing] "(not documented)" — live-session requirement and output (after L26)
  - Reality: If the workspace has no live session (declared but idle), `send` errors instead of starting one. On success it prints `✓ sent to <ws> (<session>)`, `✓ queued to …`, or `✓ sent (unverified) to …`. A transport failure is also logged as `status=stuck detail=transport_error`. Sends to the same workspace are serialized with dispatch/resume via the workspace lock. (Code bug to report separately: the no-session error suggests `shelbi workspace open <id>` and `shelbi task start`; the real commands are `shelbi open <id>` and `shelbi issue start`.)
  - Evidence: crates/shelbi-cli/src/commands/send.rs:44-59, 71-80, 99-109
  - Fix: Add: "The workspace must have a live session; an idle workspace is an error (open it with [`shelbi open`](/docs/cli/open) or dispatch an issue to it). On success `send` prints `sent`, `queued`, or `sent (unverified)`."

### site/content/docs/cli/session.mdx
- [severity: wrong] "List the sessions this project is running:" (L94)
  - Reality: `session ls` ignores `-p`/project. It lists every session directory under `<root>/sessions` on this machine, across all projects, including dead predecessors with their exit status.
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:94 (`SessionCmd::Ls => ls()`, no project), :128-131 (`discover` reads `shelbi_state::sessions_dir()`). Real `shelbi session ls` run from the shelbi repo lists `contextstore/orch`, `demo/ws/claude`, `moduloscreensaver/ws/review`, … alongside `shelbi/*`.
  - Fix: "List every session on this machine, all projects, live and dead:"
- [severity: missing] (not documented) — `ls` output shape and dead rows (should follow L35)
  - Reality: The output is a table with columns `NAME  ID  STATE  SIZE  TASK  LAUNCHED`, sorted by name then id. STATE is `live`, `dead`, `dead(code=N)` or `dead(sig=N)`. SIZE is `<cols>x<rows>` for live sessions and `-` for dead ones. LAUNCHED is `MM-DD HH:MM`. With no session dirs it prints `no sessions`.
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:135-200. Real output:
    ```
    NAME                         ID                STATE           SIZE    TASK                           LAUNCHED
    shelbi/orch                  a563d23f83372c93  live            156x54  -                              10-29 14:24
    shelbi/ws/alpha              b307bef5303a3402  live            120x40  site-home-client-switch        10-29 22:02
    shelbi/ws/alpha              b8a9ec12b4b02918  dead(code=143)  -       tui-toolbar-no-hover-text      10-29 21:50
    ```
  - Fix: Add that sample and one sentence after L35: "Dead sessions stay listed (with their exit code or signal) until pruned. A relaunch leaves its predecessors behind."
- [severity: unclear] "Most subcommands take a `<SESSION>` selector: a short-id, name, workspace, or unambiguous prefix." (L28-29)
  - Reality: Resolution runs in order: exact short-id, exact name, workspace (last `/` segment of the name), short-id prefix, then name substring. When a stage matches several sessions, `-p <project>` narrows to names under `<project>/`. If exactly one candidate is live, it wins. If none is live and they all share one name, the newest exited one wins. Otherwise the command errors with "`<sel>` matches several sessions; be more specific:" followed by the candidates. No match: "no session matching `<sel>` (try `shelbi session ls`)".
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:333-436 (`resolve_index`, `stage_match`). Real: `shelbi session snapshot nosuch` → `Error: no session matching `nosuch` (try `shelbi session ls`)`.
  - Fix: "…a short-id, full name, workspace (the last `/` segment of a name, e.g. `alpha`), short-id prefix, or name substring. When several sessions match, a live one beats its dead predecessors, and `-p <project>` breaks ties toward that project's sessions."
- [severity: unclear] "`-p, --project <PROJECT>` | string | env / cwd lookup | Project to operate on." (L90)
  - Reality: Under `session`, `-p` (or `$SHELBI_PROJECT`) only breaks selector ties for `kill`, `send` and `snapshot`. There is no cwd lookup here, and `ls` and `new` ignore it.
  - Evidence: crates/shelbi-cli/src/main.rs:549 (`session_cli::run(cli.project, cmd)` passes the raw flag/env value, not a resolved project); session_cli.rs:91-112.
  - Fix: "`-p, --project` | string | `$SHELBI_PROJECT` | Prefer this project's sessions when a selector is ambiguous (`kill`, `send`, `snapshot`)."
- [severity: missing] (not documented) — `snapshot` on a dead session; `send` on a dead session (L73-81, L63-71)
  - Reality: `snapshot` also works on a dead session and prints its final screen, which is the documented way to inspect an exited agent. `send` to a dead session fails with "session <id> (<name>) is dead; cannot send to it". `kill` on a dead session prints "session <id> (<name>) is already dead" and exits 0.
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:270-301 (kill/send dead handling), :305-315 (snapshot has no liveness check), :323-326 (attach error text points at `snapshot` for a dead session's final screen). Real: `shelbi session snapshot 0ccca2df` (a `dead(code=143)` session) printed its last screen.
  - Fix: Add under `snapshot`: "Works on dead sessions too: it prints the final screen." Add under `send`: "Fails if the session is dead."
- [severity: missing] (not documented) — `new` output and where sessions live (L37-51)
  - Reality: `new` prints `started session <id> (<name>)` and `  socket: <path>`. Session dirs (meta.json, socket, exit.json) live under `<root>/sessions/`.
  - Evidence: crates/shelbi-cli/src/commands/session_cli.rs:262-264; :128-131.
  - Fix: Add after the `new` table: "Prints the new session's id and socket path. Session state lives under `~/.shelbi/sessions/`."
- [severity: unclear] "Dump a stuck workspace's screen for inspection: `shelbi session snapshot alpha --history 200`" (L100-104)
  - Reality: The command works. But `alpha` also matches dead `…/ws/alpha` predecessors and other projects' `alpha` workspaces. It resolves to the single live match, so on a hub where two projects both run a live `alpha` it errors as ambiguous unless `-p <project>` is given.
  - Evidence: session_cli.rs:363-420.
  - Fix: Use `shelbi session snapshot alpha --history 200 -p myapp` and add "(`-p` disambiguates when several projects have an `alpha` workspace)."

### site/content/docs/cli/status.mdx
- [severity: missing] "five sections, `## Board`, `## Workspaces`, `## Zen`, `## Handoff`, and `## Daemon`, plus a sixth `## GitHub` section when the project's board is on the GitHub backend" (L14-L18); table row "five sections: Board, Workspaces, Zen, Handoff, Daemon" (L34)
  - Reality: `--full` can also print a `## Disk` section, between `## Daemon` and `## GitHub`, when free space on the hub machine's `work_dir` volume drops below the project's low-disk threshold. `## Daemon` can carry an extra `session: ⚠ …` line when the daemon lost its macOS GUI login session.
  - Evidence: crates/shelbi-cli/src/commands/status.rs:311-330 (`## Daemon`, session-lost line, conditional `## Disk`, conditional `## GitHub`), :190-215 (`disk_warning_line`).
  - Fix: L14-L18: "…`## Daemon`, plus a `## Disk` section when the hub volume is low on free space and a `## GitHub` section when the project's board is on the GitHub backend…". L34: "Emit the full sectioned bootstrap payload (Board, Workspaces, Zen, Handoff, Daemon, plus Disk/GitHub when applicable)."
- [severity: missing] "Bare `shelbi status` emits a concise, human-readable summary of where the board stands right now." (L12-L13), with no sample at L50-L54
  - Reality: The summary is one line per item: `board: N backlog / N ready / N active / N handoff / N done`, `workspaces: N idle, N in-progress`, `zen: <mode>`, `handoff: none|present (run \`shelbi status --handoff\` to consume)`, and `daemon: <ver>; cli: <ver> (match)`. Optional lines follow: `session: ⚠ …`, `github: board 30s · graphql N left`, `disk: LOW — …`, and one `review: <task> — no PR` per review-column task whose workflow opens PRs but has no open PR.
  - Evidence: status.rs:131-171. Real output: `board: 2 backlog / 0 ready / 4 active / 0 handoff / 0 done` / `workspaces: 5 idle, 4 in-progress` / `zen: on` / `handoff: none` / `daemon: 0.11.0; cli: 0.11.0 (match)` / `github: board 30s · graphql 3131 left`.
  - Fix: Add the real output as a commented sample under the first example (L52-L54), and add one sentence: "Extra `github:`, `disk:`, `session:`, and `review: <task> — no PR` lines appear only when they apply."
- [severity: missing] "(not documented)": `--handoff` refuses a mismatched daemon (L21-L24)
  - Reality: Because `--handoff` deletes a file, it requires the hub daemon's version to match the CLI's (`ensure_daemon_matches_for_mutation`). The non-destructive forms only warn on a mismatch.
  - Evidence: status.rs:79-85.
  - Fix: Append to L24: "Because it deletes the file, `--handoff` refuses to run against a hub daemon whose version doesn't match the CLI; the read-only forms only warn."

### site/content/docs/cli/wizard.mdx
- [severity: unclear] "`wizard` accepts the global `--root <PATH>`, `-p / --project <PROJECT>`, and `-y / --yes` flags." (L44-45)
  - Reality: they parse, but only `--root` does anything here. `-y` doesn't skip the card or the prompts, so for an unattended run use `shelbi init -y`. `-p` is ignored, and the project name comes from the checkout or from Customize.
  - Evidence: crates/shelbi-cli/src/main.rs:533 (`Cmd::Wizard => commands::wizard::run(false)`, no args passed); crates/shelbi-cli/src/commands/wizard.rs:22-36; crates/shelbi-cli/src/wizard.rs:272-278 (`RealSetupUi`, always interactive).
  - Fix: "`wizard` honors the global `--root <PATH>` (the Shelbi state directory). `-p` and `-y` have no effect here. For an unattended setup use `shelbi init -y`."
- [severity: unclear] "If any project is already registered, it completes without creating another one." (L40-42)
  - Reality: correct, but it exits 0 with no output at all and doesn't launch the dashboard, so the command looks like a no-op.
  - Evidence: crates/shelbi-cli/src/commands/wizard.rs:26-28.
  - Fix: "If any project is already registered, it exits quietly without creating another one or opening the dashboard."
- [severity: missing] (not documented) (after L21)
  - Reality: creating the project also installs a Shelbi `pre-commit` hook in the repo, and prints a disclosure the first time. The hook blocks commits to a protected branch only from Shelbi-managed agent panes and is removed with `shelbi guard uninstall`.
  - Evidence: crates/shelbi-cli/src/wizard.rs:416-421; crates/shelbi-cli/src/commands/guard.rs:70-97.
  - Fix: Add after L21: "Launch also installs a `pre-commit` hook that blocks commits to the default branch from Shelbi's agent panes only. Remove it with `shelbi guard uninstall`."
- [severity: unclear] "[`shelbi issue-store migrate`](/docs/cli/issue)" (L37)
  - Reality: the link resolves, but the section is `## issue-store migrate` on that page.
  - Evidence: site/content/docs/cli/issue.mdx:335 at origin/main.
  - Fix: Link to `/docs/cli/issue#issue-store-migrate`.

### site/content/docs/cli/workflow.mdx
- [severity: outdated] "Every project has a built-in `default` workflow: the canonical six-status flow (`Backlog → Todo → In Progress → Review → Done → Canceled`). These subcommands surface it alongside any custom workflows the project authors." (L13-15)
  - Reality: `shelbi init` now scaffolds two workflow files, `workflows/task.yaml` (the review-gated default track) and `workflows/subtask.yaml` (no PR/no review, squash-merges into the parent's `task/{{task}}` branch), and writes `default_workflow: task` into the project config. `default` is only a legacy fallback/alias: `load_workflow` aliases `default` → `task` when `default.yaml` is absent and `task.yaml` exists; the code-built `default` is shown only when the workflows dir has no YAML files at all. `shelbi reload` migrates legacy projects ("✓ set default_workflow: task (migrated from the legacy `default`)").
  - Evidence: crates/shelbi-state/src/workflows.rs:148-176 (`scaffold_project_workflow` at L158 writes task + subtask), :219-250 (`resolve_workflow_alias` at L236); crates/shelbi-core/src/model.rs:2453-2471 (`DEFAULT_WORKFLOW_NAME` "kept as the built-in fallback for legacy projects", `TASK_WORKFLOW_NAME`, `SUBTASK_WORKFLOW_NAME`); crates/shelbi-core/src/scaffold.rs:524 (`default_workflow: task`); crates/shelbi-cli/src/commands/reload.rs:176. Real `shelbi workflow list` in this project shows `task  6 statuses  Branch work with a review gate: the default track shipped with every project.` and `subtask  5 statuses  A piece of a parent task, squash-merged into the parent's branch with no PR or review.` and no `default` row.
  - Fix: Replace L13-15 with: "`shelbi init` ships two workflows: `task`, the review-gated default track (`Backlog → Todo → In Progress → Review → Done → Canceled`), and `subtask`, a piece of a parent task that squash-merges into the parent's branch with no PR or review. The project's `default_workflow:` (set to `task` at init) picks the one new issues use. These subcommands list and edit them alongside any custom workflows you add."
- [severity: outdated] "The default is virtual: it lives in code, not on disk, until `workflow new` or `workflow edit` writes a file." (L17-18) and "When the workflows directory is empty, the only entry is the built-in `default` workflow." (L43-44)
  - Reality: The code-built `default` only appears when `workflows/` is absent or holds no workflow YAML (a pre-scaffold legacy project). Once any workflow file exists — including after `workflow new <other>` — `list` returns only the on-disk files and the virtual `default` disappears. `workflow new <name>` never writes `default.yaml`.
  - Evidence: crates/shelbi-state/src/workflows.rs:301-310 (`list_workflows` returns `default_workflow()` only when dir missing or `raw_files.is_empty()`); crates/shelbi-cli/src/commands/workflow.rs:343-358 (test `list_sees_files_written_via_new`: after `new design-review`, `workflows.len() == 1`). Sandbox: `SHELBI_ROOT=$(mktemp -d) shelbi -p x workflow list` → `· default              6 statuses     The standard one-track flow shipped with every project.`
  - Fix: Replace L17-18 with "Workflow files live at `<config-root>/workflows/<name>.yaml`." and replace L43-44 with: "On a legacy project with no workflow files, `list` shows a single code-built `default` row marked `·`. As soon as any workflow file exists, only on-disk files are listed."
- [severity: unclear] "`show default` works even before any file is written, rendering the built-in default from code" (L54-55)
  - Reality: `show default` prints the code-built default whenever `default.yaml` is absent, even on a scaffolded project where `default` actually resolves to `task` everywhere else (board, dispatch). So it can show a table that differs from the workflow in use: in this project, `show default` shows `review  user  orchestrator` while `show task` shows `review  user  review`.
  - Evidence: crates/shelbi-cli/src/commands/workflow.rs:70-79 (checks `path.exists()` then falls back to `default_workflow()`; never calls `resolve_workflow_alias`). Real output: `shelbi workflow show default` → `review        user     orchestrator`; `shelbi workflow show task` → `review        user     review`.
  - Fix: Append: "On a project scaffolded with `task`/`subtask`, run `shelbi workflow show task` to see the default track. `show default` still prints the legacy built-in, which may differ."
- [severity: wrong] "Materialize the built-in default to disk and edit it: `shelbi workflow edit default`" (L111-115) and the `edit` description at L86-88
  - Reality: On a current project, `workflow edit default` writes a new `workflows/default.yaml` from the legacy built-in (no transitions/git). Because the `default` → `task` alias only fires when `default.yaml` is absent, this stops `workflow: default` references (and any `default_workflow: default`) from resolving to `task`. It does not edit the workflow new issues use.
  - Evidence: crates/shelbi-cli/src/commands/workflow.rs:125-140 (`edit_cmd` materializes via `write_starter` when `default.yaml` is missing); crates/shelbi-state/src/workflows.rs:219-250 (alias fires "only when `default.yaml` is genuinely absent").
  - Fix: Change the example to "Edit the shipped default track: `shelbi workflow edit task`". In the `edit` section add: "The materialize-`default` path is for legacy projects that predate `task`/`subtask`. On a scaffolded project, edit `task` instead."
- [severity: unclear] "Names must be non-empty, must not start with `.`, and may only contain `a-z`, `0-9`, `-`, `_`." (L70-72)
  - Reality: Names must also start with a lowercase letter or digit, so a leading `-` or `_` is rejected too. Uppercase is rejected by the state-layer check, but the error says "invalid agent id".
  - Evidence: crates/shelbi-core/src/model.rs:2494-2496 (`validate_workflow_name` = `validate_agent_id`), :3126-3142. Sandbox: `shelbi -p x workflow new _foo` → `Error: invalid agent id `_foo`: only lowercase ASCII letters, digits, `-`, and `_` are allowed and it must start with a lowercase letter or digit …`
  - Fix: "Names must start with a lowercase letter or digit and may contain only `a-z`, `0-9`, `-`, `_`."
- [severity: missing] (not documented) — no `list` example output (L36-44)
  - Reality: Each row is `<marker> <name padded to 20> <"N statuses" padded to 14> <description>`. A multi-line YAML description prints verbatim over several lines.
  - Evidence: crates/shelbi-cli/src/commands/workflow.rs:55-61.
  - Fix: Add a sample block after L41, e.g.
    ```text
      subtask              5 statuses     A piece of a parent task, squash-merged into the parent's branch with no PR or review.
      task                 6 statuses     Branch work with a review gate: the default track shipped with every project.
    ```

### site/content/docs/cli/workspace.mdx
- [severity: wrong] "A freshly [`init`](/docs/cli/init)'d project starts with an empty pool; the orchestrator provisions it on first boot (asking how many workspaces and which naming scheme), adding each slot with `shelbi workspace add <name>`." (L11-14)
  - Reality: `shelbi init` scaffolds a starter pool of two hub workspaces: `dev` and a `review`-tagged `review` slot. The orchestrator's ask-how-many/naming-scheme flow only runs when the pool is empty (e.g. a hand-written YAML with no `workspaces:`).
  - Evidence: crates/shelbi-orchestrator/src/project_create.rs:603-616 (`workspaces: dev, review[tags: review]`); crates/shelbi-cli/src/commands/init.rs:961-968 test "Fresh projects … ship a starter pool including a review-tagged slot"; crates/shelbi-state/src/default_orchestrator.md.template:334-361 (provisioning only when status shows none)
  - Fix: "A freshly [`init`](/docs/cli/init)'d project starts with two workspaces on the hub: `dev` and a review slot named `review`. If a project's pool is empty, the orchestrator provisions it on first boot (asking how many workspaces and which naming scheme) and adds each slot with `shelbi workspace add <name>`."
- [severity: missing] "(not documented)" — `shelbi workspace add` subcommand (insert a `## add` section before `## list`, L62)
  - Reality: `shelbi workspace add [OPTIONS] <NAME>` adds a workspace to the pool, creates its worktree, and wires Shelbi's Claude hooks into `.claude/settings.local.json`. `<NAME>` uses the issue-id character set (lowercase letters, digits, `-`, `_`). `--machine <MACHINE>` places it on a declared machine (default: the local hub). It refuses a name that already exists or slug-collides with one. If a user-authored `settings.local.json` can't be self-healed, it prints a merge message and exits with status 3 without adding the slot (merge the hooks and re-run). A hidden `--runner` is accepted and ignored with a "retired" notice.
  - Evidence: help/workspace_add.txt; crates/shelbi-cli/src/commands/workspace.rs:30-34 (`SETTINGS_MERGE_REQUIRED_EXIT = 3`), 42-56, 129-156
  - Fix: Add "## add" with usage `shelbi workspace add [OPTIONS] <NAME>`, the description above, and table rows `<NAME>` (string, required; lowercase letters, digits, `-`, `_`) and `--machine <MACHINE>` (string, default local hub, "Machine to place the workspace on"). Note exit code 3 = hooks merge required.
- [severity: missing] "(not documented)" — `shelbi workspace rm` subcommand (insert a `## rm` section after `## add`)
  - Reality: `shelbi workspace rm [OPTIONS] <NAME>` removes the workspace from the pool and tears down its worktree and session. It refuses a workspace holding an in-progress/review issue ("release it (`shelbi workspace stop <name>`) or pass --force") or whose worktree has uncommitted or unpushed work, unless `--force`.
  - Evidence: help/workspace_rm.txt; crates/shelbi-cli/src/commands/workspace.rs:58-65, 158-183
  - Fix: Add "## rm" with usage `shelbi workspace rm [OPTIONS] <NAME>`, the description above, and row "| `--force` | flag | off | Remove even if the workspace holds an in-progress/review issue or its worktree holds uncommitted or unpushed work. |" Pair it with a warning callout that `--force` discards that worktree's unpushed work.
- [severity: wrong] "`STATE` is live-probed, not board-derived: `list` probes each workspace's session (over SSH for remotes)." (L89-91)
  - Reality: `in_progress: <id>` and `review: <id>` come from the board (daemon-owned board index plus the assignment overlay). Only slots with no in-progress/review issue are probed, which is how `idle` / `occupied (user shell)` / `orphaned session` / `unreachable (…)` are decided. One unreachable verdict is shared per machine.
  - Evidence: crates/shelbi-cli/src/commands/workspace.rs:197-232 (`read_board`), 330-389 (`occupied_idle_workspaces` skips assigned slots)
  - Fix: "`STATE` combines the board and a live probe: a slot with an in-progress or review issue shows `in_progress: <task-id>` / `review: <task-id>` from the board; every other slot's session is probed (over SSH for remotes) to tell `idle` from `occupied (user shell)`, `orphaned session`, or `unreachable (<reason>)`."
- [severity: outdated] "This is the first command the orchestrator runs at session start to snapshot the pool" (L87-88)
  - Reality: The orchestrator bootstraps with `shelbi status --full --handoff`, whose Workspaces section renders the same table (`print_workspaces` is shared).
  - Evidence: crates/shelbi-state/src/default_orchestrator.md.template:214-225; crates/shelbi-cli/src/commands/workspace.rs:189-192
  - Fix: "The same table appears in the Workspaces section of `shelbi status --full`, the snapshot the orchestrator reads at session start. It's the view you want when answering …"
- [severity: wrong] "By default the in-flight issue is released back to its `ready` status (`todo` in the default workflow)" (L102-103)
  - Reality: An in-progress issue is always moved to `todo` and unassigned (reason `workspace:stop`), whatever its workflow's ready status. On a `review`-tagged slot the review-column issue is instead *parked*: unassigned, left in the review column, and marked so neither the auto-loader nor resume reloads it ("✓ <id> unloaded from review (parked — won't auto-reload)").
  - Evidence: crates/shelbi-cli/src/commands/workspace.rs:532-556, 686-713 (`move_status_and_unassign(&id, &Column::todo(), "workspace:stop")`)
  - Fix: "By default the in-flight issue is moved back to `todo` and its `assigned_to` field is cleared … On a review slot, the issue it was serving is unloaded instead: it stays in the review column, unassigned, and isn't auto-loaded again."
- [severity: unclear] "shelbi workspace stop charlie --keep-task / shelbi issue start docs-write-cli-reference-pages" (L161-166)
  - Reality: In the page's own `list` example `charlie` is the review slot serving `t-004`; `issue start` refuses a `review`-tagged workspace without `--force`, and the issue id doesn't match what charlie holds. Also `issue start` wipes the agent's context; to restart on the same issue without losing progress the dedicated command is `shelbi issue resume`.
  - Evidence: help/issue_start.txt (`--force` review-slot guard); help/issue_resume.txt
  - Fix: Use a dev slot and the matching issue, and prefer resume: "shelbi workspace stop alpha --keep-task" / "shelbi issue resume t-009", with lead-in "Bounce a stuck workspace and relaunch it on the same issue, keeping its work:".

### site/content/docs/cli/zen.mdx
- [severity: wrong] "each one does exactly one thing and prints a single line on stdout" (L22-L23)
  - Reality: Only `pr-create` and `ci-watch` print a single line. `probe` prints a pretty JSON object over many lines, or a JSON error object on exit 3. `scan` prints one id per line. `dry-run` streams one line per decision. `pr-merge` prints the SHA (or `sha-pending`) and then a `next eligible` block: either `next eligible: none`, or `next eligible (run shelbi zen scan to re-evaluate):` followed by one indented id per line.
  - Evidence: crates/shelbi-cli/src/commands/zen.rs:359 (probe `to_string_pretty`), :373 (probe error JSON), :454-470 (pr-merge prints SHA/`sha-pending` then `format_next_eligible`), :475-477 (scan loop), :799-810 (`format_next_eligible`).
  - Fix: "each one does exactly one thing and reports on stdout in a fixed, parseable shape: `probe` prints JSON, `scan` prints one task id per line, and `pr-create`, `ci-watch`, and `pr-merge` lead with a single result line (`pr-merge` follows it with the post-merge eligibility list)."
- [severity: missing] "(not documented)": `on`, `off`, and `pause` print the full `zen status` block after flipping the mode (L37-L58)
  - Reality: `set()` writes the mode and event, then calls `print_status`, so every toggle prints the same output as `shelbi zen status`. Toggles also refuse to run against a mismatched daemon version (`ensure_daemon_matches_for_mutation`), and so do `probe`, `pr-create`, and `pr-merge`. `status`, `scan`, `ci-watch`, and `dry-run` only print a warning.
  - Evidence: zen.rs:506-510 (`set` → `print_status`), :305-311 (version gate).
  - Fix: Under `## on`, add: "Prints the resulting `zen status` block. Like `off`, `pause`, `probe`, `pr-create`, and `pr-merge`, it refuses to run when the hub daemon's version doesn't match the CLI's."
- [severity: missing] "Print the current mode, the project's configured local check commands, the resolved danger-paths list (with detected project shapes labeled inline), the last crash timestamp (if any), and the count of in-flight tasks on the Zen track." (L66-L70)
  - Reality: The output also includes `ci timeout: <dur>`, a `per-workflow zen overrides:` section (only when a workflow declares a `zen:` block; lists each workflow's checks/ci_timeout/danger_paths), and `orchestrator alive: <ts>` when a heartbeat is recorded. `last crash` prints `never` when none, and adds the crash-record path when one exists. The danger-path header is `danger paths:`, `danger paths (detected: …):`, or `danger paths (project override):`. The last line is labeled `in-flight zen issues: N`.
  - Evidence: zen.rs:517-551, 556-610, 612-629. Real `shelbi zen status` output: `zen mode: on` / `checks: (none configured — set zen.checks.local in shelbi.yaml)` / `ci timeout: 15m` / `danger paths (project override):` … / `per-workflow zen overrides:` … / `last crash: 2026-10-06T03:00:34.281684+00:00  (/Users/jlong/.shelbi/projects/shelbi/crashes/….md)` / `in-flight zen issues: 1`.
  - Fix: "Print the current mode, the configured local check commands, the effective CI timeout, the resolved danger-paths list (labeled `project override` or with the detected project shapes), any per-workflow `zen:` overrides, the last crash (or `never`), the orchestrator's last heartbeat, and the count of in-flight issues on the Zen track." Optionally add a sample output block.
- [severity: missing] "\"Mechanical\" means not blocked on dependencies, not opted out via task frontmatter, and no file overlap with anything currently in flight." (L80-L81)
  - Reality: Two more mechanical filters apply. (1) A task the user demoted (`todo -> backlog reason=user:*` in events.log) is never re-promoted. (2) On an external tracker (GitHub), only backlog issues assigned to the authenticated user qualify, and nothing qualifies when that login can't be resolved (fail closed). Sorting is by priority, then by id. "File overlap" is a heuristic: path-like tokens in the candidate's body that appear in an in-progress task's body.
  - Evidence: crates/shelbi-orchestrator/src/zen.rs:14156-14166 (`read_demoted_task_ids`, `resolve_ownership_gate`), :14233-14270 (filters + sort), :14286-14295 (`file_overlaps_in_flight`).
  - Fix: Append: "It also skips tasks you demoted back to the backlog. On a GitHub board, it only considers backlog issues assigned to you, and lists nothing if your login can't be resolved. File overlap means a file path named in the task's body also appears in an in-progress task's body."
- [severity: missing] "`--interval <DURATION>` | duration | `5s` | Override the per-tick interval." (L101)
  - Reality: Intervals under 1s are raised to a 1s floor, with a stderr notice (`zen dry-run: interval … is below the 1s floor; using 1s.`).
  - Evidence: zen.rs:83 (`DRYRUN_MIN_INTERVAL` = 1s), :487-498.
  - Fix: Add to the description: "Values below `1s` are raised to `1s`."
- [severity: missing] "(not documented)": `pr-create` exit codes (after L151)
  - Reality: Exits 0 on success. Exits 75 (EX_TEMPFAIL) with empty stdout when the push landed but GitHub's PR view couldn't yet confirm the pushed head; this is retry-safe, so re-run the same command. Exits 1 on a hard provenance mismatch that must not be merged.
  - Evidence: `shelbi zen pr-create --help` ("Exit codes: 0 on success; 75 (EX_TEMPFAIL) …; 1 for a hard provenance mismatch"); zen.rs:66 (`TRANSIENT_VERIFICATION_EXIT = 75`), :391-399.
  - Fix: Add after L151: "Exits `0` on success, `75` when the push landed but GitHub could not yet confirm the PR head (retry the same command), and `1` on a provenance mismatch that must not be merged."
- [severity: outdated] "Repositories with branch protection, active rulesets, required-PR rules, fork heads, merge queues, or a non-squash strategy cannot use this boundary. Zen leaves the base unchanged and tells a human to review and merge through the repository's required GitHub workflow." (L236-L238)
  - Reality: Rulesets are now classified rule by rule. `deletion`, `non_fast_forward`, and other non-PR rules don't block the leased push. A satisfiable `pull_request` rule (0 required approvals, no code-owner review, squash allowed) makes Zen land through GitHub's own squash merge (`gh pr merge --squash`, pinned to `integration_sha`), so the landed commit is a new GitHub-authored SHA. Zen still refuses, leaving the base unchanged, when any of these hold: a `merge_queue` rule, required approvals, required code-owner review, squash not in the allowed methods, classic branch protection (or protection state that can't be read), auto-merge enabled or a merge queue present on the PR, a fork head, or a non-squash `merge_strategy`.
  - Evidence: crates/shelbi-orchestrator/src/zen.rs:2101-2116 (capability match → `merge_via_pull_request`), :2170-2222 (`merge_via_pull_request`), :2296-2446 (`BaseMergeCapability`, `classify_base_merge_capability`), :2041-2049 (non-squash refusal), :2090-2101 (auto-merge/merge-queue refusal).
  - Fix: Replace L236-L239 with: "When the base branch has a `pull_request` rule Zen can satisfy (no required approvals or code-owner review, squash allowed), Zen lands through GitHub's own squash merge, pinned to `integration_sha`, instead of the leased push. Other non-PR rules such as `deletion` and `non_fast_forward` don't block the leased push. Classic branch protection, a merge-queue rule, required approvals or code-owner review, a disallowed squash method, auto-merge, fork heads, or a non-squash `merge_strategy` all stop the flow: Zen leaves the base unchanged and asks a human to merge through the repository's required GitHub workflow."
- [severity: missing] "A landed candidate prints its SHA." (L239)
  - Reality: On the leased-push path it prints `integration_sha`. On the GitHub-merge path it prints GitHub's merge-commit SHA, or `sha-pending` if GitHub hasn't recorded it yet. After either, it prints the `next eligible…` block. It also deletes the remote task branch after landing (best effort).
  - Evidence: zen.rs (cli):454-470; orchestrator zen.rs:2158-2166 and :2208-2221 (`delete_remote_head_branch`, `merged_commit_oid`).
  - Fix: "On success it prints the landed SHA (`sha-pending` if GitHub hasn't recorded its merge commit yet), deletes the task branch on the remote, and then prints the next mechanically eligible backlog ids (or `next eligible: none`)."

## Related app issues (outside docs scope, for triage)
These came up while verifying the docs. They're bugs or stale text in the app, so a docs-only fix won't resolve them.
- `shelbi send --help` and the send.rs module comment still describe a fallback to the "legacy spawn agent registry", which the code removed (send.rs:166-205). The idle-workspace error suggests `shelbi workspace open {id}`, which doesn't exist (the command is `shelbi open`), and `shelbi task start`, a deprecated alias (send.rs:54).
- `shelbi detach --help` says "There is no `shelbi attach`", but `attach` exists.
- The `workspace stop` and `issue resume` help text still say "tmux pane".
- `shelbi agent edit --help` (agent.rs:40) gives the editor order as `$EDITOR`, `$VISUAL`, `vim`. The shared resolver actually tries `$VISUAL`, then `$EDITOR`, then `vi` (the test at agent.rs:645).
- `agent list`/`agent show` count only `skills/*.md` at the top level, so the shipped review skill `skills/load-run-detection/SKILL.md` shows as 0 skills / `(none)`.
- The path column of `shelbi remote list` shows each project's `repo:` value, which can be a git URL, not the project folder.
- Legacy `init`/`--pick-up` print `shelbi spawn TASK …`, a command that doesn't exist. `message` suggests `shelbi task assign`, the deprecated alias.
- `shelbi reload --help` still describes per-pane targets (`tasks`, `chat`, `workspace <name>`). reload.rs:38-42 ignores them with a note: every reload reloads the whole project.
- `events tail` and `project migrate-to-in-repo` define their own long-only `--project`/`--yes`. These shadow the globals, so `-p`/`-y` after those subcommands is rejected (`shelbi events tail -p shelbi` → `error: unexpected argument '-p' found`).
- An invalid workflow name errors with "invalid agent id".
- `shelbi workflow show default` prints the legacy built-in rather than the `task` workflow, and `workflow edit default` would write a `default.yaml` that breaks the `default` → `task` alias.
- `shelbi action --help` leaks internal references (`shelbi_orchestrator::actions::open_pr`, `Plans/workflows.md §12`).

## Verified accurate
All internal links and `#anchor`s on the 24 pages resolve, except the one broken-link finding in detach.mdx. The claims below were checked and found correct, so the fix task doesn't need to re-check them.
- **agent.mdx**: usage line, four subcommands with positional `<NAME>`, no per-subcommand flags; six shipped agents and their names; agents dir per config mode; `_shared` excluded from list; CUSTOMIZED yes/no/- semantics (including missing file = yes); `show` errors when missing and doesn't prepend the preamble; `new` errors if it exists, rejects empty/`.`/`_` prefixes, writes a minimal placeholder (not a copy of developer); `edit` errors when missing; statuses.yaml/id wiring example; internal links /docs/concepts/agents#customizing-an-agent, /docs/guides/getting-started/workflows#owners-and-agents, /docs/concepts/config-modes, /docs/concepts/workspaces, /docs/cli/workspace (AGENT column exists).
- **attach.mdx**: usage `shelbi attach [OPTIONS] <WORKSPACE>`; `--detach-key` default `ctrl-]`; rendered full-screen via a client-side emulator, not raw bytes (clips/letterboxes when another client is active); multiple viewers; no multiplexer; session outlives detach; examples parse (`ctrl-b` valid); links /docs/concepts/workspaces, /docs/cli/open, /docs/cli/session.
- **changelog.mdx**: usage block and all four subcommands, every flag and default (`--format` text for add/list/search, markdown for show), `-d`/stdin rule, `--author` default via $SHELBI_AGENT, `--date` forms, `--since` start-of-day, storage dir (state half, never the repo), UTC file naming and `-2`/`-3` collision suffixes, frontmatter shape (matches a real entry on disk), link syntax and word-boundary rule, PR resolution (issue tracker then github origin), template path and config-mode placement, materialization on init/create/reload/config-upgrade, palette "Edit Changelog Template", placeholder list and comment stripping, search semantics, prefix lookup for show, markdown and JSON shapes (checked against `show --format json`), schema_version/query/entry/entries rules, link `kind`s with null number/url, internal link /docs/concepts/changelog.
- **config.mdx**: command list and descriptions vs `config --help`; inventory usage (`--all`, `--format json` default json), mutual exclusion of explicit `--project` with `--all` ("--project and --all are mutually exclusive"), cwd/env project default; manifest fields (schema_version, shelbi_version, staged_dir, entries[] with logical_id/scope/canonical_path/candidate_path/format/exists/lifecycle_owned), the yaml/json/markdown formats, `global`/`project:<name>` scope strings, and logical_id shape (checked against real output); lint flags (`--staged`, `--format human|json` default human), staged `--all` semantics, exit 1 on any diagnostic, the five example codes, and diagnostic fields; upgrade flags, conflicts, read-only default, `~/.shelbi/config-upgrade-findings.json`, refusal of non-mechanical findings; dump-keybindings `-o/--out` and `--force` (refuses to clobber); check exits 1 on errors only (real run: `0 errors, 0 warnings`, exit 0); `update-shelbi-configuration` skill exists; all internal links and the `#keysyaml` anchor resolve, and docs/maintainers/system-configuration.md exists on main.
- **daemon.mdx**: usage line; subcommand set run/status/stop/restart (help/clap match); bare `daemon` == `run`; hub.sock default + `$SHELBI_HUB_SOCK`; events.log at `<root>/events.log`; pid file `<root>/shelbi.pid`; stop/restart refuse on stale/recycled pid or hub-socket pid mismatch (daemon_lifecycle.rs:282-351); session watchdog restarts a crashed daemon only while its project is open (shelbi-session/src/daemon_watchdog.rs:1-30); sessions survive daemon stop; `--root` precedence; forward-retry defaults 3 (clamped 1..=10) / 250 ms (≤5000) and TCP band 47100/64; `master_open_recovered … status=established` wording; internal links (/docs/concepts/events-log ×2) resolve.
- **detach.mdx**: usage line, `[CLIENT_ID]` arg, and `--list` flag match help; `--root` precedence wording; no `shelbi attach` for reattaching (plain `shelbi` reattaches); palette titles "Detach", "Detach Other Clients", "Quit Project", "Quit Shelbi" (crates/shelbi-app/src/command.rs:700-745); messages `Detached N client(s).` and `No clients attached.`; the `--list` table layout (ID/KIND/HOST/PROJECT/ATTACHED column widths, RFC3339 seconds timestamp, ` (pid N)` suffix) and short base-36 ids match detach.rs:126-143 and `fresh_client_id` (not run live per hub-safety rules); links /docs/cli/attach and /docs/cli/reload resolve.
- **events.mdx**: usage lines; `tail` is the only subcommand; `-n, --lines` default 20; `--since` ignores `-n` and prints all matches; `-f, --follow` streams (250 ms poll, survives truncation/rotation, holds partial lines); `--format raw|envelope` default raw; log path `<root>/events.log`; `workspace=` token and `<prev> -> <new>` shape; heartbeat cadence defaults 60s interval / 5m max (shelbi-core/src/model.rs:419-425); anchor `/docs/concepts/events-log#heartbeats` (heading "### Heartbeats" L79) and links /docs/concepts/events-log, /docs/concepts/orchestrator resolve.
- **init.mdx**: usage; two setup paths; `-y` global and position-independent; `-y` rejects `--mode`/`--pick-up`; global project layout and Welcome card (`tasks/welcome-to-shelbi.md`) from the same create function as the card's Enter; empty `workspaces: []` and orchestrator first-boot workspace interview (count + naming scheme in default orchestrator template); runner detection, both-installed hard error, explicit runner must be on PATH; `--orchestrator-runner` default; `--branch`/`--remote` hidden aliases; empty `--github-url` omits it; `--issue-tracker` values and preflight hard-fail; `--github-repo` default and requirement (and error without github); GitHub preflight checks (gh, auth, has_issues, push); stranded-board refusal; `issue_tracker:` placement; `--mode` values and non-TTY requirement; `--pick-up`; examples; links /docs/cli/workspace, /docs/concepts/config-modes, /docs/guides/getting-started/first-project, /docs/cli/wizard, /docs/cli/project.
- **issue.mdx**: usage lines and positional args for all 13 subcommands; `shelbi task` deprecated alias (hidden, prints notice, forwards; main.rs:136-143, 487-490); card path `~/.shelbi/projects/<project>/tasks/` (shelbi-state lib.rs:781); add flags/defaults (`--status backlog`, `--id`, `--depends-on` repeatable, `--prefers-machine`, `--workflow`, `--branch`); list `--status`/`--ready` (conflicts_with status)/`--workflow`, and grouped-by-column output (ran `shelbi issue list`); show; depends `--add`/`--remove`; move `--to`/`--reason` default `user:cli`, workflow fallback to `default`; unassign; start steps and `--reason` default `user:cli:start`; resume behavior (worktree untouched, recreated on existing branch if missing, `--continue` only for Claude, others re-inject prompt, restores `in_progress`, `--reason` default `user:cli:resume`; workspace.rs:2880-2885, 6831-6853); edit flags, mutual exclusions, active-status warning (mutate/add_edit.rs:248); comment; issue-store migrate flags (`--to github|file_system`, `--dry-run`, `--pace-secs` default 7.5); all internal links and anchors (#issue-store-migrate, #start, github-issues#comments-and-moving-cards-from-either-side, #migrating-between-backends).
- **machine.mdx**: usage lines for `setup <NAME>` and `status [<NAME>]`; interactive-login PATH probe; managed install path `~/.shelbi/bin/shelbi`; too-old package-manager binary never overwritten (managed copy alongside); idempotent setup; `status` columns (reachability, path, version, compatibility) and single-machine form; machines come from the project YAML `machines:` block (`p.machine(&name)`, `p.machines`); links /docs/concepts/workspaces and /docs/cli/attach resolve.
- **message.mdx**: usage lines and args_conflicts_with_subcommands parsing (positionals optional at clap, runtime errors when missing); log path `<worktree>/.shelbi/messages/<task-id>.log`; no keystrokes/Enter into the session; kinds reply/directive/context and their meanings; `--in-response-to` sets `in_response_to` and marks the stored question answered; `--wait` bare = 120s, polls events.log every 500ms, exits non-zero on timeout and survives an early TimedOut; status reads events.log; queued≠delivered wording and no ✓; send-vs-message table; examples parse (`--wait 90` after positionals); internal links /docs/cli/send, /docs/cli/question, /docs/concepts/workspaces.
- **open.mdx**: usage `shelbi open [OPTIONS] <NAME>`; errors on an undeclared workspace listing known names; used by the TUI sidebar/palette focus path via `shelbi_orchestrator::focus_workspace` (lib.rs:393-425; callers shelbi-tui app.rs:833, kanban.rs:1594); local and remote (remote shell spawned over SSH); `--root` / `-p` rows; links /docs/cli/attach, /docs/concepts/workspaces.
- **project.mdx**: usage lines; subcommand set add / migrate-to-in-repo (no hidden ones); `--dry-run` and `--yes` descriptions (match clap doc comments); `-p` value name `<NAME>` on migrate; migrate split targets (`<repo>/.shelbi/project.yaml`, `~/.shelbi/projects/<name>/local.yaml`), moved dirs (workflows/, agents/, workspace-settings template), state that stays under ~/.shelbi; idempotence; one-way callout and revert recipe; `add` launches the dashboard (`shelbi_tui::run_main`) only when a project was created; links /docs/concepts/config-modes ×2, /docs/cli/init, /docs/cli/wizard resolve.
- **question.mdx**: usage lines and `list` flags (`--task`, `--all`, `--json`) match help; storage path `~/.shelbi/projects/<project>/questions/<task-id>/<question-id>.md`; events line carries `question=`, `task=`, `file=` with a preview; `orchestrator events next/drain` attach `question` with `question.text`; `-2`/`-3` suffix on id reuse and identical re-send treated as retry; list is oldest-first and open-only unless `--all`; `<task>/<id>` form and ambiguous-id error pointing at `--task`; `shelbi message <task> reply … --in-response-to <id>` stamps `answered_at` and `answer_msg_id` (message.rs:244-260); `/docs/cli/message` link resolves; real `shelbi question list --all` output shape matches the description.
- **relay.mdx**: command is hidden (`#[command(hide = true)]`, main.rs:426-427) and the page correctly labels it internal; usage `shelbi relay [OPTIONS]`; hub starts it as `ssh <host> <bin> relay` (shelbi-orchestrator remote_session.rs:131-134) using the resolved remote binary path; one relay per machine, cached and restarted on drop, clients reconnect by sequence number, Ping/Pong keepalive (shelbi-proto relay.rs:16-18, 62-65); holds no session state; `--root` row; links /docs/cli/machine, /docs/concepts/workspaces.
- **reload.mdx**: usage line `shelbi reload [OPTIONS] [TARGET] [NAME]`; orchestrator handoff via `agents/orchestrator/handoff.md`, ingested then deleted, degrading to a cold start on miss/timeout (reload.rs:15-21); legacy `.shelbi/project` marker sweep (reload.rs:145-169); Zen PR command pinning and custom-prose preservation in instructions.md/zenmode.md (reload.rs:114-140); unknown-target error lists valid set; bare `workspace` errors; `--project` example works from any cwd; /docs/guides/getting-started/install link resolves. Note: `shelbi reload --help` itself still describes pane respawn and targeted reloads (main.rs:268-292), so the help text is stale too.
- **remote.mdx**: usage lines; `connect <HOST> [PROJECT]` args; SSH alias or user@host; own ssh setup, never a password; Reconnecting notice and Esc; recent remote sessions; `list` is read-only and never starts the daemon; `list --json` keys `protocol`, `version`, `user`, `daemon_running`, `projects[{name,label?,path}]` (checked against real output); header line format `Shelbi <ver> as <user> (daemon running|not running)`; `--root` row; link /docs/concepts/remote-sessions.
- **send.mdx**: usage line `shelbi send [OPTIONS] <ID> <MESSAGE>`; text then separate Enter, verify, one retry Enter; `queued` only when busy before and after; stuck makes the command fail; events.log line `send project=… workspace=… status=submitted|queued|stuck|unverified detail=…` (event_log.rs:2545-2553); `-p` flag row; ephemeral vs `shelbi message` (comparison table exists at message.mdx L57); links /docs/concepts/workspaces, /docs/cli/message.
- **session.mdx**: usage lines and every flag/default for ls/new/kill/send/snapshot match `--help` and clap source (`--name` required, `--cwd` default cwd, `--cols` 80, `--rows` 24, `--task`, `--raw-log`, trailing `<COMMAND>...`; `kill --signal`; `send --enter` sends `\r` after a bracketed paste; `snapshot --history`); subcommand descriptions; no hidden subcommands; links /docs/cli/attach ×2 and /docs/concepts/workspaces resolve.
- **status.mdx**: usage line, `--full`/`--handoff` flags and their composition order (full first, then the consume block), HANDOFF.md read from the local `work_dir` then deleted, no-op when absent; `list` subcommand prints ORDER/ID/NAME/CATEGORY from `workflows/statuses.yaml` (real output matches); real `--full` headings `## Board`/`## Workspaces`/`## Zen`/`## Handoff`/`## Daemon`/`## GitHub`; links to /docs/concepts/orchestrator, /docs/cli/events, and the github-issues anchors (#the-budget-and-what-shelbi-status-shows, #freshness-and-rate-limits) resolve.
- **wizard.mdx**: usage (no own flags); preflight then one card with "Enter launch    c customize    q quit"; Enter creates and launches the dashboard; Customize fields (project, repo path, branch, remote, runner, orchestrator runner, issue tracker); q/Esc/Ctrl+C quit with no project state; "Which agent?" when both runners present; `git init -b main` permission prompt for non-repos; missing-runner guidance stops before the card; issue-tracker question with GitHub pre-selection rule, gh preflight and retry/fall-back; idempotence guard keyed on any registration; `shelbi project add` runs the shared flow; anchor /docs/cli/init#where-your-issues-live and links /docs/guides/getting-started/first-project, /docs/cli/init, /docs/cli/project.
- **workflow.mdx**: usage lines and positional `<NAME>` for list/show/new/edit; `--edit` flag on `new` (only subcommand flag); `·`/blank marker semantics; `show` table columns STATUS/OWNER/AGENT, `statuses.yaml` ordering and dropping of undeclared statuses; `show <missing>` error "workflow `X` not found at <path>"; `new` scaffold = six-status default with `name:` set and description dropped for non-default names; `new` refuses existing names; `edit` uses `$EDITOR` falling back to `vi` and its missing-name hint "run `shelbi workflow new <NAME>` first"; global-mode/in-repo paths; all three internal links resolve.
- **workspace.mdx**: `list` columns NAME/HOST/AGENT/INTEG/STATE, AGENT from issue `agent:` param else `developer`, `review` for a serving review slot, `-` when idle; INTEG from the project's baseline runner (Claude/Codex → conventional, other → degraded); STATE vocabulary (workspace.rs:298-304); example table shape matches `shelbi workspace list`. `stop` usage, `--keep-task`, kills remote session too. `status [NAME]` reads `~/.shelbi/workspaces/<name>/status.yaml` (workspace_status.rs:158-161), single-row + raw yaml. Runner config pointer (`agents.<name>.runner`, `runners.<kind>`). All internal links (#status, configuration/workflow#run-ready-and-teardown, concepts/review-workspaces, cli/agent, configuration/agents).
- **zen.mdx**: subcommand set (on/off/pause/status/scan/dry-run/probe/pr-create/ci-watch/pr-merge) matches help; events.log toggle line shape and `reason=user:cli`; state.json persistence; Alt+Z (default `zen_toggle: alt-z` in dump-keybindings); dry-run flags, 5s default, log path `~/.shelbi/logs/zen-dryrun.log` and activity-feed events; probe JSON fields (repository, repository_id, base_branch, base_sha, integration_sha, head_sha, published_head_sha, merge_conflict, diff_size), `integration_sha` null on conflict, exit 3 with JSON error; pr-create/ci-watch/pr-merge usage lines and the six required `--match-*` flags plus optional `--match-published-head-commit`; ci-watch outputs green/red/timeout with exits 0/1/2; `--timeout` default `zen.ci_timeout` = 15m (shelbi-core model.rs:2646) and `--task` workflow resolution; leased push via `--force-with-lease=refs/heads/<base>:<base_sha>`; all internal links and anchors (#the-zenmodemd-file, #the-judgment-layer) resolve.
## Resolution

Fixed on branch `jlong/docs-audit-fixes` (task `docs-audit-fixes`), 2026-10-29, rebased on origin/main 9089caab. Each finding was re-verified against current source and/or the installed `shelbi` 0.11.0 before editing. Commit SHAs are pre-merge; the PR squash-merges them into one commit on `main`. Per-finding status: **fixed**, **skipped (reason)**, or **not reproducible**. `site/app/llms*.txt` and `docs-md` are generated from content at build, so they needed no hand edits. `npm run lint` and `npm run build` pass in `site/`.

### Coordinator follow-ups
- **Starter workspace pool.** cli/workspace.mdx's "dev plus review" was half right and now names both setup paths, matching init.mdx (see the concepts-config report's Resolution).
- **New pages** (quit, doctor, orchestrate, orchestrator, msrv-check, guard, action) take fractional `order:` values beside their related commands. No existing page was renumbered.

### cli-a: missing top-level commands, agent, attach, changelog, config, daemon, detach, events, init, issue (commit `dddc050f`)


#### Top-level commands with no page (new pages)
- quit — no page — fixed (cli/quit.mdx, order 30.7; detach links it)
- doctor — no page — fixed (cli/doctor.mdx, order 31.5; samples are real output: hub `shelbi doctor` on GitHub project + sandbox file_system project; WARNING line described, not sampled, since not observed)
- orchestrate — no page — fixed (cli/orchestrate.mdx, order 26). Audit said `--status` "prints attach instructions and exits"; code (orchestrate.rs) ignores it (`let _ = args.status`) and still starts the orchestrator. Documented as reserved/no-op.
- orchestrator events — no page — fixed (cli/orchestrator.mdx, order 26.5) with a "plumbing" warning callout. Verified `drain` always persists the durable cursor, even with `--cursor` (drain_persisted); `next` without --follow doesn't.
- msrv-check — no page — fixed (cli/msrv-check.mdx, order 28.6; documents SHELBI_SKIP_MSRV_CHECK / SHELBI_MSRV_NO_INSTALL; links project.mdx#zen, which another agent already points at it)
- guard — no page — fixed (cli/guard.mdx, order 12.5; outputs observed in sandbox). Linked from init.mdx. wizard.mdx (not mine) should link `/docs/cli/guard`; it already appears to reference it.
- action — no page — fixed (cli/action.mdx, order 28.3; one section per subcommand; links configuration/workflow#actions). Linking it FROM configuration/workflow.mdx transitions section is outside my files.

#### agent.mdx
- editor order ($VISUAL, $EDITOR, vi) — fixed
- `agent list` example table — fixed (real sandbox output, sorted, status ids)
- skills count/listing only top-level *.md — fixed (list + show; notes review shows `(none)`)
- name charset (uppercase accepted) — fixed ("ASCII letters, digits, -, _")
- "never clobbered on upgrade" — fixed (self-heal on init/reload wording)
- agent.yaml / settings.json missing — fixed
- preamble path global vs in-repo — fixed
- (extra) CUSTOMIZED `no` also covers pristine-stale defaults — fixed

#### attach.mdx
- "any terminal on the hub or over SSH" — fixed (local sessions only; SSH to remote machine)
- hint bar shows only ~4s — fixed
- selector matching / dead sessions / --detach-key syntax / -p tie-break — fixed (table rows + "How <WORKSPACE> is matched" subsection)

#### changelog.mdx
- example id slug (`...-changelog-lands-in-the-sidebar`, 4 places) + slug rule — fixed
- add validation refusals — fixed
- empty-result messages + skipped-file warning — fixed

#### config.mdx
- staged snapshot compat is schema_version strict equality — fixed (L59 + Compatibility)
- lint scope missing pr/changelog templates, agent.yaml, settings.local.json — fixed
- list-actions per-project override layer + table columns — fixed

#### daemon.mdx
- control.sock / poller / board refresh missing — fixed (also summary)
- idle exit timing (15s grace, 5s poll) — fixed (env overrides left out as test-only)
- status output shape — fixed (sample matches live `shelbi daemon status`)
- stop/restart messages + refusal errors — fixed
- `-p` ignored — fixed
- "TUI poller" rechecks forward — fixed (daemon per-project poller, 120s)
- master_open_failed / loopback_port_exhausted field order — fixed
- forward env vars read by daemon process — fixed

#### detach.mdx
- `shelbi quit` link to /docs/cli/open — fixed (/docs/cli/quit)
- desktop app already a client — fixed
- exit code: `No client '<id>' attached.`, non-zero on control-socket failure — fixed
- `-p` ignored — fixed
- (extra) dropped "There is no `shelbi attach` for this" (attach exists); clarified attach is a different command

#### events.mdx
- `-p` not accepted on `events tail` — fixed
- `--project` row missing — fixed
- orchestrator's source of truth is `orchestrator events next` — fixed (intro, tail description, example now `--follow --project myapp`, see-also)
- line shapes (heartbeat fields, owner=, hub ops) — fixed (also summary)
- envelope has version/transport — fixed
- --since units / bare seconds / unparsable kept — fixed

#### init.mdx
- non-git dir -> `git init -b main` with -y — fixed (verified in sandbox)
- pre-commit guard install — fixed (links /docs/cli/guard)
- re-run prints "already configured" — fixed (verified)
- which overrides require -y; `--root` row for legacy path — fixed (verified error messages)
- `--root` placement (after init vs global) — fixed
- `--project` normalization — fixed (verified message)
- File system "default" wording — fixed
- issue-store migrate anchor — fixed
- (coordinator note) starter pool: verified BOTH behaviors exist. `init -y`, `shelbi wizard`, `shelbi project add`, first-run setup (wizard.rs:197) write `workspaces: []` (sandbox confirmed). Legacy `init` without -y and TUI/desktop Add Project (project_create::scaffold_project/render_project_yaml) write `dev` + review-tagged `review` (sandbox `init --mode global` confirmed). init.mdx now states both. workspace.mdx's unconditional "dev plus review" is only true for those paths.

#### issue.mdx
- status matching is a fixed alias table, custom ids exact, error lists declared statuses — fixed
- move runs transition actions; --skip-transition-actions — fixed
- assign --force + occupied-workspace refusal — fixed
- start --force / --take — fixed
- resume --take — fixed
- add body from stdin — fixed
- rm on github closes as not_planned; suggest `canceled` — fixed
- prio flags mutually exclusive — fixed
- (extra) start/resume "pane" wording -> "session"

#### Related app issues (skimmed; no doc action needed in my files)
- agent edit --help editor order, agent skills count, detach --help "There is no shelbi attach", action --help internal refs, events tail `-p` shadowing: all still present in code (docs now describe real behavior).

#### Product issues noted
- `shelbi orchestrate --status` is a no-op (still starts the orchestrator).
- `agent list`/`show` ignore directory-style skills, so the shipped review skill is invisible.
- Starter workspace pool differs by creation path (empty for init -y / wizard / project add; dev+review for legacy init and TUI/desktop Add Project).
- `action merge` uses only the project-level `git.merge_strategy` (per-workflow ignored), consistent with the known issue.
- `orchestrator events drain --cursor N` still overwrites the durable cursor (help text implies only the no-cursor form persists).

### cli-b: machine through zen (commit `d362c1e9`)


#### cli/machine.mdx
- missing `check <NAME>` subcommand — fixed (table row, `### check` section, example; summary/intro say "find, check, or install")
- what "compatible" means (major.minor) — fixed
- `status` output / REACHABLE+COMPATIBLE values / machines.json refresh — fixed (sample + value lists)
- `setup` details (local no-op, checksum, hub-copy fallback, machines.json, output lines, unknown name) — fixed

#### cli/message.mdx
- 4th outcome `unknown message id`, non-zero for non-delivered — fixed (intro bullet + status section)
- done / no-live-reader still written; hard errors write nothing — fixed
- push daemon-version gate — fixed (note under Flags; also mentions the restart offer is skipped while workspaces are busy)
- JSON line shape / `queued <msg-id>` output / msg-id format — fixed

#### cli/open.mdx
- user-shell marks slot occupied, not relaunched, work_dir fallback, no-op if session live — fixed
- "mid-task" really means any assigned issue — fixed

#### cli/project.mdx
- `-y` ignored by `add`; `migrate-to-in-repo` rejects `-y` — fixed (verified: `error: unexpected argument '-y' found`)
- card keys q/Esc, git init, registered path, in-repo pick-up — fixed
- migrate output + non-TTY `.gitignore` behavior — fixed

#### cli/question.mdx
- frontmatter also has project/context/answer_msg_id — fixed
- hub-level fallback store — fixed
- `show` options table (`--task`, `--json`) + output — fixed

#### cli/relay.mdx
- `-p` ignored — fixed (row reworded)

#### cli/reload.mdx
- outdated pane-respawn description + summary — fixed (4-step session-runtime description)
- targets ignored (note + full reload) — fixed (table + examples removed; compatibility note kept; unknown-target error kept)
- worker panes "re-shell" — fixed (workers keep running on their old binary until relaunched; daemon restart is hub-wide)
- other repair work — fixed ("What else reload repairs" list incl. workflow migration, statuses.yaml, config-upgrade, all-agent self-heal, template reset)
- output sample + pid-refusal error — fixed
- `-y` no effect — fixed
- install.sh-only phrasing — fixed (brew/apt/source checkout + anchor link; added brew example)

#### cli/remote.mdx
- path column is `repo:` value (may be URL) — fixed (doc describes current behavior; example kept)
- no-terminal `--yes` behavior — fixed
- PROJECT matching / error lists projects — fixed

#### cli/send.mdx
- legacy spawn registry fallback (prose + arg row) — fixed
- Codex is verified, only custom runners unverified; profile from baseline runner — fixed
- live-session requirement, output, transport stuck, serialization — fixed

#### cli/session.mdx
- `ls` lists all projects — fixed (example lead-in)
- `ls` output shape / dead rows — fixed (sample + STATE/SIZE values)
- selector resolution order and tie-breaks — fixed
- `-p` only breaks ties — fixed
- snapshot/send/kill on dead sessions — fixed
- `new` output / sessions dir — fixed
- `snapshot alpha` ambiguity — fixed (`-p myapp` added)

#### cli/status.mdx
- `## Disk` section + session-lost line — fixed
- summary output sample + optional lines — fixed
- `--handoff` refuses mismatched daemon — fixed

#### cli/wizard.mdx
- `-p`/`-y` no effect, only `--root` — fixed
- already-registered exits quietly — fixed
- pre-commit guard install — fixed (links /docs/cli/guard, page being created by another agent)
- issue-store link anchor — fixed (`#issue-store-migrate`)

#### cli/workflow.mdx
- built-in `default` intro outdated (task/subtask, default_workflow, alias) — fixed
- virtual default / list fallback — fixed
- `show default` may differ from `task` — fixed (verified live: review agent orchestrator vs review)
- `edit default` breaks alias; example — fixed (examples now use `task`)
- name rule (must start with letter/digit; "invalid agent id") — fixed
- list sample output — fixed

#### cli/workspace.mdx
- init starter pool is dev + review (not empty) — fixed (verified project_create.rs + init test)
- missing `add` section — fixed (incl. exit 3)
- missing `rm` section — fixed (incl. --force warning callout)
- STATE board+probe — fixed
- orchestrator uses `status --full` — fixed
- stop: always `todo`, review slot parked — fixed
- stop/start example → stop alpha --keep-task / issue resume t-009 — fixed

#### cli/zen.mdx
- "single line on stdout" — fixed
- toggles print status block + version gate split — fixed
- status output fields — fixed (plus sample)
- scan extra filters (demoted, GitHub ownership fail-closed, id tiebreak, overlap heuristic) — fixed
- dry-run 1s floor — fixed
- pr-create exit codes 0/75/1 — fixed
- pr-merge ruleset classification / GitHub squash-merge path — fixed
- pr-merge output (sha / sha-pending, branch delete, next eligible) — fixed

#### Product issues noted
- `shelbi send` idle-workspace error suggests nonexistent `shelbi workspace open {id}` and deprecated `shelbi task start`; `send --help`/module doc still mention the legacy spawn registry.
- `shelbi reload --help` still describes per-pane targets; code ignores them.
- `shelbi workspace --help` (stop/status) still says "tmux pane"/"no tmux probing".
- `shelbi workflow show default` prints the legacy built-in rather than resolving to `task`; `workflow edit default` writes a `default.yaml` that breaks the `default` -> `task` alias.
- Invalid workflow names error with "invalid agent id"; the CLI pre-validator accepts uppercase/leading `-`/`_` that the state layer then rejects.
- `shelbi remote list` path column is the project's `repo:` value (can be a git URL or empty), not a folder.
- `project migrate-to-in-repo` (and `events tail`) define long-only `--yes`/`--project` that shadow the globals, so `-y`/`-p` after the subcommand is rejected.
- `project add` / `wizard` ignore global `-y`; `message` suggests deprecated `shelbi task assign`.
- Zen pr-merge refuses on project-level non-squash `merge_strategy` (per-workflow strategy not consulted).
- Cross-scope inconsistency for the coordinator: the cli audit's "verified accurate" list says init.mdx's empty `workspaces: []` is right, but current code (project_create.rs:603-616, init.rs test) scaffolds `dev` + `review`. workspace.mdx now says dev + review; init.mdx should be checked to match.

