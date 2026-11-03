# Docs audit: concepts & configuration

Audited at origin/main `a749b0d1c99120f622dacb4e2cb26c47f2c12a96`, shelbi 0.11.0, 2026-10-29.

Method: every page was read against the source at that commit (`crates/`, `desktop/src`, shipped `default_*.template` files and scaffolds), and checked against the installed CLI. Read-only commands ran against the live hub: `--help`, `shelbi status [--full]`, `shelbi issue list`, `shelbi zen status`, `shelbi workflow list/show`, `shelbi agent list`, `shelbi config list-actions`, and tailing `~/.shelbi/events.log`. Mutating commands ran only in throwaway roots (`env -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK SHELBI_ROOT=<tmp>`): `shelbi init` in both modes, `--pick-up`, `project migrate-to-in-repo --dry-run`, `config upgrade`, `config check`, `agent new`. In this report, "sandbox" means one of those throwaway roots. Line numbers refer to the `.mdx` files at that commit.

## Summary

121 findings across 16 pages:

- **wrong: 34**
- **outdated: 31**
- **missing: 32**
- **unclear: 21**
- **broken-link: 3**

| Page | wrong | outdated | missing | unclear | broken-link | total |
|---|---|---|---|---|---|---|
| concepts/agents.mdx | 2 | 1 | 2 | 1 | 0 | 6 |
| concepts/changelog.mdx | 0 | 0 | 0 | 1 | 0 | 1 |
| concepts/config-modes.mdx | 2 | 3 | 2 | 3 | 0 | 10 |
| concepts/config-upgrade.mdx | 2 | 1 | 1 | 2 | 0 | 6 |
| concepts/events-log.mdx | 3 | 4 | 5 | 0 | 0 | 12 |
| concepts/github-issues.mdx | 1 | 1 | 3 | 2 | 0 | 7 |
| concepts/orchestrator.mdx | 4 | 3 | 0 | 0 | 0 | 7 |
| concepts/remote-sessions.mdx | 1 | 0 | 1 | 0 | 0 | 2 |
| concepts/review-workspaces.mdx | 4 | 2 | 2 | 0 | 0 | 8 |
| concepts/workspaces.mdx | 5 | 6 | 1 | 3 | 0 | 15 |
| concepts/zen-mode.mdx | 3 | 4 | 3 | 2 | 0 | 12 |
| configuration/agents.mdx | 0 | 0 | 4 | 1 | 1 | 6 |
| configuration/global.mdx | 1 | 1 | 3 | 2 | 0 | 7 |
| configuration/project.mdx | 2 | 3 | 2 | 3 | 1 | 11 |
| configuration/statuses.mdx | 1 | 0 | 0 | 0 | 0 | 1 |
| configuration/workflow.mdx | 3 | 2 | 3 | 1 | 1 | 10 |

The same problems turn up on several pages. The fix task can sweep for each of them:

1. **tmux leftovers**:
   - "window 1", "dashboard window" and "pane" for the orchestrator and workspaces
   - per-pane `reload`
   - the deleted first-run Zen hotkey probe
   - the dead Zen crash-recovery heartbeat

   Today everything runs as a detached Shelbi daemon session.
2. **Old shipped defaults**:
   - a `default.yaml` workflow, where `shelbi init` now writes `task.yaml` + `subtask.yaml` with `default_workflow: task`
   - two agents, where Shelbi now ships six
   - a config root missing `zenmode.md`, `pr-template.md` and `changelog-template.md`
3. **Orchestrator event intake**: the passive `events tail --follow --project` watch is the background monitor, and the claim/ack `events next` self-drain is the source of truth. Review loads are done by the hub's auto-loader, not the orchestrator.
4. **Zen policy lives in `zenmode.md`**, not "the orchestrator's prompt".
5. **The literal `review` tag is special-cased** in code, despite "nothing branches on the word review". Several examples also put `tags: [review]` on a machine, which turns every slot on that machine into a review slot.
6. **Settings pages are never mentioned** on the agents, workflow and keys pages: Settings ▸ Agents, Settings ▸ Workflows and Settings ▸ Keybindings.
7. **Anchors with underscores**: the headings `## agent_runners` and `## required_params` slug to `#agent_runners` and `#required_params`, but the pages link to `#agent-runners` and `#required-params`.

## Findings

### site/content/docs/concepts/agents.mdx

- [severity: outdated] "Runs in window 1, never in a workspace." (line 31); "it's just the agent that happens to run in window 1" (line 121)
  - Reality: tmux windows are gone. The orchestrator runs in the project's own daemon-hosted orchestrator session, shown as the **Orchestrator** sidebar row and view (⌘O on desktop).
  - Evidence: `crates/shelbi-app/src/view.rs:372`; `desktop/src/keys.rs:63`; `grep -rn "window 1" crates/` finds nothing.
  - Fix: "Runs in its own orchestrator session (the **Orchestrator** row in the sidebar), never in a workspace." and "…it's just the agent that runs in the orchestrator session."
- [severity: wrong] `review` row: "The dev server itself is booted by the status's transition commands, and it hands over a working URL." (line 33)
  - Reality: the Review agent itself runs the workflow's `review:` recipe (setup → serve via `shelbi __review-serve` → ready → URL). With no `review:` block it brings nothing up and presents the diff.
  - Evidence: `crates/shelbi-state/src/default_review.md.template:30-80`; `crates/shelbi-core/src/workflow.rs:131-143`.
  - Fix: "Its charter is to make the finished branch runnable for a human: it runs the workflow's [`review:` recipe](/docs/configuration/workflow#review) and hands over the URL, or presents the diff when the workflow declares none."
- [severity: missing] On-disk tree (lines 69-99) shows no `agent.yaml`.
  - Reality: every shipped agent dir has `agent.yaml` beside `instructions.md`, `settings.json`, `skills/`. `shelbi agent new perf` creates `perf/instructions.md` and an empty `perf/skills/`.
  - Evidence: sandbox `shelbi init -y` tree (`agents/<each>/agent.yaml`); sandbox `shelbi agent new perf`; `crates/shelbi-state/src/agent_workspaces.rs:928-933`.
  - Fix: add `│   ├── agent.yaml          # manifest: preferred runner, model, effort ([reference](/docs/configuration/agents))` under each shipped agent and `└── skills/` under `perf/`.
- [severity: wrong] "A category-based fallback still exists for YAML loaded from older configs — `ready` resolves to `orchestrator`, `active` to `developer` … emits a one-per-workflow deprecation diagnostic" (lines 175-181)
  - Reality: the fallback needs an inline `category:` on the workflow status, which the project loader now rejects. For any on-disk workflow a bare `owner: agent` is a load error.
  - Evidence: `crates/shelbi-state/src/workflows.rs:196-200`. Sandbox: inline category → "carries inline status identity … drop inline name, category"; bare owner → "status `todo` has owner: agent but neither an `agent:` field nor a `category:` to derive a default from — add one".
  - Fix: delete the fallback sentence; keep "Name it explicitly: a bare `owner: agent` with no `agent:` fails the workflow load."
- [severity: unclear] Example sets `agent: qa` on the `review` (handoff) status and says it "is auto-reviewed by the shipped `qa` agent before a human ever sees it" (lines 164-183); "Point a `handoff`- or `active`-category status at `agent: security`" (lines 186-189)
  - Reality: the automated gate (daemon auto-dispatch, pass forward / bounce back) keys on an `owner: agent` **active**-category status other than the primary `in-progress`. Making the `review` handoff `owner: agent` replaces the human review lane instead of adding a pass before it.
  - Evidence: `crates/shelbi-core/src/workflow.rs:325-400` (`forward_agent_active_gate`, `is_agent_active_gate`, `gate_pass_status`); `site/content/docs/guides/doing-more-with-agents/add-to-workflow.mdx:23-51`.
  - Fix: make the example a separate gate: a `qa-review` status (category `active` in `statuses.yaml`) declared `{ id: qa-review, owner: agent, agent: qa }` with transitions `in-progress → qa-review → review`. Change "a `handoff`- or `active`-category status" to "an `active`-category gate status".
- [severity: missing] "Edit an agent by hand, or through the CLI" (lines 148-157); reset advice (lines 144-146)
  - Reality: Settings ▸ Agents (desktop ⌘6; TUI `,` then `6`) edits instructions, shared preamble, skills and manifest fields, with New / Duplicate / Rename / Delete and Reset to default.
  - Evidence: `crates/shelbi-state/src/agents_config.rs:1-55`; `crates/shelbi-app/src/settings.rs:75-77, 113-114`.
  - Fix: "Edit an agent by hand, in **Settings ▸ Agents** (desktop and TUI), or through the CLI:"; mention Reset to default next to the delete-and-reload advice.

### site/content/docs/concepts/changelog.mdx

- [severity: unclear] "`~/.shelbi/projects/<project>/agents/orchestrator/instructions.md`" (lines 73-75)
  - Reality: correct for global mode only. In in-repo mode agents live at `<repo>/.shelbi/agents/orchestrator/instructions.md`. (Changelog entries themselves always stay in `~/.shelbi/projects/<name>/changelog/`.)
  - Evidence: `crates/shelbi-state/src/lib.rs:773-778` (`agents_dir` → `config_project_dir`); `crates/shelbi-state/src/changelog.rs:76-80`.
  - Fix: "`<config-root>/agents/orchestrator/instructions.md` (`~/.shelbi/projects/<project>/` in global mode, `<repo>/.shelbi/` in in-repo mode), or run `shelbi agent edit orchestrator`".

### site/content/docs/concepts/config-modes.mdx

- [severity: outdated] In-repo tree: "workflows/ default.yaml statuses.yaml / agents/ orchestrator/ developer/ _shared/ # optional shared preamble" (lines 106-113)
  - Reality: `init --mode in-repo` writes `workflows/{task,subtask,statuses}.yaml` (no `default.yaml`), six agents (orchestrator, developer, review, qa, security, adversarial), always writes `agents/_shared/preamble.md` (a missing one is auto-healed, `SHARED_PREAMBLE_MISSING`), and writes `zenmode.md`, `pr-template.md`, `changelog-template.md` at `<repo>/.shelbi/`. The `project.yaml` comment also omits `default_workflow`, `workspace_poll_interval_secs`, `github_reconcile_interval_secs`, which init writes.
  - Evidence: sandbox `shelbi init --mode in-repo` output ("✓ wrote project workflow: …/workflows/task.yaml", "✓ wrote Zen policy: …/.shelbi/zenmode.md", "✓ wrote PR template", "✓ wrote changelog template", "✓ wrote shared agent preamble"); `crates/shelbi-state/src/workflows.rs:158-179`; `crates/shelbi-state/src/agent_workspaces.rs:228-270`.
  - Fix: tree → `workflows/{task.yaml,subtask.yaml,statuses.yaml}`, `agents/{orchestrator,developer,review,qa,security,adversarial}/` + `agents/_shared/preamble.md` (drop "optional"), plus `zenmode.md`, `pr-template.md`, `changelog-template.md`.
- [severity: missing] Table row "Shared directories (workflows/, agents/, workspace-settings.json.template)" (line 39)
  - Reality: the config root also holds `zenmode.md`, `pr-template.md`, `changelog-template.md`.
  - Evidence: `crates/shelbi-state/src/zenmode.rs:35`, `pr_template.rs:33`, `changelog.rs:84` (all `config_project_dir()`).
  - Fix: "Shared config files (workflows/, agents/, workspace-settings.json.template, zenmode.md, pr-template.md, changelog-template.md)".
- [severity: missing] "Shelbi reads the name: field … and checks that you've registered the project locally" (lines 144-151)
  - Reality: there is a second check: the discovered repo root must be the registered project's `work_dir` (or a `<work_dir>/.shelbi/wt/<name>` worktree). Otherwise Shelbi warns and falls back to the registry, so a suffixed second clone (`shelbi-2`) never resolves from cwd.
  - Evidence: `crates/shelbi-state/src/resolve.rs:155-190` (`in_repo_root_is_trusted`). Sandbox `shelbi status` in a second clone: "ignoring in-repo project config … not the registered work_dir … for project `myapp`" then "no project specified".
  - Fix: add "…and that the repo root is that project's registered `work_dir` (or a Shelbi worktree under it); otherwise the walk-up match is ignored and resolution falls back to the registry. A suffixed alias clone is only reachable with `-p <alias>`."
- [severity: unclear] "The local alias is fixed at pick-up time; there is no rename command, so … re-pick-up the clone (or edit the registry filename and its entry by hand)" (lines 196-199)
  - Reality: `--pick-up --project <alias>` chooses the alias up front. An in-repo registration is the directory `~/.shelbi/projects/<alias>/` (holding `local.yaml` and state), not a file. Re-running pick-up while the old alias exists just creates the next suffix.
  - Evidence: `crates/shelbi-cli/src/commands/init.rs:655-669`; `shelbi project --help` lists only `add` and `migrate-to-in-repo`.
  - Fix: "Pick the alias up front with `shelbi init --pick-up --project <alias>`. There is no rename command; to change it later, move `~/.shelbi/projects/<alias>/` (registration and state) by hand."
- [severity: outdated] "Moves `workflows/`, `agents/`, and `workspace-settings.json.template`" (lines 210-213) and "What to commit" (lines 251-257)
  - Reality: migration moves only those three. `zenmode.md`, `pr-template.md`, `changelog-template.md` are re-materialized at `<repo>/.shelbi/` from shipped defaults on reload/config-upgrade; customized copies stay stranded in `~/.shelbi/projects/<name>/`.
  - Evidence: `crates/shelbi-state/src/migrate.rs:46-51`; sandbox `shelbi project migrate-to-in-repo --dry-run` plans only `move dir …/workflows`, `move dir …/agents`, `move file …/workspace-settings.json.template`.
  - Fix: add "zenmode.md, pr-template.md and changelog-template.md are not moved; copy any customized ones into `<repo>/.shelbi/` yourself." Add the three files to "What to commit". (Also an app gap — see App-side issues.)
- [severity: unclear] "Prints the ordered plan (every write, move, delete)" (line 229)
  - Reality: plan actions are write, move and retire (rename to `.yaml.migrated`); nothing is deleted.
  - Evidence: `crates/shelbi-cli/src/commands/project.rs:139-157`; dry-run prints `retire <name>.yaml → <name>.yaml.migrated`.
  - Fix: "(every write, move, and retire-rename)".
- [severity: wrong] Revert step 2 "rename the retained `<name>.yaml.migrated` copy back to `<name>.yaml`" (lines 264-271)
  - Reality: a present `local.yaml` always wins over the flat file, so the restored `<name>.yaml` is ignored until `local.yaml` is removed. `git revert` also doesn't restore `workflows/`, `agents/`, the template under `~/.shelbi/projects/<name>/` — they were moved, not copied.
  - Evidence: `crates/shelbi-state/src/lib.rs:1221-1231`; the page's own callout (lines 44-56).
  - Fix: add "3. Delete `~/.shelbi/projects/<name>/local.yaml`. 4. Move `workflows/`, `agents/` and `workspace-settings.json.template` from `<repo>/.shelbi/` back to `~/.shelbi/projects/<name>/` before reverting (or recover them from the migration commit)."
- [severity: wrong] Pick-up transcript (lines 285-300)
  - Reality: for a clone whose committed `.shelbi/` already has the template/agents/workflows, pick-up prints "(… already exists)" lines, covers six agents, writes no statuses. The `ls .shelbi/` of a freshly init'd repo also shows `changelog-template.md`, `pr-template.md`, `zenmode.md`. The `next:` hint "shelbi spawn TASK …" names a nonexistent command.
  - Evidence: sandbox pick-up output:
    ```
    ✓ scaffolded …/root2
    ✓ registered project (in-repo): …/projects/myapp/local.yaml
    (workspace settings template already exists at …/clone1/.shelbi/workspace-settings.json.template)
    (agent workspace already exists: agents/orchestrator/)
    … developer/ review/ qa/ security/ adversarial/
    ✓ picked up `myapp` from …/clone1/.shelbi/project.yaml.
    ```
    `crates/shelbi-cli/src/commands/init.rs:229`; `crates/shelbi-orchestrator/src/project_create.rs:767-795`; `shelbi spawn` → "unrecognized subcommand".
  - Fix: replace with the real transcript, add the three `.md` files to the `ls`, drop the `shelbi spawn` hint.
- [severity: unclear] Suffixed pick-up transcript (lines 316-327)
  - Reality: the CLI also prints "Tip: `shelbi project rename` can retitle the local alias to something friendlier." — a command that doesn't exist.
  - Evidence: `crates/shelbi-cli/src/commands/init.rs:843-850`; sandbox second-clone output.
  - Fix: either show the line, or (better) fix the CLI and keep the transcript as is.
- [severity: outdated] "`shelbi reload` respawns the Shelbi-owned panes (sidebar, issues, review, machines)…" (lines 342-347)
  - Reality: reload runs migrations, self-heal and the config-upgrade pass, then restarts the daemon, re-execs attached clients and replaces the orchestrator session; workers keep running. No per-pane respawns. Reload also rewrites an unset/`default` `default_workflow:` to `task`.
  - Evidence: `crates/shelbi-cli/src/commands/reload.rs:36-48, 66-78, 91-100`.
  - Fix: "`shelbi reload` re-reads the project's YAML, self-heals the default agents, workspace-settings template and workflow defaults, runs the config-upgrade pass, then restarts the daemon, re-execs attached clients and replaces the orchestrator session. Workers keep running."

### site/content/docs/concepts/config-upgrade.mdx

- [severity: wrong] "`auto_heal=<M>` is informational: those `M` fixes are already applied." (lines 55-62)
  - Reality: the summary is emitted from the post-heal residual, so `auto_heal=<M>` counts auto-heals that were *not* applied. Each applied heal gets its own line `project=<label> config-upgrade code=<CODE> surface=<logical_id> file=<path> change=<old->new>`. Summaries are per scope, only when the residual is non-empty; global surfaces report as `project=global`.
  - Evidence: `crates/shelbi-cli/src/commands/config_upgrade.rs:280-338`; `config_upgrade_apply.rs:1215-1231`. Sandbox events.log: `project=my-app config-upgrade code=PROJECT_DISPLAY_NAME_DEPRECATED surface=project.my-app.registration.shared file=… change=display_name->name`, `project=global config-upgrade code=KEYMAP_ZEN_TOGGLE_LEGACY …`.
  - Fix: "Each applied auto-heal is disclosed on its own line (`project=<id> config-upgrade code=<CODE> surface=<surface> file=<path> change=<old->new>`). When findings remain after healing, each scope (a project id, or `global`) also gets `project=<id> config-upgrade auto_heal=<M> needs_judgment=<N>`, where `M` counts auto-heals that could not be applied and `N` counts findings awaiting a decision."
- [severity: outdated] "a lifecycle-owned default a project is missing … (today the per-project pr-template.md)" (lines 38-40)
  - Reality: three: `pr-template.md` (PR_TEMPLATE_MISSING), `changelog-template.md` (CHANGELOG_TEMPLATE_MISSING), `agents/_shared/preamble.md` (SHARED_PREAMBLE_MISSING).
  - Evidence: `config_upgrade.rs:2387-2422`.
  - Fix: "(today the per-project `pr-template.md`, `changelog-template.md`, and `agents/_shared/preamble.md`)".
- [severity: missing] Surfaces "project registration YAML, statuses.yaml, workflows, the global config.yaml, per-agent settings.json, and the Zen and instructions Markdown" (lines 29-32)
  - Reality: also `agent.yaml` manifests, the workspace-settings template, per-workspace `.claude/settings.local.json`, the shared preamble, legacy sibling filenames (`statuses.yml`, `worker-settings.json*`), and existence of lifecycle template files.
  - Evidence: `config_upgrade.rs:392-444`, `:2330-2370`.
  - Fix: append "agent manifests (`agent.yaml`), the workspace-settings template and per-workspace `settings.local.json`, the shared agent preamble, and legacy-named sibling files".
- [severity: unclear] "The residual is written for the orchestrator to ingest at boot, as a single hub-global JSON file" (lines 64-66)
  - Reality: the file is a boot snapshot; the orchestrator reads the live `shelbi config upgrade --needs-judgment --format json` at bootstrap and on a `needs_judgment>0` line, never the file (its reader is test-only).
  - Evidence: `config_upgrade.rs:146-155` (`#[cfg(test)] load_findings`); `crates/shelbi-state/src/default_orchestrator.md.template:311-323`.
  - Fix: "The residual is also recorded as a single hub-global JSON file … (a snapshot of the boot pass; the orchestrator reads the live `--needs-judgment` channel instead)."
- [severity: wrong] "Once you approve one, it applies exactly that finding by its `id`, reusing the same atomic write-back the auto-heal pass uses." (lines 86-88)
  - Reality: `--apply-finding` only resolves needs-judgment findings with a mechanical fix — today only `ZEN_DANGER_PATHS_CONFLICT`. Others are refused ("has no mechanical resolution — apply its fix by editing <file> directly"), and the orchestrator edits the file itself.
  - Evidence: `crates/shelbi-cli/src/commands/config_upgrade_apply.rs:126` (`RESOLVABLE_NEEDS_JUDGMENT`), `:149-160`; orchestrator template lines 836-847.
  - Fix: "…Where a finding has a mechanical resolution (today the `danger_paths` extend+override conflict), it applies it by `id` with `shelbi config upgrade --apply-finding <id>`. Otherwise the command refuses, and the orchestrator edits the file directly and tells you what changed."
- [severity: unclear] "a `display_name:` that disagrees with `name:`" (line 43)
  - Reality: needs-judgment only when `name:` also differs from the filename id; otherwise (absent, equal to display_name, or equal to the id) it's an auto-heal.
  - Evidence: `config_upgrade.rs:1300-1345` (`sniff_display_name`).
  - Fix: "a `display_name:` that disagrees with both `name:` and the project id".
### site/content/docs/concepts/events-log.mdx

- [severity: outdated] "(or `$SHELBI_HOME/events.log` if you've set it)" (line 13)
  - Reality: root resolves `--root` → `$SHELBI_ROOT` → `$SHELBI_HOME` (legacy alias) → install-time default → `~/.shelbi`.
  - Evidence: `crates/shelbi-state/src/root.rs:4-13`; `shelbi events --help` shows `--root` / `$SHELBI_ROOT`.
  - Fix: "(or `<root>/events.log` when you've overridden the Shelbi root with `--root` / `$SHELBI_ROOT`)."
- [severity: missing] "States: `working`, `awaiting_input`, `blocked`, `paused`." (line 46); line shape (line 33) omits the reason suffix
  - Reality: `serving` is also written (review slot whose server is ready). Dialog transitions carry `reason=dialog:<kind>` / `reason=dialog:<kind>:cleared`; usage-limit pauses carry `reason=usage-limit[ reset=<hint>]`.
  - Evidence: `crates/shelbi-state/src/workspace_status.rs:57,67`; `crates/shelbi-state/src/event_log.rs:1266-1330`. Real log: `project=moduloscreensaver workspace=review none -> serving`, `reason=dialog:question`.
  - Fix: "States: `working`, `awaiting_input`, `blocked`, `paused`, `serving` (a review slot whose server is up). Blocking-dialog and usage-limit transitions append `reason=dialog:<kind>[:cleared]` or `reason=usage-limit [reset=<hint>]`."
- [severity: outdated] Task-line shape and examples (lines 55, 59-61, 179-183) and reason row "`orchestrator:auto-dispatch workspace=<name>`" (line 205)
  - Reality: dispatch reasons get the agent appended (`reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer`, `user:cli:start_agent=<agent>`). Task lines can end with optional `owner=me|other:<login>|unassigned|unknown` (remote trackers), `actions=skipped` (`--skip-transition-actions`), and `pr=<n> sha=<oid>` (GitHub merge reconcile).
  - Evidence: `crates/shelbi-orchestrator/src/mutate/start.rs:314,638-639`; `event_log.rs:1553-1565` (`with_owner_token`), `1572-1599`, `1633-1656`. Real line: `… todo -> in_progress reason=orchestrator:auto-dispatch_workspace=hotel_agent=developer from_category=ready to_category=active owner=me`.
  - Fix: examples → `reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer`; table row → `orchestrator:auto-dispatch_workspace=<name>_agent=<agent>` and `user:cli:start_agent=<agent>`; add "Optional trailing tokens: `owner=…` (GitHub-backed boards), `actions=skipped`, and `pr=<n> sha=<oid>` on `poller:github-merge-reconcile` lines."
- [severity: wrong] "`<from-column>` and `<to-column>` are the snake_case status names from the issue's workflow" (lines 68-70)
  - Reality: status ids are written as-is; only stock `in-progress` is written `in_progress` (legacy wire form). Custom ids like `review-app` keep hyphens.
  - Evidence: `crates/shelbi-core/src/model.rs:2053-2063` (`wire_str`); test at `event_log.rs:4000` asserts `review -> review-app`.
  - Fix: "are the workflow's status ids, written as-is, except that the stock `in-progress` is written `in_progress` (default workflow: `backlog`, `todo`, `in_progress`, `review`, `done`)."
- [severity: missing] Heartbeat line shape (lines 82-91)
  - Reality: when disk under the hub `work_dir` is low the line carries `disk_free_gib=<n> disk_low=true`. Zen reminders after ` — ` are the first line of `zenmode.md` (every 3rd Zen heartbeat) or `re-read zenmode.md now to refresh Zen policy` (hourly).
  - Evidence: `event_log.rs:2131-2177`; `crates/shelbi-orchestrator/src/poller.rs:622,627,1386-1436`.
  - Fix: add "When disk is low, `disk_free_gib=<n> disk_low=true` is appended. While Zen is on, every third heartbeat ends with ` — <first line of zenmode.md>`, and about hourly with ` — re-read zenmode.md now to refresh Zen policy`."
- [severity: wrong] "any genuine line … both skips that tick's emission … and snaps the cadence back to the standard `interval`. An active board never sees padding lines." (lines 104-109)
  - Reality: the skip is bounded by `window_start + standard`; a continuously active board still gets a heartbeat at least once per standard interval (added after a busy hub went 30+ hours without one).
  - Evidence: `poller.rs:1175-1183, 1248-1271`; the real log shows heartbeats interleaved with dispatch/merge lines.
  - Fix: "…skips that tick's emission and snaps the cadence back to `interval`, but only within the current window: a heartbeat still lands at least once per `interval` even on a continuously active board."
- [severity: missing] `shelbi events tail` options (lines 158-170)
  - Reality: undocumented `--project <name>` (keeps only that project's lines; `-n`/`--since` count within it — this is what the orchestrator watches), `--format raw|envelope` (JSON envelope), `-f` short for `--follow`. `tail` reads only the live `events.log`, never `events.log.1`.
  - Evidence: `shelbi events tail --help`; `crates/shelbi-cli/src/commands/events.rs:18-46, 84-118`; `crates/shelbi-orchestrator/src/lib.rs:709`.
  - Fix: add `shelbi events tail --follow --project myapp   # only this project's lines (what the orchestrator watches)` and `shelbi events tail --format envelope   # JSON envelopes`; note rotated lines in `events.log.1` aren't read.
- [severity: outdated] "Filtering by project … is the orchestrator's job" (lines 17-20); "one stream the orchestrator tails" (frontmatter, line 4); "Heartbeats exist for the orchestrator's `events tail --follow` watch" (line 118)
  - Reality: the orchestrator consumes events through a durable per-project claim/ack cursor (`shelbi orchestrator events next|ack`); `events tail --follow --project` is a non-consuming accelerator with built-in project filtering. Heartbeats also serve as the stream's liveness signal.
  - Evidence: `shelbi orchestrator events --help`; `event_log.rs:1-50`; `crates/shelbi-orchestrator/src/lib.rs:695-722`; `crates/shelbi-core/src/model.rs:404-410`.
  - Fix: say the orchestrator consumes the log via `shelbi orchestrator events next` (durable, acknowledged cursor) and mirrors it with `shelbi events tail --follow --project <name>`; line 118 → "Heartbeats are the orchestrator's fallback wake-up and the stream's liveness signal."
- [severity: outdated] "Append-only. Never truncate; if you need to rotate, do it atomically via rename." (lines 228-229)
  - Reality: Shelbi rotates itself at 8 MiB to `events.log.1` (one generation) under `events.log.lock`, tracking a monotonic logical offset in `events.log.index.json` (plus `.rotation.json`/`.migration.json` for crash recovery). Outside consumers must not rotate it.
  - Evidence: `event_log.rs:7-35, 3133-3186`; `EVENTS_LOG_MAX_BYTES = 8 * 1024 * 1024` (`event_log.rs:3228`); `~/.shelbi/events.log.1` and `events.log.index.json` exist on this machine.
  - Fix: "Append-only. Shelbi rotates the file itself at 8 MiB to `events.log.1` (one older generation kept) and tracks offsets in `events.log.index.json`. Don't truncate or rotate it yourself; read `events.log.1` too for older history."
- [severity: wrong] "`user:tui:…` | the Kanban TUI | You moved the card with `H`/`L` in the TUI." (line 203)
  - Reality: the TUI writes exactly `user:tui`.
  - Evidence: `crates/shelbi-tui/src/kanban.rs:3240` (`reason: "user:tui"`); real log `reason=user:tui`.
  - Fix: `user:tui:…` → `user:tui`.
- [severity: missing] Reason table (lines 199-206)
  - Reality: missing common reasons: `user:app` (desktop board move), `user:review` / `user:review-reject` (review panel), `user:cli:add`, `user:cli:resume`, `poller:issue-reconcile`, `poller:issue-adopt`, `poller:github-merge-reconcile`, `orchestrator:zen-promote_category=<n>`, `orchestrator:zen-merge_pr=<n>`.
  - Evidence: `crates/shelbi-app/src/exec_daemon.rs:128`; `crates/shelbi-orchestrator/src/review_ui.rs:363-370,472`; `crates/shelbi-orchestrator/src/mutate/add_edit.rs:98`; `crates/shelbi-cli/src/commands/issue.rs:1228`; `event_log.rs:198-238`; reason counts in `~/.shelbi/events.log`.
  - Fix: add a row per reason with its source and meaning.
- [severity: missing] Zen and CI line shapes (zen-mode.mdx's See also, lines 821-822, says this page documents "the canonical format of the `mode=zen` and `orchestrator:zen-*` lines"; it doesn't)
  - Reality: `<ts> project=<p> mode=zen <prev> -> <new> reason=<source>` (mode `off|paused|on`; source `user:cli`, `user:palette`, legacy `user:hotkey`); legacy `<ts> project=<p> zen=off reason=crash-recovery`; `<ts> zen-dryrun task=<id> action=<a> detail=<d>`; `<ts> project=<p> task=<id> ci pr=<n> check=<c> conclusion=<x> head_sha=<sha>`.
  - Evidence: `event_log.rs:2065-2092, 1686-1703`; `crates/shelbi-state/src/lib.rs:1720-1722, 2809-2820`; real lines `project=shelbi mode=zen off -> on reason=user:palette`, `… ci pr=2012 check=all conclusion=success head_sha=…`.
  - Fix: add a "### Zen and CI lines" subsection with these shapes (or change the zen-mode See-also text).

### site/content/docs/concepts/github-issues.mdx

- [severity: wrong] "between 100 and 500 the index refresh pauses until the quota resets (while user-initiated single fetches still run); below 100, reads are served from cache with a warning." (lines 305-309)
  - Reality: three bands only: >2,000 → `refresh_secs`; 500 < r ≤ 2,000 → 120s; ≤500 → pause until reset (or base cadence if the reset has passed). A 403/429 park pauses regardless. `budget.graphql_low` (default 100) is parsed but read by nothing.
  - Evidence: `crates/shelbi-state/src/gh_budget.rs:719-748` (`tick_plan`); `grep -rn graphql_low crates` → only `crates/shelbi-core/src/model.rs`.
  - Fix: "above 2,000 points it runs at the configured cadence; between 500 and 2,000 it slows to a 120s cadence; at 500 or below the index refresh pauses until the quota resets, then resumes. Single-issue fetches are REST conditional GETs and aren't governed by this GraphQL budget."
- [severity: missing] Section "GitHub assignees and Zen Mode" (lines 210-238), esp. "Zen only auto-promotes backlog issues assigned to you" (line 223)
  - Reality: the assignee gate covers every start, not just Zen: `shelbi issue start`, `issue resume`, the daemon's active-gate dispatch and review auto-load all refuse an issue not assigned to you, pointing at `shelbi issue start <id> --take` (assigns on GitHub, then starts). The orchestrator never passes `--take`. Task lines carry `owner=me|other:<login>|unassigned|unknown`; `shelbi issue list` shows an owner column.
  - Evidence: `crates/shelbi-state/src/ownership.rs:1-21, 147-185, 228-245`; `shelbi issue start --help` (`--take`); `crates/shelbi-state/src/default_orchestrator.md.template:459-468`.
  - Fix: retitle "GitHub assignees and ownership"; add "**Shelbi only starts issues assigned to you.** On a GitHub board, `shelbi issue start`/`resume` and every automatic dispatch (orchestrator, active-gate dispatch, review auto-load) refuse an issue whose assignees don't include you. Claim one with `shelbi issue start <id> --take` or by assigning yourself on github.com. The orchestrator never passes `--take`." Mention the `owner=` token and the `issue list` owner column.
- [severity: outdated] "(`backlog` for the shipped `app` and `default` workflows)" (line 178)
  - Reality: no shipped `app` workflow; shipped are `default` (fallback), `task`, `subtask`, all entering at `backlog`.
  - Evidence: `crates/shelbi-core/src/model.rs:2458-2470`; `crates/shelbi-core/src/workflow.rs:849-903, 931-991, 1046-1095`.
  - Fix: "(`backlog` for the shipped `task`, `subtask`, and fallback `default` workflows)".
- [severity: unclear] "A **closed** issue always reads as a terminal card (`done`, or `canceled` when its reason is `not_planned`)…" (lines 162-165)
  - Reality: a closed issue's terminal `shelbi:status/*` label wins; `state_reason` is used only when the label is missing or non-terminal (a done→canceled move keeps `completed` and the label says `canceled`).
  - Evidence: `crates/shelbi-state/src/github_store.rs:4844-4869` (`GhIssue::column`), `1358-1366`.
  - Fix: "A **closed** issue always reads as terminal: its terminal status label if it has one, otherwise `done` (or `canceled` when GitHub's reason is `not_planned`)…"
- [severity: unclear] "status labels get distinct colors; everything else gets GitHub's default grey" (lines 190-191)
  - Reality: only the six stock status labels get colors; custom status labels and id labels get `ededed`. The first write creates the whole stock status set.
  - Evidence: `github_store.rs:5085-5098` (`label_color`), `2846-2855` (`bootstrap_labels`).
  - Fix: "the six stock status labels get distinct colors; custom status and id labels get GitHub's default grey. The first write also creates the full stock `shelbi:status/*` set."
- [severity: missing] No mention of `shelbi doctor`
  - Reality: `shelbi doctor` reports observed GitHub request rate, remaining budget, projected time-to-exhaustion and top callers.
  - Evidence: `shelbi --help` ("doctor  Runtime health checks for the project's GitHub API budget…"); `crates/shelbi-cli/src/commands/doctor.rs`.
  - Fix: under "The budget, and what `shelbi status` shows": "`shelbi doctor` adds the observed request rate, a projected time to run out, and a warning naming the top callers when a budget would run out within 30 minutes."
- [severity: missing] Migration section (lines 378-381)
  - Reality: comments aren't migrated and `assigned_to` is dropped.
  - Evidence: `crates/shelbi-state/src/issue_migrate.rs:24-31`.
  - Fix: append "Comments are not migrated, and workspace assignments are dropped."

### site/content/docs/concepts/orchestrator.mdx

- [severity: wrong] "Start its live watch. Launch `shelbi orchestrator events next --follow` in the background — the durable claim/ack feed is the orchestrator's primary watch (`shelbi events tail` is the raw view for humans)." (lines 154-157)
  - Reality: reversed. The background watch is a non-consuming `shelbi events tail --follow --project <name>` (one Monitor). The source of truth is a bounded self-drain `shelbi orchestrator events next --follow --max-lifetime 2s` + `shelbi orchestrator events ack <delivery-id>` before every reply, every heartbeat and whenever the tail shows a line. The prompt forbids a background `events next --follow` (two consumers race). Bootstrap also runs `shelbi config upgrade --needs-judgment --format json`.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:214-325, 384-430`; `crates/shelbi-orchestrator/src/lib.rs:691-723`; `shelbi orchestrator events next --help` ("This feed is a latency accelerator; the source of truth … is a consumer's own periodic claim/ack drain").
  - Fix: "3. **Start a passive watch.** Run `shelbi events tail --follow --project <name>` in the background; it mirrors the log without claiming events. 4. **Self-drain on every turn.** Before each reply and on every heartbeat, run `shelbi orchestrator events next --follow --max-lifetime 2s`, react, then `shelbi orchestrator events ack <delivery-id>`. Bootstrap also reads `shelbi config upgrade --needs-judgment --format json`." Update "three things" (line 143).
- [severity: wrong] "If the handoff status requires review tags, also route the finished branch onto a matching workspace — firing the status's enter transition to boot a server (or leave it queued when they're all busy)" (lines 175-179)
  - Reality: the orchestrator never loads review workspaces ("Leave the review load to the human"); the hub's review auto-loader claims an idle `review`-tagged slot each supervisor tick, or a human loads from the sidebar.
  - Evidence: `default_orchestrator.md.template:70-72, 511-520, 552-555, 645-647`; `crates/shelbi-orchestrator/src/poller.rs:352-360`; `crates/shelbi-orchestrator/src/load.rs:506-522` (`autoload_review_queue`).
  - Fix: "If the handoff status requires review tags, Shelbi's hub loads the finished branch onto a free matching review workspace on its own (or queues it until one frees; you can also load it from the sidebar). The orchestrator doesn't drive review loads."
- [severity: wrong] See also: "Review workspaces — the tag-routed slots the orchestrator loads a finished branch onto on handoff, and the queue it drains as they free." (lines 316-318)
  - Reality/Evidence: as above.
  - Fix: "Review workspaces — the tag-routed slots the hub loads a finished branch onto on handoff."
- [severity: outdated] "3. Launches the orchestrator runner in the right pane of the `dashboard` window, with that path as its working directory." (lines 95-96); frontmatter "One agent in window 1 you talk to" (line 4)
  - Reality: no tmux dashboard/pane. The orchestrator is a detached session process `shelbi-<project>` owning its PTY; its working dir is the state dir `~/.shelbi/projects/<name>/` (where `.claude/agent-instructions.md` is staged). Clients display that session.
  - Evidence: `crates/shelbi-orchestrator/src/lib.rs:589-623` ("runs as a detached session process (owning its own PTY, no tmux pane)", `workdir = shelbi_state::project_dir(...)`), `1007-1053`.
  - Fix: step 3 → "Starts the orchestrator runner as the project's `shelbi-<name>` session, with the project state directory (`~/.shelbi/projects/<name>/`) as its working directory." Summary → "One agent you talk to: it dispatches issues…".
- [severity: wrong] "The policy that decides "in scope" lives in the orchestrator agent's `instructions.md` described above." (lines 305-306)
  - Reality: Zen auto-promote categories and merge conditions live in `zenmode.md` in the config root (`~/.shelbi/projects/<name>/zenmode.md` or `<repo>/.shelbi/zenmode.md`); the orchestrator prompt only points at it. Zen never auto-merges an issue routed to a review workspace.
  - Evidence: `default_orchestrator.md.template:959-980`; `crates/shelbi-state/src/zenmode.rs:1-30`.
  - Fix: "The policy that decides "in scope", and the merge bar, live in the project's `zenmode.md` (`<config-root>/zenmode.md`). You tune it by editing prose, not code. Issues routed to a review workspace always wait for a human."
- [severity: outdated] "The polling-only event drain. Codex-backed and other polling-only runners must drain pending project events before each user-facing reply, using `shelbi orchestrator events drain` … or the documented cursor-based `events.log` fallback" (lines 249-253)
  - Reality: the section is now the source of truth for every orchestrator: `events next --follow --max-lifetime 2s` + `events ack`. `events drain` is legacy (still in the Codex standalone fallback prompt).
  - Evidence: `default_orchestrator.md.template:384-430` ("> Legacy: `shelbi orchestrator events drain` still exists…"); `crates/shelbi-orchestrator/src/lib.rs:871-887`.
  - Fix: "Every orchestrator drains pending events before each user-facing reply with `shelbi orchestrator events next --follow --max-lifetime 2s` and acks each batch with `shelbi orchestrator events ack <delivery-id>` (cursor in `~/.shelbi/projects/<name>/event-cursor`). The legacy `events drain` and a cursor-based `events.log` read remain as fallbacks."
- [severity: outdated] "The `developer` agent (and any reviewer roles you author) run inside workspaces" (line 16); "instead of pasting it into four `instructions.md` files" (line 223)
  - Reality: six shipped agents (orchestrator, developer, review, qa, security, adversarial); reviewer roles ship.
  - Evidence: `crates/shelbi-state/src/agent_workspaces.rs:49-72, 228`.
  - Fix: line 16 → "The `developer`, `review`, `qa`, `security`, and `adversarial` agents (and any you author) run inside workspaces"; line 223 → "instead of pasting it into every agent's `instructions.md`".

### site/content/docs/concepts/remote-sessions.mdx

- [severity: wrong] "**Change Host** goes back to step 1." (line 31)
  - Reality: the button label is "Change host".
  - Evidence: `crates/shelbi-app/src/launch/remote.rs:27` (`CHANGE_HOST_LABEL: "Change host"`).
  - Fix: "**Change host** goes back to step 1."
- [severity: missing] "When the connection drops" (lines 49-55)
  - Reality: Esc (or Ctrl+C in the TUI) stops retrying ("Stopped reconnecting to <host>"). A reconnect never starts the far daemon; if it has stopped the loop ends with "Shelbi isn't running on <host>." offering Start Shelbi.
  - Evidence: `crates/shelbi-tui/src/shell/remote_session.rs:1-21` (`STOP_HINT`); `crates/shelbi-app/src/launch/terminal.rs:8-11, 100`.
  - Fix: append "Press Esc to stop reconnecting. A reconnect never starts the other machine's Shelbi; if it has stopped, you're offered **Start Shelbi** instead."
### site/content/docs/concepts/review-workspaces.mdx

- [severity: wrong] "Nothing branches on the literal word "review"; it is just the tag this workflow happens to require." (lines 47-48)
  - Reality: routing itself is the generic superset query, but the literal `review` tag is special-cased: `shelbi issue start` refuses a review-tagged slot without `--force`, the sidebar hides review slots from `— Workspaces —`, they're excluded from dev capacity, and only they get the review auto-load queue and the `review:` recipe.
  - Evidence: `crates/shelbi-orchestrator/src/mutate.rs:642-650`; `crates/shelbi-orchestrator/src/sidebar_model.rs:205`; `crates/shelbi-state/src/lib.rs:3411-3427`; `crates/shelbi-orchestrator/src/workspace.rs:6201`; `crates/shelbi-orchestrator/src/load.rs:557-571`.
  - Fix: "Routing itself is the generic superset query, but `review` is also the tag Shelbi's review machinery keys on: review-tagged slots refuse direct dev dispatch, sit in the sidebar's review section, are fed by the review auto-load queue, and receive the workflow's `review:` recipe."
- [severity: missing] "Shelbi resolves the recipe's `$SLOT` / `$PORT` placeholders against the review slot's port" (lines 56-58) and the `$SLOT`/`$PORT` section (lines 100-105)
  - Reality: the "port" is the review workspace's explicit `slot:` value. With no `slot:` (as on the `review` slot `shelbi init` creates) the placeholders stay unresolved and the Review agent stops and reports the misconfiguration. The index fallback applies only to transition `$SLOT`.
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:519, 6204` (`workspace.slot.and_then(|s| u16::try_from(s).ok())`); `crates/shelbi-orchestrator/src/review_session.rs:194-206`; `crates/shelbi-state/src/default_review.md.template` ("Recipe present but the port is unresolved"); `crates/shelbi-orchestrator/src/project_create.rs:611-615`.
  - Fix: add "The port is the review workspace's explicit `slot:` value (e.g. `slot: 3000`). A review slot without `slot:` leaves `$SLOT`/`$PORT` unresolved and the Review agent stops and reports it, so give every review slot a port-valued `slot:`."
- [severity: wrong] "Complete review column" example tags machine `hub` with `tags: [review]` and labels `alpha`/`bravo` "# dev slot" (lines 141-150)
  - Reality: machine tags are inherited, so `alpha`/`bravo` become review slots too (no dev dispatch, hidden from `— Workspaces —`, in the review pool).
  - Evidence: `crates/shelbi-core/src/model.rs:1252-1258` (effective-tags union); `crates/shelbi-state/src/lib.rs:3411-3427`; `crates/shelbi-orchestrator/src/mutate.rs:642`.
  - Fix: remove `tags: [review]` from the machine; keep it only on `review-0`.
- [severity: wrong] "  - from: review, to: done, actions: [merge, delete_branch]" (line 162)
  - Reality: invalid YAML (block mapping with comma-separated pairs).
  - Evidence: `ruby -ryaml` parse → "mapping values are not allowed in this context at line 2 column 21".
  - Fix: `  - { from: review, to: done, actions: [merge, delete_branch] }`
- [severity: missing] "**Ready for Review** (`✓`, cyan) — the issue's `assigned_to` names a `review`-tagged workspace, so the branch is loaded on a review slot and serving." (lines 202-206)
  - Reality: two row states: `✓` Serving (bold cyan, `machine:workspace` badge) only once confirmed serving (the `.shelbi/shelbi-review-loaded` marker names the task, written when the `ready:` probe passes, or a URL-less review's agent session is live); `▶` Loading (muted, no badge) while assigned but not confirmed. Line 2 is `⎇ <branch>`.
  - Evidence: `crates/shelbi-orchestrator/src/sidebar_model.rs:313-343`; `crates/shelbi-app/src/view.rs:187-226`; `crates/shelbi-tui/src/shell/sidebar.rs:956-990`.
  - Fix: add "**`▶` (muted)** — loaded on a review slot but the server isn't confirmed up yet; no location badge." Reword ✓ as "assigned to a review-tagged slot **and** confirmed serving". Show `⎇ jlong/dark-mode-toggle-polish` in the example.
- [severity: outdated] "selecting a **Queued** row raises a "Load onto a review workspace?" prompt … — useful for loading a queued issue while the slots are otherwise busy." (lines 218-221)
  - Reality: the dialog is "Load for review": one free slot → "Load onto review workspace `<name>`?" [Load]/[Cancel]; several → a picker; none free → "Every review slot is busy — free one to load this task."; none configured → "No review workspace is configured." It can't load while all slots are busy.
  - Evidence: `crates/shelbi-app/src/overlay/review_confirm.rs:1-26`; `crates/shelbi-tui/src/shell/mod.rs:3959-3988`; `crates/shelbi-orchestrator/src/load.rs:557-582`.
  - Fix: "selecting a **Queued** row opens **Load for review**: with one free review slot it asks "Load onto review workspace `<name>`?", with several it lets you pick, and if every review slot is busy it says so. Use it for issues auto-load skips (a status not tagged `review`, a teammate's issue, or one you unloaded with `shelbi workspace stop`)."
- [severity: outdated] "The panel is the review window's own navigation, so the global nav sidebar stays docked in the dashboard." (lines 240-241); "switches focus back to the dashboard window" (lines 243-245)
  - Reality: no separate review/dashboard window. The review panel takes the sidebar column; Back (or Esc) restores the nav sidebar and previous view and focuses the sidebar, leaving the review loaded.
  - Evidence: `crates/shelbi-tui/src/shell/mod.rs:4157-4168, 4880, 6829`; `crates/shelbi-tui/src/shell/review.rs:334`.
  - Fix: "The panel takes the sidebar's column while the review is open." Back → "returns to the normal sidebar and your previous view (Esc does the same), leaving the review loaded so you can reopen it from the sidebar."
- [severity: wrong] "A **header** with the review status (`Ready for review`) and the review worktree's folder (`📂`)" (lines 246-248)
  - Reality: folder glyph is `📁`; between header and folder there's a task-info block (title, 3-line description preview, **More** link on `m`); a server status line shows the sub-state; on a remote review slot View Diff and Edit in `<editor>` aren't offered.
  - Evidence: `crates/shelbi-tui/src/panel.rs:76` (`FOLDER_PREFIX: "📁 "`); `crates/shelbi-tui/src/review_panel.rs:1-22, 401-411`; `crates/shelbi-app/src/review.rs:160-175`.
  - Fix: `📂` → `📁`; add "task info (title, a short description preview and a **More** link)"; add "On a remote review slot only Reviewer and Open Browser are offered."

### site/content/docs/concepts/workspaces.mdx

- [severity: outdated] "A **workspace** is a machine + pane + worktree." (line 35)
  - Reality: no tmux panes; a workspace is a machine + Shelbi session (detached `shelbi __session`) + worktree (the page's own line 14 says so).
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs` (`deploy_and_spawn`, steps "4 + 5").
  - Fix: "A **workspace** is a machine + Shelbi session + worktree."
- [severity: outdated] "The wizard sizes the pool from total RAM (~10 GB per local workspace, ~12 GB when spread across machines, clamped to `[1, 16]`)." (line 71)
  - Reality: init, wizard and Add Project all write a fixed starter pool on `hub`: `dev` (no tags) and `review` (`tags: [review]`). `recommended_workspace_count` has no non-test callers.
  - Evidence: `crates/shelbi-orchestrator/src/project_create.rs:603-616`; `crates/shelbi-core/src/system_memory.rs:97`; `crates/shelbi-cli/src/commands/init.rs:961-968` (test asserts 2 workspaces).
  - Fix: "A new project starts with two workspaces on `hub`: `dev` and `review` (tagged `review`)."
- [severity: outdated] "Add or remove workspaces later by editing the YAML and running `shelbi reload`." (lines 72-73)
  - Reality: `shelbi workspace add` / `shelbi workspace rm` (refuses an active task or unpushed work without `--force`) and Settings ▸ Workspaces (TUI and desktop) do this.
  - Evidence: `shelbi workspace --help` (add/rm/list/stop/status); `crates/shelbi-app/src/settings.rs:95`; `crates/shelbi-tui/src/shell/settings/workspaces.rs`.
  - Fix: "Add or remove workspaces later with `shelbi workspace add <name>` / `shelbi workspace rm <name>`, or in Settings ▸ Workspaces." (unverified: whether a hand edit still needs `reload`.)
- [severity: wrong] "Nothing in the routing branches on a literal tag name — it's the same superset query every tag uses." (lines 119-121)
  - Reality/Evidence: as the review-workspaces finding above (`mutate.rs:642-650`, `sidebar_model.rs:205`, `lib.rs:3411-3427`, `workspace.rs:6201`, `load.rs:557-571`).
  - Fix: "The routing query itself is generic. The `review` tag also carries built-in meaning: review-tagged slots refuse direct dev dispatch, are hidden from the sidebar's Workspaces section, are fed by the review auto-load queue, and receive the workflow's `review:` recipe."
- [severity: unclear] Tags example puts `tags: [review]` on machine `hub` (lines 88-98)
  - Reality: union semantics are right, but a machine-level `review` tag makes every slot on it a review-only slot.
  - Evidence: as above; `crates/shelbi-core/src/model.rs:1252-1258`.
  - Fix: use a neutral machine tag (e.g. `gpu`), or add "Note: a machine-level `review` tag makes every slot on that machine a review slot."
- [severity: unclear] "`{ name: review-0, machine: hub, tags: [review], slot: 0 }` / `review-1 … slot: 1`" (lines 133-137)
  - Reality: for `review:` recipes `$SLOT`/`$PORT` are the explicit `slot:` value, so these would serve on ports 0 and 1; a review slot without `slot:` leaves them unresolved.
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:6204`; `crates/shelbi-core/src/workflow.rs:1933-1944`; `default_review.md.template`.
  - Fix: use dev slots for this `$SLOT` example, or port-valued slots (`slot: 3000` / `slot: 3010`), and add "For `review:` recipes the slot value *is* the port, and review slots need an explicit `slot:`."
- [severity: wrong] "Each row shows the workspace's state badge, its name, the agent currently loaded (if any), and the issue it's on" + example with `— hub —` headers and issue slugs (lines 144-159)
  - Reality: row = badge, name, right-aligned title-cased agent (`Developer`) or `idle`; no issue. Machine groups only appear with >1 machine; headers render `▾ hub` / `▸ hub`; the only `— … —` header is `— Workspaces —`.
  - Evidence: `crates/shelbi-app/src/view.rs:224-230, 536-563`; `crates/shelbi-tui/src/shell/sidebar.rs:893-897, 910-923`.
  - Fix: replace the example with
    ```
     — Workspaces —
     ▾ hub
       ⏵ alpha                Developer
       ? bravo                Developer
       · charlie                   idle
     ▾ devbox
       ⏵ delta                       Qa
       · echo                      idle
    ```
    and say "Each row shows the state badge, the name, and the loaded agent (or `idle`)."
- [severity: wrong] "A collapsed machine keeps its `(total, active)` count on the header" (lines 170-171)
  - Reality: renders as `(4, 2 active)`, only while collapsed.
  - Evidence: `crates/shelbi-tui/src/shell/sidebar.rs:917-919` (`format!("({total}, {active} active)")`).
  - Fix: "A collapsed machine shows a `(total, N active)` count on its header."
- [severity: wrong] "The orchestrator honors the hint when at least one workspace on that machine is free … Shelbi never silently re-routes … The hint … is surfaced on the kanban card." (lines 185-190)
  - Reality: no Rust code routes on or displays `prefers_machine` (every TUI/app/desktop use sets it to `None`); it's honored only as orchestrator-prompt policy and isn't shown on cards.
  - Evidence: `grep -rn prefers_machine crates desktop/src` → only model/storage (`issue_store.rs`, `github_store.rs`), the CLI flags (`crates/shelbi-cli/src/commands/issue.rs:201,279,283`), and `default_orchestrator.md.template:445, 683-685`; `shelbi issue add --help` ("the orchestrator decides whether to honor it").
  - Fix: "The orchestrator agent is instructed to honor the hint: it considers only workspaces on that machine and leaves the issue in its ready status when none are free. The hint is stored as `prefers_machine`; clear it with `shelbi issue edit --no-prefers-machine`." Drop "surfaced on the kanban card."
- [severity: wrong] "| `💬` | `awaiting_input` |" (line 201) and `💬` in the example (line 151)
  - Reality: the awaiting-input badge is `?` (yellow). The table also omits `serving` (review slot whose `ready:` probe passed; shown `⏵`) and the `…` loading skeleton badge.
  - Evidence: `crates/shelbi-app/src/view.rs:157-166`; `crates/shelbi-state/src/workspace_status.rs:38-68`; `crates/shelbi-orchestrator/src/sidebar_model.rs:254`.
  - Fix: `💬` → `?`; add "| `⏵` | `serving` | a review slot whose branch passed the `review:` ready probe |" and "| `…` | — | status not read yet |".
- [severity: outdated] "its pane is **killed and re-created** … 3. The pane (window for hub workspaces, session for remote ones) is torn down. 4. A new pane is created …" (lines 230-246)
  - Reality: every workspace (local or remote) is a detached `shelbi __session`; dispatch ends the agent session and spawns a new one (over SSH for remote).
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs` `deploy_and_spawn` ("3. Reset the agent session … `kill_workspace_pane`"; "4 + 5. Spawn the agent session … detached `shelbi __session` … Remote: … started over SSH").
  - Fix: "its session is **ended and re-created**"; step 3 → "The workspace's agent session is ended."; step 4 → "A new Shelbi session is started (on the remote machine for remote workspaces) and the agent CLI is launched in it …"
- [severity: outdated] "5. Once the agent's input box is ready (`shift+tab to cycle` footer detected), the initial prompt is typed." (lines 247-248)
  - Reality: the startup prompt is passed at launch (Claude/Codex positional arg, aider `--message-file`, others stdin); Shelbi then waits for a busy signal. A readiness-gated typed paste is a Claude-only fallback.
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:3186-3215`; `crates/shelbi-core/src/model.rs` `AgentRunnerSpec.prompt_injection` doc.
  - Fix: "5. The startup prompt (the agent's charter plus the issue context) is passed to the agent at launch. Shelbi then confirms the agent went busy, falling back to typing the prompt once the input box is ready."
- [severity: missing] "2. Moves the issue to the next `handoff` status" (lines 281-282)
  - Reality: the poller first verifies the worktree and rebases onto the base branch; a forward edge to an agent-owned active gate (e.g. `in-progress → adversarial-review`) wins; with no handoff status it follows the active status's merge edge.
  - Evidence: `crates/shelbi-orchestrator/src/poller.rs:2769-2795, 2862-2890`.
  - Fix: "2. Rebases the branch onto the base branch and moves the issue forward: to an agent-owned gate status if the workflow declares one, otherwise to the first `handoff` status (`review` in the default workflow)."
- [severity: unclear] "The agent never runs `shelbi` itself. The marker file is the *entire* on-workspace protocol." (lines 287-288)
  - Reality: there's also an agent-transition marker (bounces/verdicts), clarification requests to `$SHELBI_HUB_SOCK`, Claude hook scripts under `.shelbi/hooks/`, and `shelbi __review-serve` on review slots.
  - Evidence: `crates/shelbi-state/src/default_developer.md.template:187-190`; `crates/shelbi-orchestrator/src/workspace.rs:6268, ~6022-6026`; `crates/shelbi-state/src/default_workspace_settings.json.template`.
  - Fix: "Completion is signalled only by the marker file; the agent doesn't call `shelbi` to finish. (Agents also report state via Claude hooks and can request clarification through the hub socket.)"
- [severity: outdated] "[`shelbi workspace`](/docs/cli/workspace) — list and stop workspaces." (line 334)
  - Reality: subcommands are `add`, `rm`, `list`, `stop`, `status`.
  - Evidence: `shelbi workspace --help`.
  - Fix: "— add, remove, list, inspect, and stop workspaces."

### site/content/docs/concepts/zen-mode.mdx

- [severity: wrong] "If the orchestrator pane dies mid-session … Zen Mode auto-disables on the next orchestrator start. The wrapper script … writes a heartbeat every 60s; … writes a `zen=off reason=crash-recovery` event line, and prints a stderr warning" (lines 68-78)
  - Reality: nothing invokes this anymore. The `__zen-orch-start` / `__zen-heartbeat` / `__zen-orch-exit` hidden commands exist but have no caller in the session model, so a crash leaves Zen on; the supervisor relaunches the orchestrator without touching `zen_mode`.
  - Evidence: `crates/shelbi-orchestrator/src/lib.rs:951-953` ("the zen heartbeat / signal traps are tmux-pane lifecycle machinery; in the session model they … are not reproduced here"); callers only at `crates/shelbi-cli/src/main.rs:364-387`; `crates/shelbi-cli/src/commands/zen.rs:15-18` ("the (future) crash-recovery path"); live `~/.shelbi/projects/shelbi/state.json` has no `zen_orchestrator_alive_at`; last `zen=off reason=crash-recovery` line in `~/.shelbi/events.log*` is 2026-09-04.
  - Fix: replace the callout with "Zen Mode is not currently auto-disabled when the orchestrator crashes. The daemon relaunches a crashed orchestrator session and Zen stays in its current mode; run `shelbi zen off` if you want it off." Drop or qualify "The orchestrator's bootstrap also scans recent event lines for this pattern" (lines 81-82) as legacy-only.
- [severity: outdated] "First-run hotkey probe … centered overlay … waits a few seconds for `Alt+Z` … a chooser pops up with three fallback chords … persisted to `~/.shelbi/config.yaml` (a later one-shot migration copies it into `~/.shelbi/keys.yaml`)" (lines 32-48)
  - Reality: the probe was deleted (`crates/shelbi-tui/src/zen_probe.rs` removed in a42a8f0a, #1505). The chord is an ordinary keybinding `global.zen_toggle` (default `alt-z`) in `~/.shelbi/keys.yaml` or Settings ▸ Keybindings; `config.yaml::keymap.zen_toggle` is only read for migration.
  - Evidence: `git show --stat a42a8f0a` (`zen_probe.rs | 442 ---`); `crates/shelbi-state/src/keymap/actions.rs:263`; `crates/shelbi-state/src/keymap/loader.rs:1090-1104`; `desktop/src/menus/tree.rs:966`.
  - Fix: "The toggle is bound to `Alt+Z` (⌥Z in the desktop app) by default. If your terminal swallows Option/Alt (e.g. Terminal.app), rebind it in Settings ▸ Keybindings or in `~/.shelbi/keys.yaml` (`defaults: { global: { zen_toggle: ctrl-g } }`). You can always use `shelbi zen on|off` or the palette."
- [severity: outdated] "the sidebar shows a green **ZEN ON** pill anchored to the bottom of the workspaces column" (lines 27-30)
  - Reality: TUI: full-width green `ZEN MODE ON` band on the sidebar's bottom row, under the footer toolbar; desktop: green "Zen mode on" band. Hidden while off or paused.
  - Evidence: `crates/shelbi-tui/src/shell/sidebar.rs:17-18, 737-749` (`let label = "ZEN MODE ON"`); `desktop/src/views/sidebar.rs:1618,1655`.
  - Fix: "When Zen is **on**, the sidebar shows a green **ZEN MODE ON** band on its bottom row, below the toolbar (desktop: a green "Zen mode on" band). If it's not there, Zen is off or paused."
- [severity: missing] "Three ways to flip the switch: CLI … TUI `Alt+Z` … Status check" (lines 19-25)
  - Reality: also the palette ("Turn Zen Mode on") and the desktop menu item "Turn Zen Mode On" (⌥Z). The first Off→On from TUI/desktop (while `zen_intro_seen` is unset) shows an "Enable Zen Mode?" dialog (Cancel / Enable, "Don't show this again"). App toggles log `reason=user:palette`.
  - Evidence: `crates/shelbi-app/src/zen.rs:15-34`; `crates/shelbi-tui/src/shell/mod.rs:3807-3808`; `crates/shelbi-tui/src/overlay/zen_intro.rs:1-8`; `desktop/src/menus/tree.rs:525,962`; `desktop/src/views/palette.rs:682`; real log `project=shelbi mode=zen off -> on reason=user:palette`.
  - Fix: add "**Palette / menu**: 'Turn Zen Mode on' in the command palette, or the desktop app's 'Turn Zen Mode On' menu item (⌥Z)." and a note on the one-time "Enable Zen Mode?" confirmation.
- [severity: outdated] The "verbatim-in-sync" `zenmode.md` block (lines 129-313) has drifted from `crates/shelbi-state/src/default_zenmode.md.template`
  - Reality: (a) the template adds an external-tracker ownership filter after "no file overlap with anything in flight" (doc lines 162-163); (b) step 8 uses `--reason "orchestrator:zen-merge pr=<pr-number>"` on one line plus a `shelbi changelog add …` step, where the doc has `--reason "orchestrator:zen-merge\n   ci-green(<pr-number>)"` (lines 271-274).
  - Evidence: `diff` of doc lines 133-312 vs the template → hunks `30,31c30,36`, `139,142c144,153`; real log `reason=orchestrator:zen-merge_pr=2007`.
  - Fix: paste the template verbatim (or at least replace those two hunks with template lines 30-36 and 144-153).
- [severity: wrong] `ci_timeout: 15m` (line 538; schema line 735) and "Accepts seconds as a bare integer, or a string like `30s`, `5m`, `2h`, `1d`" (lines 732-734)
  - Reality: project `zen.ci_timeout` is a bare `u64` seconds; `15m` fails the project load. Duration strings are only accepted by `zen ci-watch --timeout`.
  - Evidence: `crates/shelbi-core/src/model.rs:2626` (`with = "duration_secs"`), `3093-3105`. Sandbox `shelbi zen status` with `ci_timeout: 15m` → "could not read zt.yaml: yaml: zen.ci_timeout: invalid type: string "15m", expected u64"; with `900` → `ci timeout: 15m`.
  - Fix: both examples → `ci_timeout: 900   # seconds (15m)`; comment → "# How long to wait for required GitHub checks, in seconds (bare integer). Default: 900 (15m)."
- [severity: unclear] Judgment/policy attributed to "the orchestrator's prompt" (lines 437-439, 441-443, 480, 524-527, 612, 633-634, 654)
  - Reality: categories, thresholds and merge conditions live in `zenmode.md` (as the page's own lines 374-382 say); the orchestrator prompt only points at it.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:959-968` ("`zenmode.md` is the source of truth … are no longer inlined here").
  - Fix: replace "the orchestrator's prompt" with "`zenmode.md`" at each spot (e.g. "applies the judgment categories in `zenmode.md`", "Tunable in `zenmode.md`").
- [severity: missing] "`shelbi zen scan` gives … backlog ids that are *mechanically* safe: not blocked on dependencies, not explicitly opted out, no file overlap" (lines 606-609)
  - Reality: on a remote tracker the scan also keeps only issues assigned to the authenticated user and fails closed (empty) when the login is unresolved.
  - Evidence: `crates/shelbi-orchestrator/src/zen.rs:14143-14147`.
  - Fix: append "and, on a remote tracker such as GitHub, assigned to you (the authenticated account); an unresolved login yields an empty list."
- [severity: unclear] "`enabled: true` opts an issue in even when Zen is off (rare but supported)" (lines 703-705)
  - Reality: nothing acts on `enabled: true`; the scan excludes only `Some(false)`, the template never mentions `zen.enabled`, and the only consumer of `true` is the in-flight count in `shelbi zen status`.
  - Evidence: `crates/shelbi-orchestrator/src/zen.rs:14269-14273`; `crates/shelbi-cli/src/commands/zen.rs` (`zen_applies`, `count_in_flight_zen`); `grep enabled default_orchestrator.md.template` → nothing.
  - Fix: "`enabled: false` keeps the issue on the manual-review path even with Zen on. (`enabled: true` is accepted but only affects the in-flight count in `shelbi zen status`; it does not make the orchestrator act while Zen is off.)"
- [severity: outdated] "a small **ZEN** avatar badge in the same column the per-workspace avatars use, and a subtle dark-tinted row background" (lines 778-782)
  - Reality: Zen rows get a dim `[zen]` system chip in the identity column; no background tint.
  - Evidence: `crates/shelbi-app/src/activity.rs:2151-2152, 2711` (`row.chip = system_chip("zen")`); `crates/shelbi-tui/src/activity.rs:610-611` ("the badge/background tint is gone").
  - Fix: "Zen-driven events show a dim `[zen]` chip in the identity column (where workspace chips like `[alpha]` go)."
- [severity: wrong] "most render with their own specific phrasing" + table rows `orchestrator:zen-promote category=<n>` and `zen:* key=…` (lines 784-796)
  - Reality: spaces in reasons become `_` on disk (`orchestrator:zen-promote_category=2`); `parse_zen_reason` splits on space, so only `orchestrator:zen-merge` (prefix match) gets dedicated phrasing; promote and other reasons with extras render as a generic move with the `[zen]` chip.
  - Evidence: `crates/shelbi-app/src/activity.rs:1682-1729`; real line `reason=orchestrator:zen-promote_category=2`.
  - Fix: say the feed renders `orchestrator:zen-merge` (from `pr=<n>`) with dedicated phrasing and other `orchestrator:zen-*` / `zen:*` reasons as generic moves with the `[zen]` chip — or fix `parse_zen_reason` to split on `_` too (see App-side issues).
- [severity: missing] Condition 5's `pr-create` command (lines 485-486)
  - Reality: the default policy also passes `--match-published-head-commit <published_head_sha>` when the probe rebased the branch.
  - Evidence: `crates/shelbi-cli/src/commands/zen.rs` (`PinnedIdentityArgs::match_published_head_commit`); template step 6; `ProbeReport.published_head_sha` (`crates/shelbi-orchestrator/src/zen.rs:6848`).
  - Fix: after "All six values are copied directly from the probe report", add "When the probe rebased the branch, also pass `--match-published-head-commit <published_head_sha>`."
### site/content/docs/configuration/agents.mdx

- [severity: broken-link] "[`agent_runners`](/docs/configuration/project#agent-runners)" (lines 96 and 120)
  - Reality: the heading `## agent_runners` slugs to `agent_runners` (github-slugger, used by rehype-slug, keeps underscores).
  - Evidence: `site/content/docs/configuration/project.mdx:208`; `site/contentlayer.config.ts:3` (rehype-slug), `site/lib/docs.ts:1` (github-slugger).
  - Fix: `/docs/configuration/project#agent_runners`.
- [severity: missing] Resolution chains (lines 141-153) omit the top tier
  - Reality: a task's frontmatter `launch:` block (`runner`, `model`, `effort`, `permission_mode`) sits above the project for both chains; its `permission_mode` is still clamped by the ceiling.
  - Evidence: `crates/shelbi-core/src/agent_manifest.rs:345-400`; `crates/shelbi-core/src/model.rs:2207-2214, 2947-2964`; `crates/shelbi-orchestrator/src/load.rs:949`; `crates/shelbi-orchestrator/src/poller.rs:4821`.
  - Fix: runner kind: `task launch.runner → project agents.<name>.runner → agent.yaml preferred_runner → claude`; model/effort: `task launch.{model,effort} → project runners.<kind>.{…} → agent.yaml runners.<kind>.{…} → none`; permissions: "a task `launch.permission_mode` beats the manifest's; both are clamped by `workspace_permissions_mode`."
- [severity: missing] Project runners table (lines 99-105) lists only `command`, `flags`, `dialog_signatures`, `model`, `reasoning_effort`
  - Reality: the entry flattens all of `AgentRunnerSpec`, so `prompt_injection` and `integration` (`claude|codex|generic`) are accepted.
  - Evidence: `crates/shelbi-core/src/model.rs:1704-1738, 1849-1856`.
  - Fix: add rows for `prompt_injection` (optional; built-in default per executable) and `integration` (optional; overrides basename detection).
- [severity: missing] No mention of Settings ▸ Agents
  - Reality: both clients have an Agents page (desktop ⌘6, TUI `,` then `6`) editing description, instructions, preferred runner, model, effort, permissions_mode, skills, writing `agent.yaml` via a comment-preserving patch, with Reset to default.
  - Evidence: `crates/shelbi-state/src/agents_config.rs:1-55`; `crates/shelbi-app/src/settings.rs:75-77, 113-114`; `crates/shelbi-tui/src/shell/settings.rs:136-142`.
  - Fix: add an "Editing from Settings" section like the statuses page's.
- [severity: unclear] "living under `~/.shelbi/projects/<id>/agents/<name>/`" (lines 7-8, 21)
  - Reality: in-repo projects use `<repo>/.shelbi/agents/<name>/`.
  - Evidence: `crates/shelbi-state/src/lib.rs:773-778`.
  - Fix: "under `<config-root>/agents/<name>/` (`~/.shelbi/projects/<id>/` in global mode, `<repo>/.shelbi/` in in-repo mode)".
- [severity: missing] "`shelbi init` scaffolds a self-documenting `agent.yaml` for every default agent" (line 14)
  - Reality: true for the six shipped agents (reload recreates a missing one), but `shelbi agent new <name>` creates only `instructions.md` + empty `skills/`.
  - Evidence: `crates/shelbi-state/src/agent_workspaces.rs:928-933`; sandbox `shelbi agent new perf`.
  - Fix: add "An agent you create with `shelbi agent new` has no `agent.yaml` until you add one, so it resolves through the built-in defaults."

### site/content/docs/configuration/global.mdx

- [severity: missing] No `desktop:` block documented, though `desktop.window_opacity`/`window_blur` are mentioned (lines 28-34, 38-42)
  - Reality: `UserConfig.desktop` has `cli_path`, `terminal_font_family`, `terminal_font_size`, `option_as_meta` (`false|true|left|right`), `terminal_scrollbar` (`auto|scrolling|always`), `notifications.{workspace_blocked,review_ready,orchestrator_waiting}` (default on), `terminal_colors.<black..white>.{regular,bold}`, `window_opacity` (percent; floor 30; unset = 100; fraction like `0.85` accepted), `window_blur` (0-50; unset 0), `contrast` (0-100; unset 0), `app_icon` (`command-grille|retro`).
  - Evidence: `crates/shelbi-state/src/user_config.rs:52-176, 356-372, 461-490`; `crates/shelbi-core/src/scaffold.rs:462-497`.
  - Fix: add a `desktop` section with a row per key (type, default, description); note Settings ▸ General / Terminal write them.
- [severity: missing] "relocated via `--root`, then `$SHELBI_ROOT`, then `$SHELBI_HOME`, before falling back to `$HOME/.shelbi`" (lines 15-17)
  - Reality: an install-time baked default sits before the `~/.shelbi` fallback.
  - Evidence: `crates/shelbi-state/src/root.rs:4-13, 121-123`; `shelbi --help` ("default: baked at install time; also overridable via $SHELBI_ROOT").
  - Fix: "…then `$SHELBI_HOME`, then a default baked in at install time (if any), before falling back to `$HOME/.shelbi`."
- [severity: unclear] `editor` row: "Editor the review interface's 'Edit in `<editor>`' view launches" (line 42); "in the switcher" (line 56)
  - Reality: `resolve_editor()` also drives the workspace panel's "Edit in <editor>" and desktop external edits (issues, workflow YAML).
  - Evidence: `crates/shelbi-app/src/workspace_panel.rs:51`; `crates/shelbi-orchestrator/src/workspace_session.rs:123`; `desktop/src/views/external_edit.rs:155,205`; `desktop/src/views/issues.rs:1080`.
  - Fix: "Editor Shelbi opens for 'Edit in `<editor>`' (the review interface and the workspace panel) and for the desktop app's Edit YAML / external edits."
- [severity: unclear] Example sets `keymap: zen_toggle: ctrl-g` in `config.yaml` (lines 44-50)
  - Reality: a non-default `config.yaml::keymap.zen_toggle` is migrated into `keys.yaml` (`defaults.global.zen_toggle`) on load and reset, with a one-time notice — the example writes a legacy key.
  - Evidence: `crates/shelbi-state/src/keymap/loader.rs:9-15, 339-353`.
  - Fix: drop `keymap:` from the example; add "`keymap.zen_toggle` is legacy: a non-default value is moved into `keys.yaml` (`defaults.global.zen_toggle`) on the next start and reset here."
- [severity: outdated] "Overrides for the TUI keybindings." (line 94)
  - Reality: the desktop app uses the same `keys.yaml` keymaps; both clients edit it in Settings ▸ Keybindings (TUI `,` then `2`; desktop ⌘2).
  - Evidence: `desktop/src/keys.rs:1-10`; `crates/shelbi-app/src/settings.rs:66-67, 107-115`; `crates/shelbi-tui/src/shell/settings.rs:136-142`.
  - Fix: "Overrides for Shelbi's keybindings, shared by the TUI and the desktop app. Both edit this file in Settings ▸ Keybindings."
- [severity: missing] Actions table (lines 144-174) omits rebindable actions
  - Reality: missing `global.focus_sidebar` (`ctrl-h`), `global.focus_main` (`ctrl-l`), `sidebar.quit` (`q`,`ctrl-c`), `kanban.open_workspace` (`o`), `popover.move_left` (`H`,`shift-left`), `popover.move_right` (`L`,`shift-right`), `popover.open_workspace` (`o`), `activity.scroll_end` (`G`,`end`), `palette.backspace` (`backspace`).
  - Evidence: `crates/shelbi-state/src/keymap/actions.rs:259-332, 352-410`; `shelbi config list-actions`.
  - Fix: add the rows; point readers to `shelbi config list-actions`, `shelbi config dump-keybindings` and `shelbi config check` (none mentioned).
- [severity: wrong] Example `projects.myapp.kanban.move_card_left: [H, ctrl-h]` (lines 176-185)
  - Reality: `ctrl-h` is built-in `global.focus_sidebar`; global dispatch runs first, so the binding is dead and warned about.
  - Evidence: `crates/shelbi-state/src/keymap/loader.rs:562-591, 695`; sandbox `shelbi config check -p myapp` on this file → "warning: chord `ctrl-h` for `kanban.move_card_left` is also bound to global `focus_sidebar`; the global binding fires first, so this per-mode binding is dead".
  - Fix: use a free chord (e.g. `[H, alt-h]`) and note that a mode binding equal to a `global` chord is shadowed in `sidebar`/`kanban`/`activity`.

### site/content/docs/configuration/project.mdx

- [severity: broken-link] "[`agent_runners`](#agent-runners)" (line 164) and "[AgentRunner](#agent-runners)" (line 114)
  - Reality: heading `## agent_runners` (line 208) slugs to `#agent_runners`.
  - Evidence: generated contentlayer heading ids include `agent_runners`; `site/lib/docs.ts` uses github-slugger.
  - Fix: both links → `#agent_runners`.
- [severity: wrong] `repo` "Clone URL or path to the repository" (line 109); example `repo: git@github.com:me/myapp.git` (line 81)
  - Reality: `repo` is a local filesystem path (locates `<repo>/.shelbi/project.yaml`, the in-repo config root, migration repo root, changelog origin lookup). init/wizard write the absolute checkout path.
  - Evidence: `crates/shelbi-state/src/lib.rs:1448-1449`; `crates/shelbi-state/src/project_paths.rs:108`; `crates/shelbi-state/src/migrate.rs:198`; `crates/shelbi-state/src/changelog.rs:854`; `crates/shelbi-cli/src/wizard.rs:225`; sandbox `local.yaml`: `repo: /…/work/myapp`.
  - Fix: row → "Local path to the repository checkout (`~` expanded). In in-repo mode Shelbi reads `<repo>/.shelbi/project.yaml` from here. User-local."; example → `repo: ~/Workspaces/myapp`.
- [severity: wrong] `editor` "Editor invoked by `shelbi … edit` commands. User-local. Default `$EDITOR`" (line 119)
  - Reality: parsed and split into `local.yaml`, but nothing reads it. `shelbi agent/issue edit` use `$VISUAL` → `$EDITOR` → `vi`; the review "Edit in <editor>" uses global `config.yaml` `editor:` → `$EDITOR` → `vim`.
  - Evidence: `crates/shelbi-core/src/model.rs:201, 273` (field only); `crates/shelbi-cli/src/commands/mod.rs:109-150`; `crates/shelbi-state/src/user_config.rs:613-627`; no reader of `Project.editor` in crates/ or desktop/.
  - Fix: "Reserved; currently unused. `shelbi … edit` commands use `$VISUAL`/`$EDITOR` (falling back to `vi`); the review editor is set in [global config](/docs/configuration/global)."
- [severity: outdated] `default_workflow` default "`default`" (line 111); example comment "instead of workflows/default.yaml" (lines 83-84)
  - Reality: init/wizard write `default_workflow: task` and scaffold `task.yaml`/`subtask.yaml`. Reload rewrites unset/`default` to `task`; a lingering `default` resolves to `task.yaml` when `default.yaml` is absent.
  - Evidence: `crates/shelbi-orchestrator/src/project_create.rs:596`; `crates/shelbi-state/src/lib.rs:578-600`; `crates/shelbi-state/src/workflows.rs:229-245`; sandbox `project.yaml` has `default_workflow: task`.
  - Fix: default → "`task` (written by `shelbi init`; an unset or legacy `default` is migrated to `task` on `shelbi reload`, and resolves to `task.yaml` when no `default.yaml` exists)"; comment → "…use workflows/app.yaml instead of workflows/task.yaml."
- [severity: missing] Top-level table (lines 105-129) lacks `session` and `workspace_settings_template`
  - Reality: `session.raw_output_log` (bool, default false) keeps a raw log at `~/.shelbi/sessions/<id>/raw.log`; `workspace_settings_template` (path) overrides the template location.
  - Evidence: `crates/shelbi-core/src/model.rs:118-122, 160-167, 246-268, 720-740`; `crates/shelbi-state/src/project_paths.rs:127-132`.
  - Fix: add `session` | SessionConfig | no | off | "`raw_output_log: true` keeps a full raw output log of every session at `~/.shelbi/sessions/<id>/raw.log` (opt-in debugging)." and `workspace_settings_template` | path | no | `<config_root>/workspace-settings.json.template` | "Override path for the workspace settings template (`~` expanded)."
- [severity: missing] `agent_runners` field table (lines 215-219) lacks `prompt_injection` and `integration`
  - Reality: `prompt_injection` (`{kind: positional-arg|flag-file|stdin|paste, flag?}`; default by basename: claude/codex positional, aider `--message-file`, others stdin); `integration` (`claude|codex|generic`) overrides basename detection for wrappers.
  - Evidence: `crates/shelbi-core/src/model.rs:1745-1771, 1866-1910`.
  - Fix: add both rows with those defaults.
- [severity: outdated] "Claude's `.claude/settings.json` references `.shelbi/hooks/claude.*`" (lines 191-193)
  - Reality: hooks are wired into the worktree's `.claude/settings.local.json` and call `$CLAUDE_PROJECT_DIR/.shelbi/hooks/{session-start,stop,pane-working,pane-idle,pane-blocked}.sh`; `claude.*` is the pre-rename form that config-upgrade heals (`SETTINGS_STALE_CLAUDE_HOOK`).
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:2994-3004, 3993`; `crates/shelbi-state/src/default_workspace_settings.json.template`; `crates/shelbi-cli/src/commands/config_upgrade.rs:2262-2270`.
  - Fix: "…and the worktree's `.claude/settings.local.json` wires `$CLAUDE_PROJECT_DIR/.shelbi/hooks/*.sh` so the pane pushes…".
- [severity: outdated] machine/workspace `tags` "Accepts a scalar `tag:` alias and a bare string as shorthand" (lines 142, 285); `danger_paths` "accepts three forms … or a bare list" (lines 323-325, 335)
  - Reality: still loaded, but the config-upgrade pass flags and rewrites them (`TAG_SHORTHAND_DEPRECATED` → `tags: [x]`; `ZEN_DANGER_PATHS_BARE_SEQ` → `{ extend: [...] }`).
  - Evidence: `crates/shelbi-cli/src/commands/config_upgrade.rs:1422-1443, 1455-1463`.
  - Fix: "Legacy shorthands (`tag: x`, `tags: x`) still load but are auto-rewritten to `tags: [x]` by the [config-upgrade pass](/docs/concepts/config-upgrade)." Same for bare `danger_paths`; drop "or a bare list" from the example comment.
- [severity: unclear] `host` "Required when `kind: ssh`; falls back to `name` if omitted" (line 141)
  - Reality: not required; missing `host` uses `name`.
  - Evidence: `crates/shelbi-core/src/model.rs:1660-1669` (`Machine::host`).
  - Fix: "SSH hostname for `kind: ssh`; defaults to `name` when omitted. Ignored for `local`."
- [severity: unclear] Branch resolution "explicit `branch:` wins; otherwise Shelbi renders `git.branch`; when neither is set … GitHub username, falling back to `user`" (lines 423-426)
  - Reality: order is (1) task `branch:`; (2) a workflow `git:` block's `branch`/`branch_prefix` replaces the project's wholesale; (3) `git.branch` template; (4) `<branch_prefix>/<id>`; (5) `<github_login>/<id>`, else `user/<id>`.
  - Evidence: `crates/shelbi-orchestrator/src/branch.rs:36-110`.
  - Fix: spell out that order, including `branch_prefix` and the `<login>/<id>` form.
- [severity: unclear] "In the committed `<repo>/.shelbi/project.yaml`, the `name:` key is the project id" (lines 57-61; also line 24)
  - Reality: for in-repo projects the local id is the registry directory name (`~/.shelbi/projects/<id>/`), which may be a suffixed alias (`myapp-2`); the committed `name:` stays canonical and seeds pick-up.
  - Evidence: `crates/shelbi-state/src/lib.rs:1243-1244`; sandbox suffixed pick-up registered `projects/myapp-2/local.yaml`.
  - Fix: "…`name:` is the canonical id that `--pick-up` seeds a local alias from. The local id is the `~/.shelbi/projects/<id>/` directory name, which may be suffixed."

### site/content/docs/configuration/statuses.mdx

- [severity: wrong] "The single-instance categories — `backlog`, `ready`, `active`, and `handoff` — raise a non-fatal warning if any of them appears more than once … So two `active` statuses load, but with a warning." (lines 83-86)
  - Reality: only `backlog`, `ready`, `handoff` warn; `active` is deliberately excluded (downstream active gates are first-class). Two `active` statuses load with no warning.
  - Evidence: `crates/shelbi-core/src/statuses.rs:112-115` (`category_warnings` loops `[Backlog, Ready, Handoff]`; "`active` is deliberately **not** in that set").
  - Fix: "The single-instance categories — `backlog`, `ready`, and `handoff` — raise a non-fatal warning if any appears more than once, as does a missing `handoff`. `active` may repeat freely: a second active status is how you add an agent gate (e.g. `adversarial-review`) before review."

### site/content/docs/configuration/workflow.mdx

- [severity: outdated] "Every project has a built-in virtual `default` workflow that lives in code until `shelbi workflow new` or `edit` writes a file." (lines 21-24)
  - Reality: `shelbi init` writes `workflows/task.yaml` + `subtask.yaml` and sets `default_workflow: task`; the in-code `default` is only a fallback when `workflows/` has no `.yaml` files, and a `default` reference aliases to `task` when `default.yaml` is absent.
  - Evidence: `crates/shelbi-state/src/workflows.rs:150-179, 221-243, 301-310`; sandbox init → `workflows/{statuses,task,subtask}.yaml`, `shelbi workflow list` → `subtask`, `task`.
  - Fix: "`shelbi init` writes two workflows, `task` (the project default) and `subtask`. A built-in `default` workflow is used only when `workflows/` has no workflow files. See [shipped workflows](/docs/guides/getting-started/workflows#shipped-workflows-task-and-subtask)."
- [severity: outdated] "The shipped `default` workflow is a pure status track: six statuses and **no** transitions … The scaffolded `default.yaml` writes `transitions:` only as a commented optional block" + the `default.yaml` example (lines 28-52)
  - Reality: the shipped track is `task.yaml`: statuses backlog/todo/in-progress/review/done/canceled; `review` is `owner: user, agent: review, tags: [review]`; transitions `in-progress→review [push_branch, open_pr]`, `review→done [push_branch, merge]`, `in-progress→canceled` and `review→canceled [close_pr, delete_branch]`; `git: {base_branch: main, branch: '{{github_user}}/{{id}}', merge_strategy: squash}`; only `initial_status` and `zen` are commented.
  - Evidence: `crates/shelbi-core/src/workflow.rs:928-1031`; `crates/shelbi-core/src/scaffold.rs:343-394`; sandbox `task.yaml`; `shelbi workflow show task`.
  - Fix: replace the example with the scaffolded `workflows/task.yaml` and drop the "no transitions" sentence.
- [severity: wrong] `statuses` row: "Order is the column order in the TUI." (line 59)
  - Reality: columns follow `statuses.yaml` order filtered to the workflow's subset; the workflow list order only sets the default `initial_status` (first entry).
  - Evidence: `crates/shelbi-app/src/board.rs:34-55`; `shelbi workflow show --help` ("Statuses are listed in the canonical `statuses.yaml` order, filtered to the subset the workflow declares").
  - Fix: "Statuses this workflow uses (at least one). Columns render in `statuses.yaml` order. The first entry here is the default `initial_status`."
- [severity: missing] Top-level fields table (lines 56-65) has no `review` row (field documented at line 192)
  - Evidence: `crates/shelbi-core/src/workflow.rs:131-143` (`review: Option<ReviewServe>`).
  - Fix: add `| review | [Review](#review) | no | — (diff-only review) | Recipe the Review agent runs to serve this workflow's branch. |`.
- [severity: broken-link] "[Required params](#required-params)" (line 63)
  - Reality: heading `## required_params` (line 237) slugs to `required_params`.
  - Evidence: github-slugger (`site/lib/docs.ts:1`) keeps underscores.
  - Fix: `#required_params`.
- [severity: wrong] `agent` row: "Defaults to `developer` for `active` and `orchestrator` for `ready` when omitted." (line 83)
  - Reality: `owner: agent` without `agent:` fails the load (inline `category:` is rejected, so there's nothing to derive from). With `owner: user`, omitting `agent` means no automation.
  - Evidence: `crates/shelbi-core/src/workflow.rs` (`resolve_owner_agent`, `None =>` arm); `crates/shelbi-state/src/workflows.rs:196-200`; sandbox `- { id: todo, owner: agent }` → "status `todo` has owner: agent but neither an `agent:` field nor a `category:` to derive a default from — add one".
  - Fix: Default "—"; description "Which agent runs this status under automation. Required when `owner: agent`. Omitted on a `user` status means no automation path."
- [severity: missing] Transitions table (lines 103-111) has no `url`
  - Reality: `Transition.url` is the reviewable URL for a server the edge's `run:` starts (`$PORT`/`$SLOT` substituted); fallback for Open Browser when the workflow has no `review.url`.
  - Evidence: `crates/shelbi-core/src/workflow.rs:215-229, 1474-1485`.
  - Fix: add `| url | string | no | — | Reviewable URL for a server this edge's run: starts; used by Open Browser when the workflow has no review.url. |`.
- [severity: wrong] git `merge_strategy` "How this workflow's branches integrate back." default "project's `merge_strategy`" (line 270); "the workflow inherits … `merge_strategy` from the project" (lines 261-263)
  - Reality: the `merge` action reads only the project's `git.merge_strategy` and rejects `rebase` ("merge action does not support `merge_strategy: rebase` yet"); a workflow `git:` block that omits the key deserializes it as `squash`, not the project value. No consumer reads the workflow's value beyond `Workflow::resolve_git`.
  - Evidence: `crates/shelbi-orchestrator/src/actions.rs:503, 1500-1510`; `crates/shelbi-core/src/model.rs:659-663, 1174-1177`; `crates/shelbi-core/src/workflow.rs:789`.
  - Fix: "`merge_strategy` | `squash` \| `merge` | … | Accepted for symmetry, but the `merge` action currently uses the **project's** `git.merge_strategy`; `rebase` is rejected." (Or fix the app — see App-side issues.)
- [severity: unclear] "the in-file `name:` field must match" the filename (lines 21-22)
  - Reality: a mismatch only warns; lookup is by filename, so the declared name won't resolve; duplicate names across files are a hard error.
  - Evidence: `crates/shelbi-state/src/workflows.rs:344-357`.
  - Fix: "keep the in-file `name:` equal to the filename: Shelbi resolves workflows by filename and warns on a mismatch."
- [severity: missing] No mention of Settings ▸ Workflows
  - Reality: Settings ▸ Workflows (desktop and TUI, `,` then `4`) edits workflow files via a form and a graph builder (statuses, owner/agent/tags, transitions with run/ready/url, git, required_params, `review:`), saving through a comment-preserving YAML patch.
  - Evidence: `crates/shelbi-state/src/workflow_config.rs:1-58`; `crates/shelbi-tui/src/shell/settings.rs:136-142`.
  - Fix: add an "Editing from Settings" section mirroring the statuses page's.

## App-side issues found while auditing

These are not doc fixes. They are app bugs or gaps, for whoever triages them. Some doc fixes above depend on which way each one is resolved.

1. **`review` listed as a removed project key.** `REMOVED_PROJECT_KEYS` contains `"review"` (`crates/shelbi-cli/src/commands/config_upgrade.rs:1102`). As a result, the live, documented `review: { diff_command: … }` block is reported as `needs_judgment[PROJECT_REMOVED_KEY]`, and an inert one would be auto-dropped. Reproduced with sandbox `shelbi config upgrade`.
2. **Config upgrade breaks pick-up of in-repo projects.**
   - In-repo init writes `name: <slug>` + `display_name: <Label>` (`crates/shelbi-orchestrator/src/project_create.rs:638-640`).
   - The config-upgrade pass then auto-heals the committed file to `name: <Label>`.
   - After that, `shelbi init --pick-up` fails with "project name `My App` is invalid". Reproduced in a sandbox.
3. **CLI hints name commands that don't exist.**
   - `shelbi spawn` (`crates/shelbi-cli/src/commands/init.rs:229, 242`).
   - `shelbi project rename` (`init.rs:847`).
   - After an in-repo init, the `next:` hint points at `~/.shelbi/projects/<name>.yaml`, which doesn't exist in that mode (`init.rs:237-240`).
4. **Legacy global init writes an empty repo path.** `shelbi init --mode global` writes `repo: ''` and per-workspace `runner: claude` (`project_create.rs:603-616`). Running `migrate-to-in-repo` on such a project then plans a relative `.shelbi` repo root.
5. **Migration leaves files behind.** `migrate-to-in-repo` doesn't move `zenmode.md`, `pr-template.md` or `changelog-template.md` (`crates/shelbi-state/src/migrate.rs:46-51`).
6. **Unused project field.** The project `editor` field is parsed but never read.
7. **Unused budget key.** `issue_tracker.budget.graphql_low` is parsed but never read (`crates/shelbi-state/src/gh_budget.rs`).
8. **Zen crash recovery is dead code.** The `__zen-orch-start`, `__zen-heartbeat` and `__zen-orch-exit` hooks have no caller in the session model. A stale comment at `crates/shelbi-orchestrator/src/poller.rs:348-349` still refers to them.
9. **Activity feed misses Zen reasons.** `parse_zen_reason` (`crates/shelbi-app/src/activity.rs:1682-1729`) splits on a space, but on-disk reasons use `_`. So `orchestrator:zen-promote_category=N` never gets its dedicated phrasing.
10. **Per-workflow merge strategy is ignored.** A workflow's `git.merge_strategy` is never consulted by the `merge` action (`crates/shelbi-orchestrator/src/actions.rs:503`). If per-workflow strategies are meant to work, this is a bug rather than a doc fix.
11. **`zen.enabled: true` does nothing.** It is accepted, but no code acts on it (`crates/shelbi-orchestrator/src/zen.rs:14269-14273`).

## Verified accurate

The claims below were checked against source and/or the CLI and found correct. The fix task does not need to re-check them. Every in-scope page also appears under Findings; this section lists the parts of each page that held up.

### concepts/agents.mdx
- **Shipped agents:** six, with their names and order (`crates/shelbi-state/src/agent_workspaces.rs:228-277`; `shelbi agent list`). The developer, qa, security and adversarial role descriptions match their templates. The reviewers ship unwired (`shelbi agent list` shows `-` statuses).
- **Prompt composition:** `_shared/preamble.md` is prepended to `instructions.md` (`compose_agent_prompt`, `agent_workspaces.rs:471-494`). The hub-safety preamble is seeded on new projects and created if missing; anything else is a config-upgrade finding (`config_upgrade.rs:2425-2460`). Every default agent has `skills/`, and `review/` ships `load-run-detection`.
- **Self-heal:** a pristine-but-stale file auto-upgrades; the orchestrator gets required-section repair and the zen-command migration; others are Preserved; deleting `instructions.md` and reloading restores it (`agent_workspaces.rs:848-1035`).
- **CLI:** `shelbi agent list|show|edit|new` behave as described. `developer` is the CLI dispatch fallback for an active status (`crates/shelbi-cli/src/commands/issue.rs:908-950`).
- **Links:** all resolve.
- **unverified:** the sidebar mockup's per-workspace agent label. It is consistent with `WorkspaceRow.agent` (`crates/shelbi-app/src/view.rs:99-111`), but the sidebar was not rendered.

### concepts/changelog.mdx
- **Storage:** entries live in the state dir `~/.shelbi/projects/<name>/changelog/` in either mode (`crates/shelbi-state/src/changelog.rs:1-26, 76-80`).
- **UI:** there is a Changelog page in the TUI (`shell/changelog.rs:47`, `sidebar.rs:1295`) and in the desktop app (`desktop/src/app.rs`, `menus/tree.rs`).
- **`shelbi changelog add`:** the flags `--author`, `--title`, `--date`, `--issue`, `--pr`, `--link` and `-d` all exist, and the author defaults to `user`.
- **Orchestrator template:** the `## Changelog` section matches (four merge sources keyed on `to_category=done`, the duplicate check via `changelog search`, one entry per feature, always write one) (`default_orchestrator.md.template:897-942`).
- **Config upgrade:** it appends the section only when it is missing (`config_upgrade.rs:887-967`, `config_upgrade_apply.rs:242-247`).
- **Links:** all resolve, including `cli/changelog#the-template`.

### concepts/config-modes.mdx
- **Config vs state:** the split holds, and state always lives under `~/.shelbi/`.
- **Init flags:** first-run, `init -y` and the wizard use global mode, and `--mode` is incompatible with `-y` (`init.rs:249-253`).
- **Mode table:** the shared/local locations, state location, global discovery, and walk-up then `ProjectNotPickedUp` rows are right. The `local.yaml`-is-authoritative callout is right (`lib.rs:1221-1231`).
- **Errors:** the misplaced-field and duplicate-key errors match (`model.rs:1365-1442`, `error.rs:136-144`).
- **Resolution order:** global reverse lookup (local machines, deepest match, `-p` short-circuit) and walk-up beating the registry both match (`resolve.rs:52-95`).
- **Files written:** a fresh in-repo init writes both halves, and `local.yaml` holds repo/machines/editor/workspaces. The `.gitignore` snippet matches `IN_REPO_GITIGNORE_SNIPPET` byte for byte.
- **Aliases:** the suffix ladder is `-2`, `-3` (`init.rs:885-899`).
- **Migration:** the split, `config_mode: in-repo`, the rename-then-copy fallback (`migrate.rs:368-410`) and the `.yaml.migrated` retire are all as described. So are `--dry-run`, `--yes`, `--project`, the non-TTY skip and idempotency. There is no `migrate-to-global`.
- **Edge cases:** the legacy name-only stub fallback (`init.rs:700-727`) and the bare-init safety-net error text match.
- **Links:** all resolve.

### concepts/config-upgrade.mdx
- **Detection:** there is no schema version; it is content sniffing only.
- **When it runs:** on daemon start for every registered project, including split ones (`serve.rs:255`), and on reload (`reload.rs:91`). It is best-effort, runs detect → apply → re-detect → emit, and never auto-applies a needs-judgment finding.
- **Finding codes:** every example code exists with the stated classification.
- **Findings file:** `~/.shelbi/config-upgrade-findings.json`, removed when clean. Its fields are id/code/scope/file/location/message/proposed_fix/rationale.
- **CLI:** `shelbi config upgrade` with `--needs-judgment`, `--apply` and `--apply-finding` exists.
- **Links:** all resolve.

### concepts/events-log.mdx
- **Location:** `events.log` sits under the Shelbi root, is hub-global, and the named functions exist.
- **Workspace line:** the base shape `project= workspace= <prev|none> -> <new>` is right, and lines are written only on real transitions.
- **Task line:** the base shape with `reason= from_category= to_category=` is right. An empty workflow is written as the default name, categories come from `statuses.yaml`, and whitespace in a reason becomes `_`.
- **Heartbeat:** the base shape with `zen=on` is right. The interval is 60s doubling to a 5m cap, `heartbeat: off` works, real events are detected by an mtime baseline, the first attempt after a restart waits one interval, and the offline probe targets `1.1.1.1:443` with a 1s timeout. The activity feed hides heartbeats.
- **Parsing:** `EventKind::from_body` keywords match.
- **Atomicity:** a single O_APPEND `write_all` under a file lock; the test `concurrent_task_and_workspace_appends_dont_tear` exists.
- **`events tail`:** `-n` defaults to 20, `--since` takes s/m/h/d, follow polls every 250ms, a partial last line is held back, and truncation resets the offset.
- **Reasons:** `user:cli` is the `issue move` default and `workspace:ready-marker` is the handoff cause.
- **Links:** all resolve.

### concepts/github-issues.mdx
- **Config:** the `issue_tracker.backend` / `github.repo` YAML shape and its validation match (`model.rs:806-937, 1045-1098`).
- **Auth:** the token chain is `GH_TOKEN` → `GITHUB_TOKEN` → `gh auth token` → `tokens.yml` (`issue_auth.rs:10-19, 287-300`). The setup preflight is as described.
- **Backends:** the file-system default path is right. The jira/linear stub error is right.
- **Labels:** the `shelbi:id/<slug>` scheme, the 50-character truncation to `<slug>-<hash8>`, and the single status label all match. A move keeps non-status labels.
- **Metadata block:** the markers, fields and order match, and a malformed block is handled as described (`github_store.rs:5114-5185`). Close reasons and the open-with-terminal-label → `backlog` rule match.
- **Issues filed on GitHub:** the number-as-id fallback, the initial status when a label is missing, and PR filtering all match.
- **Label auto-creation:** "Managed by shelbi", idempotent.
- **Ownership details:** the assignment overlay in `<project_dir>/assignments/` is never written to GitHub. Issues are created assigned to the viewer, and the Zen gate is case-insensitive and fails closed.
- **Board index:** the board-index fields are right. `refresh_secs` defaults to 30, staleness is 3× the interval with a 90s floor, and there is a 600s cold read.
- **Done history:** 50 issues, 10-minute TTL.
- **Transport:** GraphQL with REST fallback; single fetches use REST `If-None-Match`.
- **`shelbi status`:** the `## GitHub` section format matches real `shelbi status` / `status --full` output.
- **Rate limiting:** the REST reserve of 100 and its refusal text, the parked and missing-auth error texts, comments with an idempotency marker, and the merged-PR reconcile (`poller:github-merge-reconcile`, 900s, `0` disables) all match.
- **Migration:** `shelbi issue-store migrate` flags match (`--to`, `--dry-run`, `--pace-secs` 7.5).
- **Links:** all resolve.
- **unverified:** "the repo-scoped `repo` token scope covers both" (lines 47-48). Checking GitHub's scope semantics needs a real token and repo; the preflight only checks that issues are readable and labels writable.

### concepts/orchestrator.mdx
- **Categories:** the six-category table matches, categories are fixed while names are customizable, and `in_progress`/`in-progress` are both accepted.
- **Prompt and launch:** the prompt is composed into `.claude/agent-instructions.md`. `ensure_dashboard()` is at `crates/shelbi-orchestrator/src/lib.rs:433`. The Claude, Codex and other runner launch wiring matches (`lib.rs:784-866`), and `orchestrator.runner` is validated.
- **Behavior:** the reaction rules, dispatch order with `prefers_machine` (as prompt policy), "when not to dispatch", the reload self-heal of `## Polling-only event drain`, and the "What the orchestrator does not do" list all match the template.
- **Config root:** both modes are right.
- **Links:** all resolve.

### concepts/remote-sessions.mdx
- **Entry points:** the launch screen, the desktop menu "Connect to Remote Session…", the palette, and `shelbi remote connect <HOST> [PROJECT]`.
- **Dialog:** the copy and the recent-sessions list match (max 8, newest first, deduped by host+user+project).
- **Connection:** the `ssh -t … --project` invocation, the window title `Shelbi · user@host · project`, and the reconnect notice with backoff match. `BatchMode=yes`, there are no password prompts, and host keys are never accepted for you.
- **Remote daemon:** Start Shelbi via `__ensure-daemon`. Every error-table message matches. Binary search, major.minor compatibility, and `shelbi remote list [--json]` are right.
- **Links:** all resolve.

### concepts/review-workspaces.mdx
- **`review:` block:** the fields and semantics match (`workflow.rs:1619-1645`), including `$X`/`${X}` substitution and diff-only when the block is absent.
- **Transition commands:** `run`/`ready`/`ready_timeout` (default 90s) and teardown match.
- **Shipped config:** the `review` agent charter and the shipped `task` review status (`agent: review, tags: [review]`) match.
- **Slots:** `slot: 3000` → `$PORT`=3000. The `hub:review-0` location badge and the Queued `·` badge match.
- **Auto-load:** runs each tick and takes slots in declaration order.
- **Review interface:** the switch labels and glyphs match (🤓/🔀/✍️/🌐). Approve and Reject behave as described, including the reject fallback to `todo`.
- **Commands and editor:** the View Diff command and `review.diff_command` match. Editor resolution and the label rule match.
- **Handoff:** the poller rebases and closes the dev session on handoff. Zen fires on `merge` edges.
- **Collapse callout:** correct.
- **Links:** all resolve.

### concepts/workspaces.mdx
- **Session and worktree:** one session plus one worktree per slot; the worktree path is `<work_dir>/.shelbi/wt/<name>` and is not configurable.
- **Config:** the pool YAML shape (with the `workers:` alias) is right. Tags are a union with shorthand forms, and routing is a set-AND superset with the loud no-match failure.
- **`slot:`:** the index default and the transition env exports match.
- **Sidebar:** collapse state persists in `state.json` `sidebar.collapsed_machines`.
- **Status:** `workspace_poll_interval_secs` defaults to 5, and the `status.yaml` path is right.
- **Badges and events:** `⏵`/`⚠`/`⏸`/`·` are right, and the workspace event line is right.
- **Dispatch:** branch sync aborts on a dirty tree, and the `settings.local.json` self-heal runs before the session reset.
- **Handoff:** the marker path, the poller checks and the event-line format match.
- **Remote:** the requirements, `shelbi relay` and `shelbi attach` match.
- **Links:** all resolve.
- **unverified:** "a workflow can override [the base branch] further" for the dispatch-time branch cut. `sync_worktree` gets `project.base_branch()` (`workspace.rs:2648`), but `lifecycle.rs resolve_base` may pre-create the branch from the workflow base; this was not traced to a conclusion.

### concepts/zen-mode.mdx
- **CLI:** all `shelbi zen` subcommands exist. `zen status` output matches a real run.
- **States:** off and pause semantics are right.
- **`zenmode.md`:** the path in both modes and its reload preservation are right.
- **Heartbeat reminders:** every 3rd heartbeat, plus the hourly re-read cue.
- **Prompt and bootstrap:** the orchestrator `## Zen Mode` pointer section exists, and the bootstrap scan mention is right.
- **Merge bar:** the order and the probe JSON fields match (`zen.rs:6735-6871`).
- **Merge commands:** the `pr-create`/`ci-watch`/`pr-merge` flags, exit codes and outputs match, as does `pr-merge` `--force-with-lease`.
- **Per-workflow `zen:` block:** the semantics and checks precedence are right.
- **Danger paths:** the built-in danger paths table matches (`model.rs:2811-2925`), and `enabled: false` excludes an issue.
- **Daemon:** autoload and the heartbeat defaults are right.
- **Links:** all 20 `/docs/...` targets and in-page anchors resolve.
- **unverified:** the prompt-policy behaviour (what the orchestrator does while Zen is off or paused). It only exists as prompt text; the text matches the templates, but runtime agent behaviour wasn't observed.

### configuration/agents.mdx
- **`agent.yaml` fields:** fields, types and validation match (`crates/shelbi-core/src/agent_manifest.rs:22-150`). A missing manifest is non-fatal and a malformed one is an error. The scaffolded manifest keeps identity active and the knobs commented.
- **Runner kinds and flags:** the kinds and adapter flags are right (Claude `--model`/`--effort`; Codex `-c model_reasoning_effort`, with max→`xhigh`).
- **Project config:** `runners:` keyed by kind is right and bridged to the legacy `agent_runners`. The `agents.<name>.runner` pin is right.
- **Chains and permissions:** the per-field chains are independent. The permission ceiling clamp and mode mapping are right.
- **Legacy:** `workspaces[].runner` is ignored.
- **Anchors:** all except `#agent-runners` resolve.

### configuration/global.mdx
- **`config.yaml`:** the path and the partial-file fallback are right.
- **New installs:** seeding (`theme: dark`, `window_opacity: 90`, `window_blur: 40`) matches.
- **Keys and values:** `keymap.zen_toggle` (default `alt-z`, chord list), `theme` (`auto|dark|light`, default `auto`), and `editor` resolution and labels are right.
- **`keys.yaml`:** the path, `defaults`/`projects.<name>` shape, three-layer resolution, replace-not-union, leaf forms, mode names, and every built-in chord listed all match `shelbi config list-actions`.
- **Other files:** `shelbi.yaml` holds `last_launched`, and `config reset` is described as future (it doesn't exist).
- **Links:** all resolve.
- **unverified:** the live-repaint and "reads config.yaml again whenever Settings opens" claims; the TUI wasn't run.

### configuration/project.mdx
- **Files and identity:** the global vs in-repo file locations are right. The id comes from the filename, with the slug rule and rename error. `display_name` is a deprecated alias.
- **Defaults:** the defaults and required flags for `default_branch`, `machines`, `orchestrator`, `agent_runners`, `runners`, `agents`, `workspaces`, `zen`, `github_url`, `config_mode`, `workspace_poll_interval_secs` (5), `github_reconcile_interval_secs` (900, `0` disables) and `workspace_permissions_mode` are right.
- **Heartbeat:** 60s/5m, `off`, scalar and map forms, and back-off.
- **Disk:** 20/20 GiB, and the `disk_low` line.
- **`git`:** the fields are right, `branch`/`branch_prefix` are mutually exclusive, and `merge_strategy` defaults to squash.
- **Review and tracker:** the `review.diff_command` placeholders and default are right. So are the `issue_tracker` budget defaults (2000/500/100/120/100) and backend validation.
- **Machines and workspaces:** forward/tags and the worktree path are right.
- **Orchestrator channel:** the Codex polling channel is right.
- **PR template:** the paths, scaffold, self-heal, sections and `.shelbi/pr-body.md` handoff match.
- **Links:** all except `#agent-runners` resolve.
- **unverified:** the external GitHub `model.rs` URL (not fetched), and the exact orchestrator launch-flag text at lines 184-189.

### configuration/statuses.mdx
- **File and contents:** the path is right, and the example matches the shipped `statuses.yaml` exactly.
- **Field rules:** match. There are exactly six categories, and done/archived must be terminal.
- **Order:** declaration order is the column order.
- **Settings ▸ Workflows:** the statuses editor behaves as described (`workflow_config.rs:1-80, 486-530`).
- **Links:** all resolve.

### configuration/workflow.mdx
- **Statuses:** the reference-only form is right, and inline name/category fail the load. `owner` and `tags` semantics are right. `description` and `initial_status` are right.
- **`required_params`:** validation is right, and the subtask example matches the shipped `subtask.yaml`.
- **Transitions:** the any-to-any edges, the six actions, short-circuiting, `target` and the merge-bar trigger are right.
- **`run`/`ready`/teardown:** these and the exported vars are right.
- **`review:` fields:** right.
- **`git`:** `base_branch`, `branch`, `branch_prefix` and the fallback login are right.
- **`zen`:** the block and the research example load.
- **Links:** all except `#required-params` resolve.
## Resolution

Fixed on branch `jlong/docs-audit-fixes` (task `docs-audit-fixes`), 2026-10-29, rebased on origin/main 9089caab. Each finding was re-verified against current source and/or the installed `shelbi` 0.11.0 before editing. Commit SHAs are pre-merge; the PR squash-merges them into one commit on `main`. Per-finding status: **fixed**, **skipped (reason)**, or **not reproducible**. `site/app/llms*.txt` and `docs-md` are generated from content at build, so they needed no hand edits. `npm run lint` and `npm run build` pass in `site/`.

### Coordinator follow-ups
- **Starter workspace pool.** The groups disagreed here as well. `shelbi project add`, `shelbi wizard` and `shelbi init -y` write `workspaces: []` (`wizard.rs`). `shelbi init` without `-y` and the app's Add project form (`project_create::scaffold_project`) write `dev` plus a `review`-tagged `review`. concepts/workspaces, cli/init and cli/workspace now describe both paths (concepts/workspaces in `6839f254`).
- **events-log.mdx frontmatter.** The new `summary:` held a bare `: `, which broke the contentlayer YAML parse. It was reworded.

### concepts-a: agents, changelog, config-modes, config-upgrade, events-log, github-issues (commit `2e7d3da0`)


#### concepts/agents.mdx
- "window 1" (table row, prompt paragraph, See also) — fixed (orchestrator session / Orchestrator sidebar row)
- review row: dev server booted by transition commands — fixed (runs the workflow's `review:` recipe, or presents the diff)
- on-disk tree missing `agent.yaml` / `perf/skills/` — fixed (verified in sandbox init; also noted in-repo path `<repo>/.shelbi/agents/`)
- deprecated category-based owner fallback — fixed (removed; bare `owner: agent` fails the load)
- `agent: qa` on the `review` handoff example — fixed (rewrote as a `qa-review` active gate with transitions; "handoff- or active-category" → active-category gate; dispatch attributed to the daemon)
- Settings ▸ Agents not mentioned — fixed (editing + Reset to default)

#### concepts/changelog.mdx
- orchestrator instructions path global-only — fixed (`<config-root>/…` for both modes, plus `shelbi agent edit orchestrator`)

#### concepts/config-modes.mdx
- in-repo tree outdated (default.yaml, two agents, optional _shared, missing .md files) — fixed (verified with sandbox `init --mode in-repo`)
- table row missing zenmode.md / pr-template.md / changelog-template.md — fixed
- walk-up second check (repo root must be registered work_dir) — fixed (reproduced: second clone warns and falls back)
- alias rename advice — fixed (`--pick-up --project <alias>`; rename `~/.shelbi/projects/<alias>/` by hand)
- migration doesn't move the three .md files / "What to commit" — fixed (verified with dry-run plan)
- "every write, move, delete" — fixed (write, move, retire-rename)
- revert steps — fixed (move dirs back, delete local.yaml)
- pick-up transcript — fixed (real sandbox transcript, six agents, "already exists" lines, three .md files in `ls`, spawn hint elided)
- suffixed transcript missing `shelbi project rename` tip — fixed (shown verbatim; prose notes the command doesn't exist)
- `shelbi reload` per-pane respawn — fixed (daemon restart, client re-exec, orchestrator session replaced, workers keep running; See also too)

#### concepts/config-upgrade.mdx
- `auto_heal=<M>` "already applied" — fixed (per-heal disclosure lines; summary counts are the residual; per-scope incl. `global`)
- lifecycle-owned defaults only pr-template.md — fixed (+ changelog-template.md, _shared/preamble.md)
- surfaces list incomplete — fixed
- findings file "for the orchestrator to ingest" — fixed (boot snapshot; orchestrator reads live channel)
- `--apply-finding` applies any approved finding — fixed (only mechanical ones, today ZEN_DANGER_PATHS_CONFLICT; otherwise refused and orchestrator edits the file)
- display_name vs name — fixed (disagrees with both `name:` and the project id)

#### concepts/events-log.mdx
- `$SHELBI_HOME` — fixed (`--root` / `$SHELBI_ROOT`)
- missing `serving` state and dialog/usage-limit reasons — fixed
- dispatch reasons carry `_agent=…`; optional owner/actions/pr tokens — fixed (examples, line shape, table)
- "snake_case status names" — fixed (ids as-is; only `in-progress` → `in_progress`)
- heartbeat disk_low + Zen reminders — fixed
- "active board never sees padding lines" — fixed (bounded skip, at least once per interval)
- `events tail` options (--project, --format, -f, rotated file not read) — fixed
- "filtering is the orchestrator's job" / tails / heartbeat purpose — fixed (claim/ack cursor + `tail --follow --project`; liveness signal)
- "never truncate; rotate via rename" — fixed (Shelbi self-rotates at 8 MiB to events.log.1, index file)
- `user:tui:…` — fixed (`user:tui`)
- reason table missing reasons — fixed (user:app, user:review/-reject, user:cli:add, user:cli:resume, poller:issue-reconcile/-adopt, poller:github-merge-reconcile, zen-promote, zen-merge)
- Zen and CI line shapes — fixed (new "Zen and CI lines" subsection; `ci` lines described per zen.rs: `check=all success` or failing check + `failure`)

#### concepts/github-issues.mdx
- budget bands (100/500, cache below 100) — fixed (three bands; 403/429 park; single fetches are REST)
- ownership gate covers every start, `--take`, owner column/token — fixed. Kept the heading "GitHub assignees and Zen Mode" (audit suggested retitling) because zen-mode.mdx (not my file) links to `#github-assignees-and-zen-mode`
- `app` workflow — fixed (task, subtask, fallback default)
- closed issue terminal mapping — fixed (terminal label wins)
- label colors — fixed (six stock labels colored; full stock set created on first write)
- `shelbi doctor` missing — fixed
- migration drops comments/assignments — fixed

#### Also
- Replaced em dashes with colons in the See also lists of my pages, and in the events-log summary.

#### Product issues noted
- `shelbi init --pick-up` prints a tip pointing at `shelbi project rename`, which doesn't exist (init.rs ~847); both init hints also suggest a nonexistent `shelbi spawn` (init.rs ~229/242). After an in-repo init the `next:` hint points at `~/.shelbi/projects/<name>.yaml`, which doesn't exist in that mode.
- `migrate-to-in-repo` doesn't move zenmode.md / pr-template.md / changelog-template.md, so customized copies are left behind (migrate.rs:46-51).
- `issue_tracker.budget.graphql_low` is parsed but never read.
- Activity feed `parse_zen_reason` splits on a space, but on-disk reasons use `_` (affects how zen-promote/zen-merge rows read; docs now describe the on-disk form).

### concepts-b: orchestrator, remote-sessions, review-workspaces, workspaces, zen-mode (commit `1ebe5a5b`)


#### concepts/orchestrator.mdx
- Bootstrap "live watch" reversed (events next --follow as background watch) — fixed (passive `events tail --follow --project` + per-turn `events next --follow --max-lifetime 2s` / `events ack` self-drain + `config upgrade --needs-judgment`; "three things" reworded)
- Orchestrator routes finished branch onto review workspace — fixed (hub's review auto-loader loads it; orchestrator doesn't drive review loads)
- See also: "the slots the orchestrator loads" — fixed
- Step 3 "right pane of the dashboard window" + frontmatter "window 1" — fixed (`shelbi-<name>` session, state dir as workdir; summary reworded); also intro "dashboard's main pane", "window setup", "pasted into the pane", "respawning the orchestrator pane", "whose pane died"
- Zen policy lives in orchestrator `instructions.md` — fixed (`zenmode.md`; review-routed issues always wait for a human)
- Polling-only event drain uses `events drain` — fixed (events next + ack for every orchestrator; `events drain` / cursor read as legacy fallbacks)
- "developer (and any reviewer roles you author)" / "four instructions.md files" — fixed (six shipped agents; "every agent's")

#### concepts/remote-sessions.mdx
- "Change Host" label — fixed ("Change host")
- Missing Esc-to-stop / reconnect never starts far daemon — fixed (scoped to TUI and `shelbi remote connect`; desktop stop behavior not verified so not claimed)

#### concepts/review-workspaces.mdx
- "Nothing branches on the literal word review" — fixed (lists the built-in review special-casing)
- `$SLOT`/`$PORT` = explicit `slot:`; init's `review` slot has none — fixed (in serve section and `$SLOT` vs `$PORT` section)
- Complete example tags machine `hub` with `review` — fixed (tag only the slot; also fixed the first example on the page the same way, plus a note on machine-tag inheritance)
- Invalid YAML transition line — fixed (flow mapping)
- Ready for Review `✓` only when serving; missing `▶` Loading; `⎇` branch line — fixed
- "Load onto a review workspace?" prompt / loading while busy — fixed ("Load for review" dialog: one slot, several, all busy)
- Review panel "stays docked in the dashboard" / back to dashboard window — fixed (panel takes sidebar column; Back/Esc restores sidebar + view, leaves review loaded)
- Folder glyph `📂`, missing task-info block, remote slot options — fixed (`📁`, task info + More, remote shows only Reviewer + Open Browser)
- Extra (not in audit): `workflows/default.yaml` → `workflows/task.yaml`; "The orchestrator routes it" (line 44 + flow step 2) → hub auto-loader; added note that shipped Zen policy never auto-merges review-routed issues

#### concepts/workspaces.mdx
- "machine + pane + worktree" — fixed
- Wizard sizes pool from RAM — fixed (starter pool `dev` + `review` on `hub`)
- Add/remove by editing YAML + reload — fixed (`shelbi workspace add/rm`, Settings ▸ Workspaces; hand-edit + reload claim dropped as unverified)
- "Nothing in the routing branches on a literal tag name" — fixed
- Tags example with machine-level `review` — fixed (neutral `gpu` machine tag + warning about machine-level `review`)
- `$SLOT` example with review slots on slot 0/1 — fixed (dev slots; note that review slots need an explicit port-valued `slot:`)
- Sidebar row example / description (issue column, `— hub —` headers) — fixed (audit's example; headers only with >1 machine)
- Collapsed `(total, active)` — fixed (`(total, N active)`)
- `prefers_machine` honored/surfaced on card — fixed (prompt policy only; `--no-prefers-machine`; card claim dropped)
- `💬` awaiting_input badge; missing `serving` and `…` — fixed (`?`, added `serving` ⏵ and `…`)
- Pane killed and re-created, window/session steps — fixed (session ended and re-created)
- Prompt typed after `shift+tab to cycle` readiness — fixed (passed at launch, busy confirm, Claude-only typed fallback)
- Handoff "moves to next handoff status" — fixed (rebase, push, agent-owned gate vs first handoff, edge actions)
- "Agent never runs shelbi / marker is entire protocol" — fixed
- See also `shelbi workspace` "list and stop" — fixed
- Extra: "the orchestrator then loads the promoted branch" → hub's review auto-loader

#### concepts/zen-mode.mdx
- Crash-recovery callout (heartbeat wrapper auto-disables Zen) — fixed (documents current behavior: Zen stays on; bootstrap scan note qualified as legacy)
- First-run hotkey probe — fixed (replaced with "Rebinding the hotkey": `global.zen_toggle`, Settings ▸ Keybindings, `defaults.global.zen_toggle` in keys.yaml)
- "ZEN ON pill" — fixed ("ZEN MODE ON" band; desktop "Zen mode on")
- Missing palette/menu toggles + first-enable dialog — fixed (dropped audit's `reason=user:palette` claim: TUI hotkey path logs `user:hotkey`, palette path `user:palette`)
- Verbatim zenmode.md block drifted — fixed (block replaced with template verbatim; diff now empty)
- `ci_timeout: 15m` / duration-string comment — fixed (`900` seconds in both examples and schema comment; ci-watch resolution text reworded)
- Policy attributed to "the orchestrator's prompt" — fixed at all spots (two-paths section, diff-size tunable, mechanical I/O line, judgment layer, per-status automation section)
- `zen scan` remote-tracker ownership filter — fixed
- `enabled: true` opts in when Zen off — fixed (only affects in-flight count)
- ZEN avatar badge + tinted rows — fixed (`[zen]` chip)
- Reason phrasing / `_` folding — fixed (only `orchestrator:zen-merge` gets dedicated phrasing; others generic with `[zen]` chip)
- Missing `--match-published-head-commit` — fixed
- Extra: `workflows/default.yaml` → `task.yaml` in QA example; "project's `default` workflow" → `task`; landing step now mentions the changelog entry

Counts: 44 audit findings, 44 fixed, 0 skipped, 0 not reproducible.

#### Product issues noted
- Zen crash recovery is dead code: `__zen-orch-start` / `__zen-heartbeat` / `__zen-orch-exit` have no caller in the session model; a crashed orchestrator is relaunched with Zen unchanged. Stale comment at `crates/shelbi-orchestrator/src/poller.rs:348-349`. Orchestrator bootstrap still scans for `zen=off reason=crash-recovery` lines that are never written now.
- `zen.enabled: true` per-issue does nothing beyond the `zen status` in-flight count.
- `parse_zen_reason` splits on space but on-disk reasons use `_`, so `zen-promote`, `zen:failed-checks`, etc. never get their dedicated Activity phrasing.
- `prefers_machine` is enforced only by orchestrator prompt policy; no Rust dispatch path (including deterministic gate dispatch) reads it, and no UI shows it.
- The starter `review` workspace written by `shelbi init` / Add Project has no `slot:`, so any workflow `review:` recipe using `$SLOT`/`$PORT` is unresolved out of the box and the Review agent stops.
- Orchestrator template "## Review workspaces" / handoff rule still says the finished issue waits "for a human to load it onto a review slot from the sidebar", while the hub auto-loader now does it.
- `shelbi reload --help` and `shelbi workspace stop --help` still talk about tmux panes.

### configuration: agents, global, project, statuses, workflow (commit `25e96998`)


Files: site/content/docs/configuration/{agents,global,project,statuses,workflow}.mdx
YAML examples validated in a throwaway SHELBI_ROOT (`shelbi init -y`, `config lint`, `config upgrade`, `config check`, `workflow show`, `issue show`).

#### configuration/agents.mdx
- broken `#agent-runners` links (x2) — fixed (`#agent_runners`)
- resolution chains omit task `launch:` tier — fixed (both chains + permission note + validated `launch:` example)
- project runners table lacks `prompt_injection` / `integration` — fixed
- no Settings ▸ Agents — fixed (new "Editing from Settings" section; ⌘6 / `,` then `6`)
- path assumes global mode — fixed (`<config-root>/agents/<name>/`)
- `shelbi agent new` creates no agent.yaml — fixed (verified in sandbox)

#### configuration/global.mdx
- no `desktop:` block — fixed (new Desktop section, all keys)
- root resolution misses install-time baked default — fixed
- `editor` scope too narrow — fixed (review + workspace panel + desktop external edits)
- example writes legacy `keymap.zen_toggle` — fixed (dropped from example; row marked legacy w/ migration note)
- keys.yaml "TUI keybindings" — fixed (shared by both clients, Settings ▸ Keybindings)
- actions table missing rebindable actions — fixed (9 added) + pointers to `config list-actions` / `dump-keybindings` / `check`
- `move_card_left: [H, ctrl-h]` dead binding — fixed (`alt-h`; added shadowing note; `config check` clean)

#### configuration/project.mdx
- broken `#agent-runners` links (x2) — fixed
- `repo` is a local path, not clone URL — fixed (row + example)
- `editor` unused — fixed (documented as reserved/unused)
- `default_workflow` default `default` — fixed (`task`, reload migration, alias)
- missing `session` / `workspace_settings_template` — fixed
- agent_runners missing `prompt_injection` / `integration` — fixed
- Claude hooks `.claude/settings.json` + `claude.*` — fixed (`settings.local.json`, `*.sh` hook names)
- tag / danger_paths shorthands — fixed (still load, auto-rewritten by config upgrade; example comment updated)
- `host` "required" — fixed
- branch resolution order — fixed (5-step list)
- committed `name:` vs local id — fixed (canonical id; local id may be suffixed alias)
- extra (theme 5): machine example `tags: [review]` — fixed (`gpu`; machine-level review tag warning in row); review slot example `slot: 0` → `slot: 3000` and slot row notes review recipe port = explicit slot
- extra (theme 1/3): "dashboard/orchestrator pane", "workspace pane", heartbeat "events tail watch" wording — fixed
- extra: `merge_strategy: rebase` example/row — fixed (merge action rejects rebase; example uses `merge`)
- extra (app issue 7): `graphql_low` unused — fixed (row says not read; describes the two real thresholds)
- extra (app issue 1): `review:` misreported as removed key — fixed (short note; reproduced in sandbox)
- extra: zen example `cargo test --workspace` triggered ZEN_CARGO_TEST_NO_FAIL_FAST — fixed (`--no-fail-fast`)
- cli.md: link `/docs/cli/msrv-check` from `zen.checks.local` — fixed (row link + example entry)

#### configuration/statuses.mdx
- `active` listed as single-instance warning category — fixed

#### configuration/workflow.mdx
- virtual `default` workflow claim — fixed (init writes task + subtask; built-in default only when no files; alias)
- `default.yaml` example w/ no transitions — fixed (replaced with scaffolded task.yaml; validated)
- "Order is the column order" — fixed (statuses.yaml order; first entry = default initial_status)
- missing `review` top-level row — fixed
- broken `#required-params` link — fixed (`#required_params`)
- `agent` default derived from category — fixed (required for `owner: agent`; verified load error)
- transitions missing `url` — fixed
- git `merge_strategy` inheritance — fixed (documented as not consulted; project's value is used)
- `name:` "must match" — fixed (resolved by filename, mismatch warns, duplicates error)
- no Settings ▸ Workflows — fixed (new "Editing from Settings" section)
- extra (theme 5): literal `review` tag special-casing — fixed (one sentence in tags callout)
- extra: subtask `base_branch: task/{{task}}` "the parent task's branch" — fixed (documents literal `task/<parent-id>` and mismatch with `{{github_user}}/{{id}}`)
- cli.md: link `/docs/cli/action` from transitions section — fixed

Counts: 35 audit findings fixed, 0 skipped, 0 not reproducible (+ 2 cli.md link requests, + extras above).

#### Product issues noted
- Subtask `base_branch: task/{{task}}` never matches the parent's shipped branch name `{{github_user}}/{{id}}`.
- Per-workflow `git.merge_strategy` is ignored by the `merge` action (uses project value); `rebase` parses but is rejected.
- `REMOVED_PROJECT_KEYS` contains `review`, so config upgrade flags the live `review: { diff_command }` block (and would auto-drop an inert one).
- `issue_tracker.budget.graphql_low` is parsed but never read.
- Project `editor` field is parsed/split into local.yaml but never read.
- Config upgrade auto-heals the committed in-repo `name:` slug to the label, breaking `--pick-up` (not documented; doc describes intended behavior).

