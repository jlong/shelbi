# Docs audit: Guides, AI prompts, changelog, and docs mockups
Audited at origin/main a749b0d1 (2026-10-29), shelbi 0.11.0 (installed binary; workspace version 0.11.0), 2026-10-29.

## Summary
132 findings: **61 wrong**, **23 outdated**, **20 missing**, **23 unclear**, **1 broken-link**, **4 unverified**.

Pages audited, all under `site/content/docs/`:
- `guides/getting-started/`: index, install, first-project, first-task, workflows, custom-workflow, multi-workspace, review-workspaces, enable-zen-mode
- `guides/understanding-workflows/`: index, trunk-based, feature-branch, git-flow, forking
- `guides/doing-more-with-agents/`: index, add-to-workflow, adversarial-review-agent
- `ai-prompts.mdx` and `changelog.mdx`
- The docs mockup: `site/components/KanbanMockup.tsx` (`AppMockup`) via `site/components/mdx-components.tsx`, plus every `<AppMockup>` usage in the guides

Method:
- Read each claim against an extracted snapshot of origin/main (`crates/`, `desktop/src`, `site/`).
- Ran the installed `shelbi` read-only (`--help` on every command and flag mentioned, `shelbi status`, `workflow list/show`, `agent list`, `config list-actions`, `zen status`, `events tail`).
- Validated every workflow/project YAML example by loading it in a throwaway `SHELBI_ROOT`.
- Checked the changelog against git tags and `git log` ranges.
- Checked internal links and anchors with a script against github-slugger heading slugs.
- Re-checked the highest-impact claims directly in source before recording them. Those findings are marked "verified" in their evidence.

### Cross-cutting themes (fix these once, then sweep the pages)
1. **The launch screen replaced the first-run setup flow.** Plain `shelbi` on a fresh machine (no daemon, interactive terminal) now opens a "Welcome to Shelbi" launch screen with Add project and Connect to remote (#1985, landed 2026-10-29). The preflight-and-plan-card flow the guides describe now runs only via `shelbi project add`, or when a daemon is already up. This affects index, install, first-project, first-task and ai-prompts. The owner may want to decide whether this is the intended UX before the docs are rewritten around it.
2. **The sidebar nav is now Orchestrator / Issues / Changelog**, not Chat/Issues/Activity or Chat/Tasks/Activity. Activity and Machines moved to the palette. This affects first-task, enable-zen-mode, and the `AppMockup` `NAV_ITEMS`.
3. **The shipped workflow is `task`, not `default`.** Several pages still show `workflows/default.yaml` with `review: { agent: orchestrator }` and no transitions. The real `task.yaml` has `review: { agent: review, tags: [review] }`, a `git:` block (`base_branch: main`, `branch: '{{github_user}}/{{id}}'`, squash), and four active transitions, including two `→ canceled` edges. Affected: feature-branch, add-to-workflow, git-flow, workflows.
4. **Branch names are `{{github_user}}/{{id}}`** (e.g. `you/fix-login`), not `shelbi/<slug>`. This affects first-task, custom-workflow, three understanding-workflows mockups, and the hard-coded card meta in `KanbanMockup.tsx`.
5. **Six statuses, including Canceled (archived).** Boards show six columns, with empty ones folded to a strip. Examples that rewrite `statuses.yaml` without `done`/`canceled` break the shipped `task`/`subtask` workflows, and with them the whole project load (git-flow, forking, add-to-workflow).
6. **Review flow.**
   - The `in-progress → review` transition pushes and opens a PR.
   - The card appears under *Queued for Review*.
   - The **hub daemon** auto-loads it onto a `review`-tagged slot; the orchestrator never does. The Review agent serves it there, and it then shows under *Ready for Review* with `✓ machine:workspace`.
   - Moving the card to Done (or ✅ Approve) runs the merge through the `review → done` transition.
   - Affected: first-task, multi-workspace, review-workspaces, workflows and enable-zen-mode. All of them describe an orchestrator-driven review, or a "merge it" step followed by a manual move.
7. **The ready-marker handoff only fires from status id `in-progress`**, so custom active statuses (`staging`, `in-review`) and gates must use the transition marker or keep the `in-progress` id. Workflows with no handoff status merge immediately, with no Zen gate. Affected: trunk-based, git-flow, forking, adversarial-review-agent. Verified at `crates/shelbi-orchestrator/src/poller.rs:2858-2859`.
8. **Orchestrator prompt plumbing changed.**
   - The composed prompt goes to `.claude/agent-instructions.md` via `--append-system-prompt`, not `CLAUDE.md`.
   - Zen judgment categories and merge conditions live in `zenmode.md`, not the orchestrator prompt.
   - The bootstrap is `shelbi status --full --handoff` plus the `orchestrator events next/ack` drain.
   - Affected: custom-workflow and enable-zen-mode.
9. **`depends_on` is display-only.** `Issue::is_blocked` drives only the 🔒 badge (TUI board, desktop, `issue list/show`). No dispatch path, poller path, `status --full` field, or orchestrator-prompt rule gates on it. Two sub-audits disagreed on this, and it was resolved by grepping every `is_blocked` caller. Affected: workflows, first-task.
10. **The `AppMockup` `review={[]}` prop is silently ignored** (the fields are `readyReview`/`queuedReview`). Every guide mockup that passes it inherits the default preset's Zen band, five phantom review entries and an "app" workflow badge.
11. **Possible product bugs found along the way** (for the owner, not the docs-fix task):
    - `subtask.yaml`'s `base_branch: task/{{task}}` doesn't match the parent's `{{github_user}}/{{id}}` branch (workflows.mdx).
    - The wizard's Customize answer for "Agent runner (used by every workspace)" is never saved (first-project.mdx).
    - Zen crash-recovery hooks (`__zen-orch-start/heartbeat/exit`) appear unwired after tmux removal (enable-zen-mode.mdx).

## Findings
Severity tags: wrong = never true, or no longer true in a way that misleads; outdated = was true and has since changed; missing = shipped behavior the page omits; unclear = misleading wording; unverified = could not confirm (reason given). Line numbers are from origin/main a749b0d1.

### guides/getting-started/index.mdx
- [severity: outdated] "Shelbi checks the repo, runner, and machine, then shows one detected plan. Press Enter once to write it and open the dashboard, or press `c`…" (lines 28-30)
  - Reality: on a fresh machine, `shelbi` opens the launch screen (see install.mdx). Even in the setup flow, Shelbi always asks the issue-tracker question before showing the card, and if both runners are installed it asks the runner question too.
  - Evidence: `crates/shelbi-cli/src/main.rs:636-666` (verified); `crates/shelbi-app/src/launch/route.rs:13-19,59-67`; `crates/shelbi-cli/src/wizard.rs:350-363`.
  - Fix: "…then asks where issues should live and shows one detected plan. Press Enter to write it and open the dashboard…". Add the launch-screen step once install.mdx is settled.

### guides/getting-started/install.mdx
- [severity: outdated] "brew install jlong/shelbi/shelbi && shelbi … Shelbi checks your prerequisites and current Git checkout before it writes anything" (lines 9-17); "With no project configured, this starts the guided setup." (lines 106-107)
  - Reality: plain `shelbi` outside a registered project opens the TUI **launch screen** when no hub daemon is running and stdin/stdout are a terminal, which is the normal fresh-install case. The screen shows "Welcome to Shelbi", "Open a project on this Mac or connect to Shelbi on another machine.", and **Add project** / **Connect to remote** buttons. Add project opens a separate "Add a project" form (name, repo path, config location) that creates a file-system board, with no preflight, runner question or plan card. The preflight + plan-card flow runs only when a daemon is already up, the session is non-interactive, or you run `shelbi project add`. Landed in #1985 (66849762, 2026-10-29).
  - Evidence: `crates/shelbi-cli/src/main.rs:636-666` (`shows_launch_screen(daemon_state(), is_interactive())`, verified); `crates/shelbi-app/src/launch/route.rs:13-19`; `crates/shelbi-tui/src/launch/screen.rs:10-40`; `crates/shelbi-tui/src/launch/mod.rs:271-281` (scaffold with `IssueTrackerConfig::default()`); `crates/shelbi-app/src/overlay/add_project.rs:22-25`.
  - Fix: Describe the launch screen and its Add project form, or point readers at `shelbi project add` (run inside the repo) for the detected-plan card. Consider raising the divergence with the owner as a possible app regression, not just a docs fix.
- [severity: wrong] "Declare more than one later, and the project YAML picks per workspace." (lines 70-71)
  - Reality: workspaces no longer pick a runner. A workspace's `runner:` key is read and ignored. Each agent's manifest recommends a runner, and the project config can override it.
  - Evidence: `crates/shelbi-core/src/model.rs:1513-1522`; `crates/shelbi-core/src/agent_manifest.rs:1-11`.
  - Fix: "Both runners are declared in the project YAML; each agent (orchestrator, developer, review, …) resolves which runner it uses."
- [severity: missing] No mention of the macOS desktop app.
  - Reality: the tap also ships a `shelbi-desktop` cask (Apple silicon, macOS 15+, depends on the CLI formula).
  - Evidence: repo `README.md` "macOS: the desktop app"; `docs/release/homebrew-tap.md` "Desktop App Cask"; `desktop/README.md`.
  - Fix: Add a macOS desktop tab or note: `brew install jlong/shelbi/shelbi jlong/shelbi/shelbi-desktop`.
- [severity: unclear] "The package installs the `shelbi` binary only." (line 57)
  - Reality: the .deb also installs `/usr/share/shelbi/plugins/update-shelbi-configuration/…` and docs. The Homebrew formula installs the same system plugin assets.
  - Evidence: `.goreleaser.yaml` nfpms `contents`; `docs/release/homebrew-tap.md` "Formula Shape".
  - Fix: "The package installs the `shelbi` binary and its bundled configuration plugin; there is no service to install or enable."
- [severity: missing] "The script runs `cargo build --release` and copies the binary to `$HOME/bin/shelbi`." (lines 160-161)
  - Reality: `scripts/install.sh` also:
    - runs `cargo fetch --locked`;
    - asks interactively for the Shelbi root directory and bakes it into the binary (default `~/.shelbi` with no TTY, e.g. `curl | sh`);
    - installs the system plugin to `<prefix>/share/shelbi/plugins/update-shelbi-configuration`;
    - restarts an already-running hub daemon onto the new binary (`--no-daemon` skips this).
  - Evidence: `scripts/install.sh` at a749b0d1 (root prompt ~lines 55-110; fetch/build/plugin/codesign; daemon restart at the end).
  - Fix: Mention the root-directory prompt, `SHELBI_DEFAULT_ROOT`, the plugin install, and the automatic daemon restart / `--no-daemon`.
- [severity: outdated] "The orchestrator and workspace panes re-shell into `shelbi` on every call…; only the sidebar, Issues, and Activity views need a manual `shelbi reload`" (lines 168-170)
  - Reality: this is tmux-era wording. The sidebar, Issues and Activity views now run inside the TUI process. On the session runtime, `shelbi reload` restarts the daemon, re-execs attached clients and replaces the orchestrator session, and the install script already restarts a running daemon.
  - Evidence: `crates/shelbi-cli/src/commands/reload.rs:35-40`; `scripts/install.sh` daemon-restart block.
  - Fix: "The script restarts a running hub daemon onto the new build; run `shelbi reload` (or restart the TUI) so attached clients and the orchestrator pick it up."

### guides/getting-started/first-project.mdx
- [severity: outdated] "brew install … && shelbi … On the happy path, Shelbi asks one question: launch the detected plan?" (lines 9-16)
  - Reality: on a fresh machine, `shelbi` shows the launch screen (see install.mdx). Inside the setup flow, Shelbi always asks the issue-tracker question before the card, plus the runner question when both runners are on PATH. It is never just one question.
  - Evidence: `crates/shelbi-cli/src/wizard.rs:350-363`; `crates/shelbi-cli/src/main.rs:636-666`.
  - Fix: "From the repo, run `shelbi project add` (or Add project on the launch screen)… Shelbi asks where issues live, then shows one plan card to confirm."
- [severity: wrong] Preflight sample "✓ remote  github.com:you/myapp.git" / "✓ machine  10 cores, recommending 4 workspaces", and "and a workspace count suited to the machine" (lines 26-33)
  - Reality: the remote is shown normalized, so `git@github.com:you/myapp.git` becomes `github.com/you/myapp`. The machine row shows only the core count (e.g. `10 cores`), with no workspace recommendation. With both runners installed, the agent row reads `claude X + codex Y on PATH`.
  - Evidence: `crates/shelbi-cli/src/wizard.rs:880-947` (`detect_snapshot`), `:1646-1663` (`display_remote`).
  - Fix: Use `✓ remote  github.com/you/myapp` and `✓ machine  10 cores`; replace "and a workspace count suited to the machine" with "and the machine's core count".
- [severity: unclear] The issue-tracker prompt mock shows inline descriptions (lines 54-58).
  - Reality: Shelbi first prints "Where should this project's issues live?" followed by two lines: "GitHub Issues  cards are GitHub issues your team can see and file (requires `gh` logged in)" and "File system    private markdown files on this machine (no GitHub needed)". Then it shows a bare `Issue tracker:` select with just "GitHub Issues" / "File system".
  - Evidence: `crates/shelbi-cli/src/issue_tracker_setup.rs:271-292`.
  - Fix: Mirror that output.
- [severity: wrong] Card rows "workspaces  created on first boot" and "github  github.com:you/myapp.git" (lines 85-88); "The displayed plan includes the local `hub`" (line 103)
  - Reality: the workspaces row reads `created at first boot (orchestrator interview)`, and the github row is normalized (`github.com/you/myapp`). On a file-system board the issues row reads `file system (local markdown)`. The card has no hub row: the `hub` machine is written to the YAML but not shown.
  - Evidence: `crates/shelbi-cli/src/wizard.rs:1474-1506` (`render_plan_card`), `:1525-1534`, `:183-191`.
  - Fix: Update the card text. Line 103: "The written plan also declares the local `hub` machine, the shipped `task` and `subtask` workflows, and the default agent roles."
- [severity: unclear] "the detected runner is the one the orchestrator uses for the workspaces it creates" (lines 104-106)
  - Reality: the detected runner is saved as `orchestrator.runner`. Workspaces carry no runner, and agents resolve their own. Possible app bug for the owner to confirm: the Customize answer to "Agent runner (used by every workspace)" (`selected_runner`) is never read by `to_project()`, so only the orchestrator runner is saved.
  - Evidence: `crates/shelbi-cli/src/wizard.rs:183-257`.
  - Fix: "The detected runner becomes the orchestrator's runner; both runner declarations stay available so agents can use either."
- [severity: unclear] "the sidebar briefly shows: Ctrl+P palette · type E to edit settings" (lines 136-141)
  - Reality: the hint is armed only when creating the machine's first project, and it shows once. A later `shelbi project add` won't show it.
  - Evidence: `crates/shelbi-cli/src/wizard.rs:590-592` (`is_first_registration` → `arm_first_run_hint`); `crates/shelbi-tui/src/shell/sidebar.rs:50,1259-1262`.
  - Fix: "…the first time you set up a project on this machine, the sidebar shows…"
- [severity: unverified] "The new board contains one Backlog card named Welcome to Shelbi" (lines 136-137) when GitHub Issues is chosen
  - Reality: `scaffold_welcome_task` always writes a local markdown file (`tasks/<id>.md`). Not confirmed whether a GitHub-backed board ever shows it.
  - Evidence: `crates/shelbi-state/src/lib.rs:3044-3067`, `:2977-2980`.
  - Fix: Check against a GitHub-backed project; if the card doesn't appear, qualify the sentence to file-system boards.
- [severity: unclear] Missing-runner output (lines 175-180)
  - Reality: the `✗ agent` row is followed by `✓ machine  N cores`. The message is "No supported agent runner was found on PATH. Install and authenticate Claude Code (https://docs.claude.com/en/docs/claude-code) or Codex (npm install -g @openai/codex), then start Shelbi again."
  - Evidence: `crates/shelbi-cli/src/wizard.rs:927-945,1027-1034`.
  - Fix: Paste the exact message.

### guides/getting-started/first-task.mdx
- [severity: wrong] "With one project configured, `shelbi` launches its TUI directly. With more than one, you get a fuzzy picker: type to filter, Enter to launch." (lines 22-23)
  - Reality: there is no fuzzy project picker. Inside a registered project's `work_dir` (or with `-p`), `shelbi` opens that project. Outside one with no daemon running, it shows the launch screen (a "Projects on this Mac" list; ↑↓ to move, ⏎ to open). Outside one with a daemon running, it starts project setup for the current directory.
  - Evidence: `crates/shelbi-cli/src/main.rs:636-666`; `crates/shelbi-app/src/launch/route.rs:13-19`; `crates/shelbi-tui/src/launch/screen.rs:10-40`.
  - Fix: Replace with the launch-screen description.
- [severity: wrong] "the nav (Chat, Issues, Activity)… The right pane shows a live session: the orchestrator agent by default, or one of the built-in views (Chat / Issues / Activity)" (lines 33-37)
  - Reality: the sidebar nav is **Orchestrator, Issues, Changelog**. Activity and Machines are reachable only from the palette. A project opens on the **Issues** board by default.
  - Evidence: `crates/shelbi-app/src/view.rs:365-383` (`default_nav_items`, verified); `crates/shelbi-app/src/nav.rs:33-37`.
  - Fix: "…the nav (Orchestrator, Issues, Changelog)… The main pane opens on the Issues board; Orchestrator switches to the orchestrator session; Activity and Machines are in the palette (Ctrl+P)."
- [severity: wrong] Branch name `shelbi/fix-login-bug-on-safari` (lines 59, 121, 134)
  - Reality: the shipped `task` workflow names branches from the template `{{github_user}}/{{id}}` (e.g. `jlong/fix-login-bug-on-safari`), falling back to `user/<id>` without a GitHub login.
  - Evidence: `crates/shelbi-core/src/workflow.rs:1020-1026`; `crates/shelbi-orchestrator/src/branch.rs:12,64-77`.
  - Fix: Use `you/fix-login-bug-on-safari` and say once that `you` is your GitHub login.
- [severity: wrong] "You land on the 5-column Kanban board" (line 95), plus the mockup's five columns (lines 145-169)
  - Reality: the default board has **six** columns: BACKLOG, TO DO, IN PROGRESS, REVIEW, DONE, CANCELED. CANCELED is the archived category (dark gray, glyph ⊘) and folds to a narrow strip when empty.
  - Evidence: `crates/shelbi-core/src/workflow.rs:940-990`; `crates/shelbi-core/src/statuses.rs:186-230`; `crates/shelbi-tui/src/kanban.rs:5788-5807` (test: six columns incl. Canceled); `crates/shelbi-app/src/board.rs:59-68`.
  - Fix: "the Kanban board (Backlog → Todo → In Progress → Review → Done, plus a Canceled lane)". Adding a CANCELED strip to the mockup needs a component change (see KanbanMockup findings).
- [severity: wrong] `<AppMockup … review={[]} />` (lines 141-182)
  - Reality: `review` is not an `AppState` field (the fields are `readyReview`/`queuedReview`), so it's ignored. The mockup inherits the default preset's 3 Ready and 2 Queued phantom review entries (Cold-start cache, CSV import fix, …), a "ZEN MODE ON" band (`zenMode: true`) on a page before Zen is introduced, and an "app" workflow badge. See the KanbanMockup section for the full list.
  - Evidence: `site/components/KanbanMockup.tsx:196-213` (AppState), `:247-256`, `:2001-2019` (shallow merge over `PRESETS.default`), `:2117-2237` (default preset).
  - Fix: Pass `readyReview={[]} queuedReview={[]} zenMode={false}` (and drop or correct the workflow badge).
- [severity: wrong] Review section: "— Ready for Review — / ✓ fix-login delta" and "Click that row… to focus the workspace's window, where the finished branch is checked out…; you can ask for edits" (lines 199-213); "that's where the `✓` you see below comes from" (lines 192-195)
  - Reality:
    - When the card enters Review, the dev workspace's session is torn down. The shipped `in-progress → review` transition pushes the branch and opens a PR.
    - The card first appears under **Queued for Review** as `· <title>` / `  ⎇ <branch>`. The `✓` (plus `machine:workspace`) appears only while it's served on a `review`-tagged workspace.
    - Enter on a queued row opens a "Load for review" dialog. A fresh project has no review-tagged workspace (the first-boot interview creates none), so it says "No review workspace is configured."
    - No dev workspace with the branch checked out is left to talk to.
  - Evidence: `crates/shelbi-core/src/workflow.rs:964-973,994-998`; `crates/shelbi-app/src/view.rs:192-215`; `crates/shelbi-tui/src/shell/sidebar.rs:950-993`; `crates/shelbi-app/src/overlay/review_confirm.rs:1-26`; `crates/shelbi-state/src/default_orchestrator.md.template:73-82,328-366`.
  - Fix: Rewrite the section: Shelbi pushes the branch and opens a PR; the card shows under "Queued for Review" (`· Title` / `⎇ branch`); review the PR diff, or set up a review workspace (link to review-workspaces) and press Enter to load and serve it, at which point it moves to "Ready for Review" with `✓ host:workspace`.
- [severity: wrong] "Two flavors of accept… **Move the card to Done.** The orchestrator never auto-completes review. You confirm the merge landed and move the card yourself" (lines 222-247); callout "The orchestrator squash-merges on request but never marks an issue Done for you." (lines 249-256)
  - Reality:
    - In the shipped `task` workflow, `review → done` runs `push_branch` + `merge`, so moving the card to Done (`L` on the board, or `shelbi issue move … --to done`) **is** the merge.
    - When asked to merge, the orchestrator runs `shelbi action merge <id>` and then moves each merged issue to `done` itself.
    - The merge targets the workflow's `git.base_branch` (literal `main` in the shipped task.yaml), not the project `default_branch`. Without a PR, the hub runs `git merge --squash` and pushes.
  - Evidence: `crates/shelbi-core/src/workflow.rs:999-1008,1019-1022`; `crates/shelbi-state/src/default_orchestrator.md.template:873-904`; `shelbi action merge --help` ("absent, the merge lands on the task's workflow `git.base_branch`…").
  - Fix: Merge the two flavors into one: "Accept by moving the card to Done: `L` from Review, `shelbi issue move <id> --to done`, or ask the orchestrator to 'merge it'. The move runs the workflow's merge: through the PR via `gh` if one is open, otherwise a hub-side squash-merge into the workflow's base branch (`main` by default)." Reword the callout to say the human decides when the issue reaches Done.
- [severity: unverified] "`--depends-on`… The orchestrator skips a todo card until everything it depends on is in `done`." (lines 87-88)
  - Reality: conflicting evidence. This auditor found no gating in the orchestrator prompt or `dispatch.rs`. The workflows.mdx auditor found `Issue::is_blocked` (`crates/shelbi-core/src/model.rs:2247-2256`) requires each dependency to be exactly status id `done`, plus a 🔒 badge on the board (`crates/shelbi-tui/src/kanban.rs:2623`). Whether dispatch consults `is_blocked` is unconfirmed.
  - Evidence: above; `shelbi action --help` (merge auto-restacks dependents).
  - Fix: Confirm where `is_blocked` gates dispatch. If it does, keep the sentence and add "(🔒 on the card)". If not, say: "`--depends-on` records the dependency; Shelbi cuts the branch from the parent and restacks it when the parent merges."
- [severity: unclear] "`--prefers-machine` — soft hint; the orchestrator routes to a free workspace on that machine when one's available." (lines 89-90)
  - Reality: the default orchestrator **restricts** dispatch to that machine; if nothing there is free, the card waits in Todo.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:443-452`.
  - Fix: "…the orchestrator only dispatches it to a workspace on that machine, waiting in Todo until one is free."
- [severity: missing] Kanban key table (lines 97-105)
  - Reality: the listed keys are correct, but the table omits `Tab` (cycle workflow filter), `o` (open the card's workspace), `/` (search) and `f` (filter).
  - Evidence: `shelbi config list-actions` (kanban rows); `crates/shelbi-tui/src/handlers/kanban.rs:94-105`.
  - Fix: Add those rows.
### guides/getting-started/workflows.mdx
Every YAML example on this page was loaded in a throwaway root with `shelbi workflow show/list` (the real `load_workflow`) and passes validation. These examples were tested: the statuses catalog, default.yaml, the owners example, the target example, both feature-task variants, feature-release, task.yaml and subtask.yaml. The findings below are about claims around them.
- [severity: wrong] "The default workflow renders as the canonical five-column board, one column per category" (lines 25-26)
  - Reality: the board shows one column per **status** in `statuses.yaml` order, not one per category. The shipped catalog has six statuses, including `Canceled` (archived). Empty columns fold to a narrow strip, so Canceled usually shows as one.
  - Evidence: `crates/shelbi-tui/src/kanban.rs:5905-5925` (test "canceled stays visible but empty"); `crates/shelbi-app/src/board.rs:461` (`ColumnExpansion::Auto.is_collapsed(0)`); `crates/shelbi-core/src/statuses.rs:186-230`.
  - Fix: "The board renders one column per status in `statuses.yaml` order (six for the shipped catalog: Backlog, Todo, In Progress, Review, Done, Canceled; empty columns fold to a narrow strip), each header colored by its status's category."
- [severity: outdated] `<AppMockup terminalTitle="jlong@hub — shelbi" />` (line 28)
  - Reality: the default preset has several problems. Its nav is Chat/Tasks/Activity, but the real nav is Orchestrator/Issues/Changelog. It draws five columns with no Canceled strip. It shows Zen on and an "app" workflow badge. It lists review slots under Workspaces. Its sidebar project is "my-project", which contradicts the "— shelbi" title.
  - Evidence: `crates/shelbi-app/src/view.rs:365-383`; `site/components/KanbanMockup.tsx:320-323,2117-2230`.
  - Fix: Render the starter/default-workflow state explicitly (six columns with CANCELED collapsed, `zenMode={false}`, `project="shelbi"` or no custom title) after the component fixes listed under KanbanMockup.
- [severity: missing] "Workflow files live at `~/.shelbi/projects/<project>/workflows/<name>.yaml`." (lines 31-32)
  - Reality: that path is for global mode only. In-repo projects use `<repo>/.shelbi/workflows/`.
  - Evidence: `crates/shelbi-state/src/workflows.rs:66-72` (`workflows_dir` via `config_project_dir`).
  - Fix: Append "(or `<repo>/.shelbi/workflows/<name>.yaml` for an in-repo project)".
- [severity: missing] "If the project field is omitted, Shelbi falls back to the built-in `default`." (line 53; same idea at 502-503, 612-616)
  - Reality: the name `default` is an alias. If `workflows/default.yaml` doesn't exist but `task.yaml` does, `default` (in `default_workflow:` or an issue's `workflow: default`) resolves to `task`. The in-memory built-in is used only when neither file exists.
  - Evidence: `crates/shelbi-state/src/workflows.rs:220-247` (`resolve_workflow_alias`).
  - Fix: Add "On a project that has `task.yaml` but no `default.yaml`, the name `default` resolves to `task`."
- [severity: outdated] The Schema example labelled `# ~/.shelbi/projects/<project>/workflows/default.yaml`, with `description: The standard one-track flow shipped with every project.` (lines 77-113), plus the statuses.yaml example (67-75)
  - Reality: no project ships `default.yaml` any more. `shelbi init` writes `task.yaml` + `subtask.yaml`, and `default_workflow_yaml()` is only called from tests. The shipped `statuses.yaml` also has a sixth entry, `canceled` (archived), which the example omits.
  - Evidence: `crates/shelbi-state/src/workflows.rs:158-180`; `crates/shelbi-core/src/scaffold.rs:374`; `crates/shelbi-core/src/statuses.rs:186-230`.
  - Fix: Label the example as an illustrative custom workflow (e.g. `workflows/example.yaml`) and drop "shipped with every project". Add `- { id: canceled, name: Canceled, category: archived }` to the statuses example.
- [severity: wrong] "Omit it and Shelbi derives a default from the category (`developer` for `active`, `orchestrator` for `ready`)." (lines 197-198); rule 3 "For the `ready` and `active` categories a bare `owner: agent` is accepted and defaults to `orchestrator` / `developer` (with a deprecation warning)" (lines 219-222)
  - Reality: workflow files must reference statuses without an inline `category:`, so the category is never available when the owner is resolved. A bare `owner: agent` with no `agent:` **always fails**, even on `todo`/`in-progress`.
  - Evidence: tested. `{ id: todo, owner: agent }` → `status 'todo' has owner: agent but neither an 'agent:' field nor a 'category:' to derive a default from — add one`. Code: `crates/shelbi-core/src/workflow.rs` `resolve_owner_agent` (`None =>` arm); `crates/shelbi-state/src/workflows.rs:197-200` (inline identity rejected first).
  - Fix: Line 197: "`agent:` is required whenever `owner: agent`." Rule 3: "**An `agent`-owned status must name its `agent:`.** A bare `owner: agent` fails the load, whatever its category."
- [severity: wrong] "**`owner` is `user` or `agent`.** No other value parses" (lines 217-218)
  - Reality: any other string is accepted as the legacy named-owner form. `owner: qa` is rewritten to `owner: agent, agent: qa` with a one-time deprecation warning. A missing `owner` is an error.
  - Evidence: tested. `owner: qa` loads and shows `todo agent qa`, warning "legacy `owner: qa` rewrote to `owner: agent, agent: qa`". Code: `resolve_owner_agent` (`other =>` arm).
  - Fix: "`owner` should be `user` or `agent`. Any other value is read as the legacy `owner: <agent-name>` form (migrated to `owner: agent, agent: <name>` with a deprecation warning). A missing `owner` is an error."
- [severity: missing] The validation list (lines 126-130; "fails the load", 208-209)
  - Reality: the list omits several rules:
    - duplicate `from -> to` transitions are rejected;
    - an `agent:` with no `agents/<name>/` directory is rejected, and the error lists the available agents;
    - a missing `statuses.yaml` is a hard error;
    - an invalid `git.branch`/`git.branch_prefix` is rejected;
    - in `shelbi workflow list` and on the board, a malformed workflow file is **skipped with a warning**, and only a by-name load (`workflow show`, dispatch) hard-fails;
    - more than one `backlog`/`ready`/`handoff` status only warns ("generic code … keys off the first").

    The owners example at 172-180 references `signoff`, which the catalog shown earlier doesn't declare, and the "Review with QA, sign off by hand" example (two `handoff` statuses) loads with that warning.
  - Evidence: `crates/shelbi-core/src/workflow.rs` `validate()`; `crates/shelbi-state/src/workflows.rs:476-505` (agent references), `301-375` (per-file skip); `crates/shelbi-core/src/statuses.rs:126-157`; tested.
  - Fix: Add those items to the validation list, and note that `signoff` must first be added to `statuses.yaml`.
- [severity: wrong] "The activity feed renders an `active → handoff` move as "handed off"" (lines 160-161)
  - Reality: the feed row reads "finished <title>" with a `ready for review` pill.
  - Evidence: `crates/shelbi-app/src/activity.rs:2157-2165` (`category_pill`), `:2583` (`"finished"`).
  - Fix: "…renders an `active → handoff` move as "finished … [ready for review]", whatever the status names."
- [severity: unclear] "All actions are **hub-side** … workspaces never invoke actions directly." (lines 272-274)
  - Reality: the hub orchestrates every action, but `push_branch` and `open_pr` run git/gh inside the **assigned workspace's worktree** (over SSH for remote machines). `merge`, `close_pr` and `delete_branch` run on the hub.
  - Evidence: `crates/shelbi-orchestrator/src/actions.rs:38-43`.
  - Fix: "Actions are orchestrated from the hub; `push_branch`/`open_pr` run in the issue's worktree on its machine, the rest on the hub. Workspaces never invoke them."
- [severity: missing] The Transitions section and action table (lines 252-305)
  - Reality: the page leaves out:
    - transition-level `run:` (shell commands), `ready:` (readiness probe), `ready_timeout:` (default 90s) and `url:`;
    - the workflow-level `review:` serve recipe and `zen:` override;
    - that `merge` rejects `merge_strategy: rebase` at run time, even though `rebase` passes schema validation.
  - Evidence: `crates/shelbi-core/src/workflow.rs` (`Transition` struct; `Workflow.review`, `Workflow.zen`); `crates/shelbi-orchestrator/src/actions.rs:1501-1514`; tested that `merge_strategy: rebase` loads.
  - Fix: Add a sentence pointing to `/docs/configuration/workflow#run-ready-and-teardown` and `#review`, and note "`rebase` validates but the `merge` action refuses it."
- [severity: unclear] "The canonical default workflow's transitions, written out:" (lines 276-287)
  - Reality: the built-in `default` declares **no** transitions; Zen falls back to "the handoff status trips the merge bar". The shipped `task` workflow declares these two edges **plus** two `→ canceled` edges (`[close_pr, delete_branch]`).
  - Evidence: `crates/shelbi-core/src/workflow.rs` (`default_workflow`: `transitions: None`; `fires_merge_bar`; `task_workflow`).
  - Fix: "The shipped `task` workflow's forward transitions (it also declares two cancel edges, shown below):"
- [severity: wrong] "Placeholders are allowed in `git:` field values only. They are not resolved inside … the `transitions:` shape." (lines 416-418); "resolve from the issue's frontmatter at load time" (lines 365-366)
  - Reality:
    - A transition's `target:` also supports `{{var}}`, resolved from frontmatter params (`resolve_transition_target`; `shelbi action apply-transition --help` says the same).
    - `git.branch` also gets the built-ins `{{id}}` and `{{github_user}}`.
    - Substitution happens at dispatch/merge time, not at load.
  - Evidence: `crates/shelbi-core/src/workflow.rs` `resolve_transition_target`, `resolve_git`; `crates/shelbi-orchestrator/src/branch.rs:64-90`.
  - Fix: "Placeholders resolve in `git:` values and in a transition's `target:`, at dispatch/merge time. `git.branch` also gets `{{id}}` and `{{github_user}}`."
- [severity: broken-link] "[`required_params`](/docs/configuration/workflow#required-params)" (line 424)
  - Reality: the heading slug on the target page is `required_params` (with an underscore).
  - Evidence: github-slugger keeps `_`; the heading on `site/content/docs/configuration/workflow.mdx` is `required_params`.
  - Fix: `/docs/configuration/workflow#required_params`.
- [severity: wrong] subtask: "the templated `base_branch: task/{{task}}` resolves to the parent's branch, so the subtask branches from and squash-merges directly into that branch" (lines 574-577, 589); quoted `description: A piece of a parent issue, …` (line 585)
  - Reality: `task/{{task}}` renders literally as `task/<parent-id>`, and nothing maps it to the parent's actual branch. The shipped `task` workflow names branches `{{github_user}}/{{id}}` (e.g. `jlong/add-auth`), so with the shipped defaults the subtask base is **not** the parent's branch. This is likely a **product bug**: the code comments still assume `task/<id>`. Separately, the shipped description says "A piece of a parent **task**…".
  - Evidence: `crates/shelbi-core/src/workflow.rs` (`task_workflow`: `branch: {{github_user}}/{{id}}`; `subtask_workflow`: `base_branch: task/{{task}}`); `crates/shelbi-orchestrator/src/actions.rs:589-600` (`resolve_task_base_branch` only substitutes params).
  - Fix: Fix the product, or document: "`base_branch: task/{{task}}` resolves to `task/<parent-id>`; this matches the parent's branch only if the parent's branch is named that way (the shipped `task` workflow uses `<github_user>/<id>`, so set the parent's `branch:` or edit `subtask.yaml`)." Correct the description quote to "parent task".
- [severity: wrong] "`column: Designing  # uses a status name from the assigned workflow`" (line 498)
  - Reality: `column:` holds a status **id**. Only the core ids are normalized (`in_progress`, `wip`, …), so `Designing` would be stored as the literal id `Designing`.
  - Evidence: `crates/shelbi-core/src/model.rs:1960-1990,2121-2132` (`normalize_status_id`). Real issue files contain e.g. `column: done`.
  - Fix: `column: designing   # a status id from the assigned workflow`.
- [severity: outdated] "An issue is bound to its workflow at creation. **Cross-workflow moves are not allowed.** To switch an issue into a different workflow, create a fresh issue" (lines 509-512)
  - Reality: `shelbi issue edit <id> --workflow <name>` changes it (the name must be an existing `workflows/<name>.yaml`), and so do the TUI and desktop issue editors.
  - Evidence: `shelbi issue edit --help` ("--workflow <NAME> Set the issue's workflow. Must name an existing `workflows/<NAME>.yaml`"); `crates/shelbi-orchestrator/src/mutate/add_edit.rs:135-141,196`; `crates/shelbi-app/src/issue_edit.rs:507-522`.
  - Fix: "Change an issue's workflow with `shelbi issue edit <id> --workflow <name>` (or the issue editor); the target must exist."
- [severity: wrong] "Cycles in `depends_on:` are rejected at issue-load time" (line 483)
  - Reality: cycles, self-references and unknown ids are rejected when the issue is **saved** (`issue add` / `issue depends`), with the error `dependency cycle: a → b → a`.
  - Evidence: `crates/shelbi-state/src/issue_store.rs:808-813`; `crates/shelbi-state/src/lib.rs:3438-3480`.
  - Fix: "…are rejected when the issue is created or its dependencies are edited, with an error naming the cycle path."
- [severity: wrong] The `depends_on` section's claim that a dependent issue won't be dispatched until its parents are `done`, and "An issue whose parent ended up in a non-`done` terminal state … is surfaced in the activity feed" (lines ~478-487)
  - Reality: `Issue::is_blocked` (all deps exactly status id `done`) only drives the 🔒 badge, in the TUI board (`crates/shelbi-tui/src/kanban.rs:2619,3462`), the desktop view model (`crates/shelbi-app/src/view.rs:1061`) and `shelbi issue list/show` (`crates/shelbi-cli/src/commands/issue.rs:692,764`). Nothing gates dispatch on it: there is no call in `crates/shelbi-orchestrator` (dispatch, poller, `mutate/start.rs`), and the default orchestrator prompt has no dependency rule. The orchestrator model might honour the 🔒 it sees in `issue list` output, but nothing enforces it. Nothing surfaces a canceled parent in the activity feed either.
  - Evidence: `grep -rn is_blocked crates/` (callers above only); `crates/shelbi-core/src/model.rs:2247-2256`; `crates/shelbi-state/src/default_orchestrator.md.template` (no depends/blocked rule).
  - Fix: Either add enforcement (product) or reword: "`depends_on` marks the issue 🔒 blocked on the board and in `shelbi issue list` until every dependency is in `done`; the branch is cut from the parent and restacked when it merges. Dispatch isn't hard-gated, so leave blocked issues in Backlog." Drop "surfaced in the activity feed".
- [severity: wrong] "When an issue lands here, the orchestrator dispatches it to a **`review`-tagged workspace** and loads the Reviewer agent" (lines 555-557)
  - Reality: the **daemon's review auto-loader** fills idle `review`-tagged slots on each supervisor tick, and a human can press Enter on a Queued-for-Review row. The orchestrator prompt explicitly tells the model to leave review loads alone.
  - Evidence: `crates/shelbi-orchestrator/src/poller.rs:4954-4965` (`maybe_autoload_review_queue`); `crates/shelbi-orchestrator/src/load.rs:506-522`; `crates/shelbi-state/src/default_orchestrator.md.template:513-520`.
  - Fix: "When an issue lands here, Shelbi auto-loads it onto a free `review`-tagged workspace (or you load it from the sidebar's Queued for Review list) and boots the Review agent…"
- [severity: unclear] Status name spelled `InProgress` (lines 140, 337, 626, 636, 642, 659)
  - Reality: the shipped display name is `In Progress` (id `in-progress`, header `IN PROGRESS`). Line 10 already uses "In Progress".
  - Evidence: `crates/shelbi-core/src/statuses.rs:186-230`.
  - Fix: Use "In Progress" for the name and `in-progress` where an id is meant.
- [severity: missing] No mention of Settings ▸ Workflows
  - Reality: both the TUI and the desktop app have a Settings ▸ Workflows page with:
    - the list of workflows and the status catalog;
    - create, duplicate, delete, set default and Edit YAML;
    - add, rename, recategorize and reorder for statuses;
    - a workflow editor (Form plus graph Builder).
  - Evidence: `crates/shelbi-tui/src/shell/settings/workflows.rs:1-27`; `crates/shelbi-app/src/settings/{workflows,workflow_editor,workflow_builder}.rs`; `desktop/src/views/settings_nav.rs:57`.
  - Fix: Add a short "Editing in the app" note beside the CLI reference.
- [severity: missing] CLI reference (lines 753-765)
  - Reality: the reference omits:
    - `shelbi action <push-branch|open-pr|merge|close-pr|delete-branch|restack|apply-transition>`, which runs the primitives by hand;
    - `shelbi issue edit <id> --workflow <name>`;
    - `shelbi issue move --skip-transition-actions` (the recovery escape hatch);
    - `shelbi workflow new --edit`.
  - Evidence: `shelbi action --help`, `shelbi issue edit --help`, `shelbi issue move --help`, `shelbi workflow new --help`.
  - Fix: Add those lines.

### guides/getting-started/custom-workflow.mdx
- [severity: unclear] Title "Author a custom Shelbi workflow" and summary "the workflow is two markdown files you own" (lines 2-4, 7-11)
  - Reality: in Shelbi a "workflow" is the YAML status pipeline (`workflows/*.yaml`, covered by workflows.mdx). This page is about the orchestrator prompt, so the terms collide.
  - Evidence: `crates/shelbi-state/src/workflows.rs`; compare `site/content/docs/guides/getting-started/workflows.mdx`.
  - Fix: Retitle to "Customize the orchestrator prompt"; summary "…the orchestrator's prompt is two markdown files you own".
- [severity: wrong] "On every `shelbi reload` (and at session start) Shelbi composes `preamble.md + instructions.md` and writes the rendered output to `~/.shelbi/projects/<name>/CLAUDE.md` … auto-loads the rendered prompt. No flags, no env vars, no MCP." (lines 34-39). The same claim appears in the Callout "Don't hand-edit CLAUDE.md" (41-48) and at 238-240 ("Read that file to see exactly what the runner loads").
  - Reality: `CLAUDE.md` is no longer written or read; a leftover one triggers "CLAUDE.md detected at … but no longer read". The composed prompt goes to `~/.shelbi/projects/<name>/.claude/agent-instructions.md`. Claude is launched with `--append-system-prompt "$(cat .claude/agent-instructions.md)"` (plus `--plugin-dir` for Shelbi's system skill). Codex gets the rendered instructions in its startup prompt.
  - Evidence: `crates/shelbi-orchestrator/src/lib.rs:229` (`ORCH_AGENT_INSTRUCTIONS_REL = ".claude/agent-instructions.md"`, verified), `:600-612`, `:816-844` (`launch_with_bootstrap`); `crates/shelbi-state/src/agent_workspaces.rs:496-541` (legacy hint, verified); `crates/shelbi-orchestrator/src/workspace.rs:5096-5113`.
  - Fix: "On every orchestrator (re)launch Shelbi composes `preamble.md + instructions.md` into `~/.shelbi/projects/<name>/.claude/agent-instructions.md`; Claude gets it via `--append-system-prompt`, Codex as its startup prompt." Replace the CLAUDE.md callout with "a legacy CLAUDE.md is ignored; delete it". Point lines 238-240 at `.claude/agent-instructions.md`.
- [severity: wrong] "`shelbi agent edit orchestrator` … (and materializes the shipped default first if you haven't touched it yet)" (lines 61-62)
  - Reality: `agent edit` just opens `agents/<name>/instructions.md` in `$EDITOR`. If the directory is missing it errors ("agent `x` not found. Run 'shelbi agent new x'"). Defaults are materialized by `shelbi init` and by reload's self-heal, not by `edit`.
  - Evidence: `shelbi agent edit --help`; `crates/shelbi-cli/src/commands/agent.rs:203-212`.
  - Fix: "That opens `agents/orchestrator/instructions.md` (seeded by `shelbi init`) in `$EDITOR`."
- [severity: outdated] "**The bootstrap flow.** The first reply of every session runs `shelbi issue list`, `shelbi workspace list`, starts `shelbi events tail --follow`, and reads `shelbi zen status`." (lines 71-74)
  - Reality: the shipped bootstrap is:
    - `shelbi status --full --handoff` (one snapshot: board, workspaces, zen, handoff);
    - a non-consuming `shelbi events tail --follow --project <you>` watched through Monitor;
    - a periodic self-drain, `shelbi orchestrator events next --follow --max-lifetime 2s` then `shelbi orchestrator events ack <id>`;
    - `shelbi config upgrade --needs-judgment --format json`.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:214-327`.
  - Fix: Replace the list with those four steps, and stress that the `events next` + `ack` drain is the delivery guarantee.
- [severity: outdated] "**The Zen-Mode section** … Replace the judgment categories and the merge conditions" (lines 82-86); Tweak 2 puts a `## Zen Mode / ### Auto-promote judgment categories` block in the orchestrator prompt (lines 131-170)
  - Reality: judgment categories and merge conditions moved out of the prompt into a user-owned `zenmode.md` (`~/.shelbi/projects/<name>/zenmode.md` or `<repo>/.shelbi/zenmode.md`). The shipped prompt's `## Zen Mode` section tells the orchestrator to read that file as the source of truth ("Tune Zen Mode per project by editing **`zenmode.md`**, not this prompt"). Editing instructions.md as shown would conflict with it.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:959-975,1067-1073`; `crates/shelbi-state/src/zenmode.rs:1-30`; `crates/shelbi-state/src/default_zenmode.md.template`.
  - Fix: Rewrite Tweak 2 to edit `zenmode.md`, keeping its first line as the one-line summary the heartbeat re-injects. In "What to keep", say to keep the `## Zen Mode` pointer to `zenmode.md`.
- [severity: wrong] "The default prompt errs toward terse one-line status updates ("✓ dispatched to delta. branch: shelbi/fix-login")" (lines 174-176)
  - Reality: the shipped `## Reporting` rule is a one-line, comma-separated summary of issue changes, with branch names shown without the user prefix, e.g. `palette-icons started on alpha, fetch-loop -> handoff, no ready issues for charlie`.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:700-709`.
  - Fix: Quote the real format.
- [severity: wrong] "In addition to the project YAML's `prefers_machine` hints" / "The YAML's `prefers_machine` hint still overrides everything if set explicitly on a card." (lines 107, 124-125)
  - Reality: `prefers_machine` is an **issue frontmatter** field (`shelbi issue add --prefers-machine`), not a project YAML key. The shipped selection rule also skips `review`-tagged slots and slots a user has occupied with a shell.
  - Evidence: `crates/shelbi-core/src/model.rs:2201`; `shelbi issue add --help`; template `:659-685`.
  - Fix: "In addition to each issue's `prefers_machine:` hint…" and "An issue's `prefers_machine:` still overrides these rules."
- [severity: outdated] "`shelbi reload` respawns the sidebar, Issues, and Activity panes … It does recycle the orchestrator pane, so its conversation context resets" (lines 242-246); "Composes the prompt against your edits and respawns the panes" (224)
  - Reality: before the respawn, the old orchestrator writes `agents/orchestrator/handoff.md`, which the new instance ingests and then deletes, so in-flight context carries forward. `shelbi reload chat` reloads only the orchestrator.
  - Evidence: `shelbi reload --help`.
  - Fix: "…recycles the orchestrator, which writes a handoff note its successor ingests, so in-flight context carries over (the raw transcript does not). Use `shelbi reload chat` to reload only the orchestrator."
- [severity: missing] "Edit them carefully, but don't delete" (line 69) / "your edits stay put on upgrade" (27-28)
  - Reality: self-heal keeps your edits, but it merges runner-critical orchestrator sections back into a customized prompt if they're missing, and re-adds a `## Changelog` heading.
  - Evidence: `crates/shelbi-state/src/agent_workspaces.rs:28-36`; template `:940-942`.
  - Fix: Add a sentence saying so.
- [severity: unclear] "so you write it once instead of pasting it into four prompts" (lines 31-32)
  - Reality: six agents ship (orchestrator, developer, review, qa, security, adversarial).
  - Evidence: `crates/shelbi-state/src/agent_workspaces.rs:10-26`; `shelbi agent list`.
  - Fix: "…into every agent's prompt".
### guides/getting-started/multi-workspace.mdx
- [severity: wrong] Issue ids used throughout (lines 56-59, 130-132, 211-215), e.g. `shelbi issue move add-csv-export-to-reports --to todo`, `tighten-rate-limit-error-copy`
  - Reality: the auto-generated id is the full slug of the title, with no words dropped. `shelbi issue add "Add CSV export to the reports page"` gives `add-csv-export-to-the-reports-page`, and "Tighten the rate-limit error copy" gives `tighten-the-rate-limit-error-copy`. The `issue move` commands on lines 130/132 would fail with "not found". Only `fix-cookie-domain-bug-on-safari-sso` comes out as shown.
  - Evidence: `crates/shelbi-orchestrator/src/mutate/add_edit.rs:349-389` (`generate_unique_id` → `slugify`); `shelbi issue add --help` ("--id … Override the auto-generated id (slugified from the title)").
  - Fix: Add `--id add-csv-export-to-reports` and `--id tighten-rate-limit-error-copy` to the `issue add` lines (65, 67), or use the full slugs everywhere afterwards.
- [severity: outdated] Sample events lines `todo -> in_progress reason=orchestrator:auto-dispatch_workspace=alpha …` (lines 213, 215)
  - Reality: `issue start` appends the agent, so the line reads `reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer`. On a GitHub tracker every task line also ends with `owner=me`.
  - Evidence: `crates/shelbi-cli/src/commands/issue.rs:1616-1634` (`dispatch_reason_with_agent`); `crates/shelbi-state/src/event_log.rs:1549-1564`; live `shelbi events tail`: `todo -> in_progress reason=orchestrator:auto-dispatch_workspace=bravo_agent=developer from_category=ready to_category=active owner=me`.
  - Fix: `…reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer from_category=ready to_category=active` (and the same for bravo).
- [severity: wrong] "a `✓` row appears in the sidebar's **Ready for Review** section" (line 231); "Shelbi checks the branch out into the project's working directory on the machine that ran the issue and spawns a fresh agent session pointed at the diff … the review pane is its own conversation in the same worktree" (lines 242-251)
  - Reality: a handed-off card appears under "Ready for Review" only once it's loaded onto a `review`-tagged slot (`▶` while loading, `✓` once serving). Until then it sits under "Queued for Review" with `·`.
    - With no review slot, Enter fails with "no free review workspace to load onto — free one or wait".
    - With one, the hub daemon's auto-loader (active when the review status is review-tagged, as in the shipped `task` workflow) puts the card on the slot and launches the **Review agent** in that slot's own worktree. It never uses the dev worktree or the project working directory.
  - Evidence: `crates/shelbi-app/src/view.rs:186-215,585-604`; `crates/shelbi-orchestrator/src/load.rs:506-522,833-858`; `crates/shelbi-orchestrator/src/poller.rs:351-360`.
  - Fix: Rewrite: the card shows under Queued for Review; with a review slot it auto-loads, moves to Ready for Review with `✓`, and Enter opens the review panel. Link to the review-workspaces guide. Without a review slot, read the diff on GitHub or locally.
- [severity: wrong] "Accept when you're ready: `you: merge it.` Then move the card from `review` to `done`" (lines 253-263)
  - Reality: the shipped `task` workflow's `review → done` transition does the merge itself (`actions: [push_branch, merge]`). The Review agent "does not modify code" and won't merge. Asking an agent to merge and then also moving the card risks a double merge that strands the card.
  - Evidence: `crates/shelbi-core/src/workflow.rs:1000-1004`; `crates/shelbi-state/src/default_review.md.template:20-25`; `crates/shelbi-state/src/default_zenmode.md.template` ("Running `zen pr-merge` first double-merges … strands the card").
  - Fix: "Accept with **✅ Approve** in the review panel, or move the card to `done` (`L` on the board, or `shelbi issue move <id> --to done`). The workflow's `review → done` transition merges the branch."
- [severity: outdated] "The workspace's pane died. The orchestrator surfaces it (`workspace=<name> pane_alive=false`) and waits for you. Re-launch with `Enter` on the workspace row…" (lines 284-288)
  - Reality: the auto-restart supervisor relaunches a crashed pane and re-dispatches its issue on its own (`supervision=restart`). The orchestrator is told not to react to a bare pane death. Only `supervision=gave-up reason=crash-loop` (restart cap reached) is surfaced to you.
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:560-580`.
  - Fix: "**A workspace keeps crashing.** Shelbi restarts a crashed pane automatically; after repeated crashes it gives up (`supervision=gave-up reason=crash-loop`) and the orchestrator tells you. Investigate, or run `shelbi workspace stop <name>` to release the issue back to `todo`."
- [severity: missing] "edit `~/.shelbi/projects/<name>.yaml`, bump the pool, and run `shelbi reload`" (lines 12-14)
  - Reality: three things are missing here:
    - `shelbi workspace add <name> [--machine <m>]` adds a slot and creates its worktree.
    - In-repo projects keep the config in `<repo>/.shelbi/project.yaml`.
    - A fresh `shelbi init` pool is one dev slot plus one `review`-tagged slot. The review slot never takes dev dispatch and is hidden from the sidebar's Workspaces section.
  - Evidence: `shelbi workspace add --help`; `crates/shelbi-cli/src/commands/init.rs:961-968`; `crates/shelbi-state/src/default_orchestrator.md.template:663-670`; `crates/shelbi-tui/src/app.rs:1568-1575`.
  - Fix: "If you only have one dev workspace, run `shelbi workspace add bravo` (repeat as needed, `--machine <name>` for a remote box), or edit the project YAML (`~/.shelbi/projects/<name>.yaml`, or `<repo>/.shelbi/project.yaml` in-repo). The starter `review` slot doesn't count: it only serves finished branches."

### guides/getting-started/review-workspaces.mdx
- [severity: wrong] `  - from: review, to: done, actions: [merge, delete_branch]` (line 76)
  - Reality: invalid YAML. A plain block mapping can't hold several `key: value` pairs on one line, so it fails with "mapping values are not allowed in this context".
  - Evidence: a YAML parse of the snippet fails as above, while the braced form parses. The shipped files use braces (`crates/shelbi-core/src/scaffold.rs:320-321`).
  - Fix: `  - { from: review, to: done, actions: [merge, delete_branch] }`. Optionally note that the shipped `task` workflow uses `[push_branch, merge]` with no `delete_branch` (`crates/shelbi-core/src/workflow.rs:996-1004`).
- [severity: wrong] "Leave `slot:` out and it defaults to the slot's zero-based index among the machine's workspaces" (lines 48-51)
  - Reality: that fallback applies only to `$SLOT` in transition `run:` commands. The `review:` recipe's `$PORT`/`$SLOT` comes only from an explicit `slot:`. Without one the port is unresolved, and the Review agent is told "This review workspace has no assigned slot", so it stops and reports a misconfiguration instead of serving.
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:518,6204` (`workspace.slot.and_then(...)`, no fallback), `:6285-6287`; `crates/shelbi-core/src/workflow.rs:1662-1666`.
  - Fix: "A review slot **must** set `slot:`. The `review:` recipe takes its `$PORT` only from an explicit `slot:`, and without one the Review agent stops and reports a misconfiguration instead of serving. (The declaration-order fallback applies only to `$SLOT` in transition `run:` commands.)"
- [severity: wrong] "the orchestrator routes the issue onto a free `review`-tagged slot (preferring the one it ran on)" (lines 116-119)
  - Reality: the orchestrator never loads reviews. Its prompt says loading is "a human action from the sidebar". The hub daemon's poller runs `autoload_review_queue` each tick and puts queued review cards (board order) onto idle review slots (declaration order), but only when the card's review status declares `tags: [review]`. Nothing prefers "the one it ran on": a dev slot is never a review target. A manual Enter or palette load reuses a review slot the card already holds, otherwise the first free one.
  - Evidence: `crates/shelbi-orchestrator/src/load.rs:506-522,553-561,766-813,822-858`; `crates/shelbi-orchestrator/src/poller.rs:351-360`; `crates/shelbi-state/src/default_orchestrator.md.template:68-72,525-535`.
  - Fix: "Because that status requires `tags: [review]`, the hub automatically loads the issue onto the first free `review`-tagged slot and the Review agent boots it from the `review:` recipe. You can also load it yourself with Enter on its row or from the palette."
- [severity: missing] Steps 1-2 (lines 33-97) present tagging a slot and requiring the tag as new setup
  - Reality: `shelbi init` already creates a `review` workspace with `tags: [review]` (and no `slot:`). The shipped `task` workflow's `review` status already has `agent: review` + `tags: [review]`. For a default project the real work is adding `slot:` to the existing slot and adding the `review:` block.
  - Evidence: `crates/shelbi-cli/src/commands/init.rs:961-968`; `crates/shelbi-core/src/workflow.rs:966-973`; `crates/shelbi-core/src/scaffold.rs:533-539`.
  - Fix: Add at the top of Steps: "New projects already have a `review` slot and a review status that requires the tag. Give that slot a `slot:` port and add the `review:` block; the tagging steps apply to projects created earlier or with custom workflows."
- [severity: unclear] "you make it from the review panel's **Actions**" (line 198)
  - Reality: there is no "Actions" heading. The panel shows one row with `✅ Approve` and `❌ Reject`.
  - Evidence: `crates/shelbi-tui/src/review_panel.rs:17-21,448-484`.
  - Fix: "from the **✅ Approve / ❌ Reject** row at the bottom of the review panel".
- [severity: missing] The view-switcher list (lines 179-189)
  - Reality: on a remote review slot, View Diff and Edit in `<editor>` are hidden. Only Reviewer, plus Open Browser when a URL is declared, remain.
  - Evidence: `crates/shelbi-app/src/review.rs:162-172`.
  - Fix: Add "(View Diff and Edit are local-slot only)".

### guides/getting-started/enable-zen-mode.mdx
- [severity: wrong] `ci_timeout: 15m` (lines 43, 56-57)
  - Reality: project-level `zen.ci_timeout` is an integer number of seconds. With `15m` the whole project YAML fails to load, and `shelbi zen status` prints `checks: (could not read myapp.yaml: yaml: zen.ci_timeout: invalid type: string "15m", expected u64 …)`.
  - Evidence: `crates/shelbi-core/src/model.rs:2624-2627` (`with = "duration_secs"`, "Serialized as a number of seconds", verified), `:3094-3111`. Reproduced in a throwaway root; with `ci_timeout: 900`, status prints `ci timeout: 15m` and the danger-path list exactly as the page shows.
  - Fix: `  ci_timeout: 900   # seconds (15 minutes, the default)`; line 57: "Integer seconds; default `900` (15m)."
- [severity: wrong] "First-run hotkey probe … a chooser offers `Ctrl+\`, `Ctrl+G`, or `Ctrl+Shift+Z` … persisted to `~/.shelbi/shelbi.yaml`" (lines 117-122)
  - Reality: there is no hotkey probe or chooser any more (the `zen_probe` module a loader comment names is gone).
    - The chord is `defaults.global.zen_toggle` in `~/.shelbi/keys.yaml`, default `alt-z`. The legacy `config.yaml` `keymap.zen_toggle` is migrated into it.
    - Allowed values: `alt-z`, `ctrl-backslash`, `ctrl-g`, `ctrl-shift-z`, `none`.
    - On first enable there is an "Enable Zen Mode?" intro popover with "Don't show this again".
  - Evidence: `crates/shelbi-state/src/keymap/actions.rs:263`; `crates/shelbi-state/src/keymap/loader.rs:10-11,1090-1106`; `crates/shelbi-state/src/user_config.rs:520-545`; `crates/shelbi-core/src/scaffold.rs:467-471`; `crates/shelbi-tui/src/overlay/zen_intro.rs:1-8`; `crates/shelbi-app/src/zen.rs:15-26`.
  - Fix: "**Rebinding**: if your terminal swallows `Alt` (e.g. Terminal.app), set `defaults.global.zen_toggle` in `~/.shelbi/keys.yaml` to `ctrl-backslash`, `ctrl-g` or `ctrl-shift-z`. The first time you turn Zen on from the app, an *Enable Zen Mode?* intro explains what it does."
- [severity: missing] "Three ways" to toggle (lines 113-116)
  - Reality: Zen also toggles from the command palette ("Turn Zen Mode on/off") and the desktop app's View menu ("Turn Zen Mode On", ⌥Z). In the TUI `Alt+Z` is a global binding that works from any focus, not only the sidebar.
  - Evidence: `crates/shelbi-app/src/command.rs:439-444`; `desktop/src/menus/tree.rs:522-526,962`; `shelbi config list-actions` (`global zen_toggle`).
  - Fix: Add "**Palette** (`Ctrl+P` → Turn Zen Mode on)" and "**Desktop**: View ▸ Turn Zen Mode On (⌥Z)", and say "from anywhere in the TUI".
- [severity: wrong] The band is "anchored to the bottom of the workspaces column … off shows a dim `Alt+Z Zen mode` hint in the same spot", plus the ASCII sidebar sketch (lines 124-140)
  - Reality: the `ZEN MODE ON` band is the bottom row of the whole sidebar, below the footer toolbar (⚙ / 📣 / error badge), and only while Zen is on. When Zen is off or paused there is no row and no hint (the hint lines were removed). The desktop shows a "Zen mode on" pill.
  - Evidence: `crates/shelbi-tui/src/shell/sidebar.rs:13-21,272-281,737-760`.
  - Fix: "…the full-width green **ZEN MODE ON** band along the bottom edge of the sidebar, under the toolbar. Off or paused, the band is simply absent." Update the ASCII sketch (`— Workspaces —` header, footer toolbar row, then the band).
- [severity: missing] The walkthrough promises a finished issue auto-merges (lines 162-189)
  - Reality: the review-workspace gate. If the project declares any `review`-tagged workspace, Zen never auto-merges a handoff; the issue waits for a human to load it and Approve. A default `shelbi init` project has a `review` slot, and the previous guide sets one up, so following the guides in order means no auto-merge.
  - Evidence: `crates/shelbi-state/src/default_zenmode.md.template` ("First, the review-workspace gate…"); `crates/shelbi-state/src/default_orchestrator.md.template:1003-1006`; `crates/shelbi-cli/src/commands/init.rs:961-968`.
  - Fix: Add a callout before the flow: "Zen only auto-merges in projects with **no** review workspace. If you have a `review`-tagged slot (new projects do), Zen still auto-promotes and dispatches, but every finished issue waits for you to Approve on the review slot. Remove the slot (or its tag) to let Zen merge on its own."
- [severity: outdated] Pipeline ending in `-> pr-merge` (lines 170-176); "runs against every Zen-eligible review before it opens a PR" (lines 20-23)
  - Reality: the shipped flow is:
    1. `zen probe`: local checks, conflict, diff size (≤30 files / ≤2000 lines by default), danger paths.
    2. The orchestrator's own review of the diff against the acceptance criteria (`zen:review-reject` on failure).
    3. `zen pr-create`, only if there is no open PR or it has drifted (the `in-progress → review` transition already opened one).
    4. `zen ci-watch`.
    5. Move the card to `done`, whose `review → done` transition merges.

    `zen pr-merge` is explicitly not used for ordinary issues (it would double-merge); it is only for the release flow.
  - Evidence: `crates/shelbi-state/src/default_zenmode.md.template` (Merge conditions steps 1-9, Release flow); `crates/shelbi-state/src/default_orchestrator.md.template:1006-1027`; `crates/shelbi-core/src/workflow.rs:993-1004`.
  - Fix: `review-marker -> probe (local checks, merge conflict, diff size, danger paths) -> orchestrator diff review -> pr-create (only if no matching PR) -> ci-watch -> move to done (review->done transition merges)`. Lines 20-23: "before it lets the PR merge".
- [severity: outdated] Judgment categories "live in its prompt … `~/.shelbi/projects/<name>/agents/orchestrator/instructions.md` … run `shelbi agent edit orchestrator`" (lines 284-293)
  - Reality: the judgment categories, merge conditions and one-line summary live in a user-owned `zenmode.md` (`~/.shelbi/projects/<name>/zenmode.md`, or `<repo>/.shelbi/zenmode.md` in-repo). The orchestrator prompt says to edit `zenmode.md`, "not this prompt". It's reachable from the palette and Settings as "Edit Zen Mode".
  - Evidence: `crates/shelbi-state/src/default_orchestrator.md.template:959-972,1066-1072`; `crates/shelbi-state/src/zenmode.rs:1-35`; `desktop/src/app_smoke.rs:75`; `crates/shelbi-tui/src/shell/settings.rs:2715`.
  - Fix: "That policy is a file you own: `~/.shelbi/projects/<name>/zenmode.md` (`<repo>/.shelbi/zenmode.md` in-repo). Open it from the palette (**Edit Zen Mode**) or any editor. Its first line is the one-line summary the heartbeat re-injects, so keep it accurate."
- [severity: outdated] "Zen-driven moves get distinct visual treatment in the sidebar's activity feed: a small **ZEN** badge in the avatar column, and a dark-tinted row background" (lines 238-242)
  - Reality: Activity is a main-area view (`Ctrl+P` → Activity), not part of the sidebar. The ZEN badge and row tint are gone. Zen rows are filtered with a green **Zen** pill, toggled by `z` in Activity.
  - Evidence: `crates/shelbi-tui/src/activity.rs:1-14,483,610-614`; `shelbi config list-actions` (`activity toggle_zen_filter … z`).
  - Fix: "Zen-driven moves show up in the **Activity** view; press `z` (or click the **Zen** pill) to filter to them."
- [severity: unclear] Sample row "`ZEN promoted tighten-rate-limit-error-copy (category: routine issue shape you accept without changes)`" (lines 157-160)
  - Reality: the Activity row reads "promoted <issue title>" with a `backlog → todo` detail. The category isn't rendered.
  - Evidence: `crates/shelbi-app/src/activity.rs:2630-2635`.
  - Fix: Show `promoted Tighten the rate-limit error copy   backlog → todo`, or label the block illustrative.
- [severity: unclear] The reason table uses spaced forms (`orchestrator:zen-promote category=<n>`), then says "`shelbi events tail --follow` shows the same lines in raw form" (lines 246-251)
  - Reality: the events log turns spaces in reasons into underscores (`reason=orchestrator:zen-promote_category=2`, `reason=orchestrator:zen-merge_pr=2007`). Line 186 already uses the underscore form.
  - Evidence: `crates/shelbi-state/src/event_log.rs:3422-3426`; live `shelbi events tail`.
  - Fix: Add "(in the raw log, spaces become underscores: `zen-promote_category=<n>`, `zen-merge_pr=<n>`)".
- [severity: wrong] "`shelbi issue move <id> --to backlog` from `review` — pull one specific card out of Zen's hands" (lines 265-266)
  - Reality: a card back in `backlog` is a `zen scan` candidate again and can be re-promoted. The scan treats only user `todo -> backlog` demotions as permanent opt-outs. The per-issue opt-out is `zen: { enabled: false }` in the issue frontmatter.
  - Evidence: `crates/shelbi-orchestrator/src/zen.rs:14233-14268,14327-14360`.
  - Fix: "**Per issue**: set `zen: { enabled: false }` in the issue's frontmatter to keep Zen from auto-promoting it. Moving a card back to `backlog` alone doesn't stop Zen from re-promoting it."
- [severity: unverified] "The wrapper writes a heartbeat every 60s; an ungraceful exit flips Zen off, writes a `zen=off reason=crash-recovery` event line, and prints a stderr warning." (lines 272-278)
  - Reality: the hooks exist (`shelbi __zen-orch-start`, `__zen-heartbeat`, `__zen-orch-exit`), but nothing in the snapshot invokes them, and the orchestrator launch argv has no Zen wrapper. Auto-disable may no longer fire after tmux removal. The last `zen=off reason=crash-recovery` line in this hub's events.log is from 2026-10-04. The bootstrap prompt still scans for the line.
  - Evidence: `crates/shelbi-cli/src/commands/zen_lifecycle.rs:1-19,38-80`; `crates/shelbi-cli/src/main.rs:364-387`; `crates/shelbi-orchestrator/tests/golden/claude/orchestrator-argv.txt`; `crates/shelbi-orchestrator/src/lib.rs:703-710`.
  - Fix: Confirm with the owner whether crash auto-disable is still wired; if not, remove or soften the callout.
### guides/understanding-workflows/index.mdx
No factual errors found. See Verified accurate.

### guides/understanding-workflows/trunk-based.mdx
- [severity: wrong] "that collapsed edge trips exactly the same high-confidence probe … Trunk-based is where Zen auto-merge earns its keep" (lines 14-21); "squash-merged straight back into `main` at **Done**, the moment checks pass and Zen clears the bar" (79-81); "The `zen.checks` that run before the merge bar *are* the gate" (126-128); "Trunk-based only pays off with auto-merge on … hold the card in `InProgress` and surface it" (132-151)
  - Reality: the workflow has no `handoff` status. In that case the poller's ready-marker handoff auto-advances the card along the outgoing `merge` edge and runs the merge as soon as the developer writes the ready marker. Zen Mode, `zen.checks` and the orchestrator's merge conditions are never consulted. Only the clean-worktree/non-empty-branch check, the rebase and the push run first. The card never waits in In Progress for Zen, and the orchestrator rules in the sample can't hold it.
  - Evidence: `crates/shelbi-orchestrator/src/poller.rs:2873-2915` (fallback 3, "no handoff status … auto-advance along merge transition"); `poller.rs:2984-3000` (`execute_merge_action` runs before the move, with no zen call); `crates/shelbi-core/src/workflow.rs:1037-1045` describes the same behavior for the shipped `subtask` workflow.
  - Fix: Rewrite the intro and "Orchestrator adjustments": "With no `handoff` status, the hub merges the branch automatically when the developer writes its ready marker. Zen Mode is not consulted. If you want a green-checks gate before merge, keep a `review` (handoff) status and let Zen auto-accept `review → done`." Remove the "hold the card in InProgress" merge-condition sample, or move it to a review-gated variant.
- [severity: unverified] `actions: [push_branch, open_pr, merge, delete_branch]` (line 111)
  - Reality: on a handoff-less auto-advance the poller runs `merge` first, before the card moves. The other actions run afterwards with `merge` skipped, so `open_pr` fires after the branch is already merged. Not run, so the failure mode is unconfirmed (a "no commits" PR error short-circuiting `delete_branch` is likely). The shipped handoff-less `subtask` workflow uses `[push_branch, merge]`.
  - Evidence: `crates/shelbi-orchestrator/src/transition.rs:103-115,272-279` (`execute_transition_except`); `poller.rs:2984`; shipped subtask edge `[push_branch, merge]` at `crates/shelbi-core/src/workflow.rs:1100-1104`.
  - Fix: Use `actions: [push_branch, merge, delete_branch]` and drop the "open a PR into main" sentence (lines 124-125), or explain that no PR is opened in trunk mode.

### guides/understanding-workflows/feature-branch.mdx
- [severity: outdated] "`# workflows/default.yaml` … name: default … Inherits the project's base_branch … The shipped default.yaml ships with no active transitions block" (lines 109-144); "The `user`-owned `review` status still names `agent: orchestrator`. That's what lets Zen Mode land the merge" (150-152)
  - Reality: line 12 correctly says new projects ship `workflows/task.yaml` (`default_workflow: task`), but the YAML shown is the legacy `default.yaml`. The shipped `task.yaml` differs in three ways:
    - it has active `transitions`: `in-progress→review [push_branch, open_pr]`, `review→done [push_branch, merge]`, and both `→canceled [close_pr, delete_branch]`;
    - it has its own `git:` block (`base_branch: main`, `branch: '{{github_user}}/{{id}}'`, `merge_strategy: squash`);
    - its review status is `{ id: review, owner: user, agent: review, tags: [review] }`, served by the Review agent on a review-tagged workspace.
  - Evidence: `crates/shelbi-core/src/workflow.rs:932-1031` (`task_workflow`); `crates/shelbi-state/src/lib.rs:560-596` (migration to `task`); the real `~/.shelbi/projects/changelog-labs/workflows/task.yaml` matches. That Zen depends on `agent: orchestrator` is unverified.
  - Fix: Replace the second YAML block with the real `workflows/task.yaml`: name `task`, the four active transitions, the `git:` block above, and `review` with `agent: review` + `tags: [review]`. Rewrite lines 146-152: `git.base_branch: main` makes `open_pr`/`merge` target `main`, and the Review agent serves the `review` status.
- [severity: outdated] Mockup branch `"shelbi/cache-warm-up"` (line 77). The same pattern appears in git-flow.mdx (~line 79, `shelbi/rework-audit-log`) and forking.mdx (line 78).
  - Reality: generated branches follow `{{github_user}}/{{id}}`, e.g. `you/t-046`.
  - Evidence: `crates/shelbi-core/src/workflow.rs:1022-1026`; `crates/shelbi-orchestrator/src/branch.rs:27-110`.
  - Fix: Use `you/<id>` branch names in all three mockups.

### guides/understanding-workflows/git-flow.mdx
- [severity: wrong] The statuses.yaml catalog under "shipped defaults" drops `done` (lines 103-115).
  - Reality: every status id a workflow references must be declared in `statuses.yaml`. The shipped `task` workflow references `done`, and the page itself routes hotfixes there. If the `default_workflow` (`task`) fails to load, the whole project fails to load: "declares default_workflow: `task`, but that workflow could not be loaded".
  - Evidence: `crates/shelbi-core/src/workflow.rs:688-713` (`resolve_against` errors on unknown ids); `crates/shelbi-state/src/lib.rs:1279-1285`.
  - Fix: Add `- { id: done, name: Done, category: done }` to the shipped-defaults block.
- [severity: wrong] `merge_strategy: merge   # a true merge preserves develop's history` (line 169) and "Note `merge_strategy: merge` (not `squash`)…" (197-199)
  - Reality: the `merge` action always uses the project's `git.merge_strategy`. A workflow-level `git.merge_strategy` is parsed but ignored by `merge`, so this release would be squash-merged under the project default. Zen's exact-provenance merge supports only `squash`.
  - Evidence: `crates/shelbi-orchestrator/src/actions.rs:503` (`require_supported_strategy(project.merge_strategy())`, verified); `transition.rs:565-566` (project passed unchanged); `crates/shelbi-orchestrator/src/zen.rs:2043`.
  - Fix: Say the strategy must be set at project level (`git.merge_strategy: merge` in project.yaml), which affects all workflows. Alternatively, have the human merge the release PR on GitHub, and the poller's GitHub-merge reconcile advances the card. Remove the workflow-level claim.
- [severity: wrong] Release workflow `- { id: staging, owner: agent, agent: developer }` / `initial_status: staging` (lines 172, 176)
  - Reality: the ready-marker forward handoff fires only for a card whose status id is literally `in-progress`. A developer finishing in `staging` writes a ready marker the poller ignores, so `staging → review` (`push_branch, open_pr`) never fires automatically. The release workflow has no `ready` status either, so the orchestrator's normal Todo dispatch never applies to a card created in `staging`.
  - Evidence: `crates/shelbi-orchestrator/src/poller.rs:2858-2859` (`tf.task.column == Column::in_progress()`, verified); `crates/shelbi-core/src/model.rs:2010`.
  - Fix: Use the shipped ids `todo` (ready) and `in-progress` (active), relabeled through `name:` in statuses.yaml if wanted, or say the user moves the card `staging → review` by hand. Also warn that the developer handoff rebases the branch onto `git.base_branch` and force-pushes (`poller.rs:2948-2970,3780-3792`), which would rewrite `develop` if the release branch is `develop`. A dedicated `release/<x>` branch is safer.
- [severity: unclear] `base_branch: develop   # feature/* branches cut off develop` (line 125)
  - Reality: the branch is cut from `develop`, but its name comes from `git.branch`/`branch_prefix`. This block sets neither, so the name falls back to the project setting or `<github-login>/<id>`, not `feature/*`.
  - Evidence: `crates/shelbi-orchestrator/src/branch.rs:41-110`.
  - Fix: Add `branch: 'feature/{{id}}'` to the `git:` block, or change the comment to "branches cut off develop".
- [severity: outdated] "**Hotfixes** … use the `default` workflow (base `main`)" (line 212)
  - Reality: fresh projects ship `task`. `default` survives only as a load-time alias.
  - Evidence: `crates/shelbi-core/src/model.rs:2452-2464`; `crates/shelbi-state/src/lib.rs:560-575`.
  - Fix: "use the shipped `task` workflow (base `main`)".

### guides/understanding-workflows/forking.mdx
- [severity: wrong] "Because the branch lives on someone else's fork, Shelbi doesn't cut branches or push here. Each issue carries the fork's PR branch in `branch:`" (13-15); "Pre-fill `branch:` with the PR's head ref so `merge` and `close_pr` act on the right PR" (146-148)
  - Reality: Shelbi never fetches from a fork. Every action uses `origin`: `push-branch` pushes to `origin`, and PR lookup is `gh pr list --head <branch>` in the hub repo. At dispatch, if `branch:` doesn't resolve locally, the hub cuts a fresh branch of that name off the base, so the reviewer gets an empty branch. Head refs written as `owner:branch` fail branch-name validation.
  - Evidence: `shelbi action push-branch --help` ("Push the task's branch … to `origin`"); `crates/shelbi-orchestrator/src/workspace.rs:6501-6536` (`rev-parse --verify`, then `worktree add -b <branch> <base>`); `branch.rs:35-38`.
  - Fix: Say the maintainer first fetches the PR head into a local branch (e.g. `git fetch origin pull/401/head:pr-401`) and sets `branch: pr-401`. Note that `push_branch` then publishes that branch to `origin`, and that `merge` uses the fork PR only if `gh pr list --head <branch>` finds it (unverified for cross-repo heads).
- [severity: wrong] `actions: [push_branch]   # push the fork branch to a review ref; no PR to open` (line 132)
  - Reality: `push_branch` pushes the task branch to `origin` under its own name. There is no "review ref". It also contradicts "doesn't … push here" (line 14).
  - Evidence: `crates/shelbi-orchestrator/src/transition.rs:553-555`; `shelbi action push-branch --help`.
  - Fix: Drop the action (`actions: []`), or change the comment to "pushes the branch to origin under the same name".
- [severity: wrong] Status `in-review` is the only active status (lines 102, 122)
  - Reality: a worker dispatched into `in-review` gets the developer handoff (rebase + ready marker), but the ready-marker handoff fires only for status id `in-progress`, so nothing moves the card to `sign-off`. Because `in-review` is the primary active status, it isn't treated as a gate and gets no pass/reject prompt.
  - Evidence: `crates/shelbi-orchestrator/src/poller.rs:2859`; `crates/shelbi-core/src/workflow.rs:398-427` (`primary_active_status`/`is_agent_active_gate`).
  - Fix: Keep the id `in-progress` and relabel it: `{ id: in-progress, name: In Review, category: active }`. Update the workflow and transitions to match.
- [severity: wrong] The page replaces the whole `statuses.yaml` with forking-only ids (lines 97-106).
  - Reality: the shipped `task`/`subtask` workflows (and `default_workflow: task`) reference `backlog`/`todo`/`in-progress`/`review`/`done`/`canceled`. Without those ids, `task` fails to load, and the whole project with it.
  - Evidence: `crates/shelbi-state/src/lib.rs:1279-1285`; `crates/shelbi-core/src/workflow.rs:688-713`.
  - Fix: Add a step: set `default_workflow: contribution` in project.yaml and delete or adjust `workflows/task.yaml` and `subtask.yaml`, or keep the default ids in the catalog alongside the new ones.
- [severity: missing] "`agent: reviewer` role, an agent you author" (lines 152-155)
  - Reality: the workflow loader rejects a status that names an agent with no `agents/<name>/` directory, so this workflow won't load until `reviewer` exists. Shipped reviewer presets already exist: `adversarial`, `security`, `qa`.
  - Evidence: `crates/shelbi-state/src/workflows.rs:497`; `shelbi agent list` shows adversarial, qa, security.
  - Fix: "Create it first with `shelbi agent new reviewer` (the workflow won't load until it exists), or point the status at the shipped `security` or `adversarial` preset."
- [severity: unclear] "(or a Zen-authorized reviewer agent) accepting it" (line 18)
  - Reality: `sign-off` has no `agent:`, and a status without an agent has no automation path. The page's own YAML and Zen rules keep it human.
  - Evidence: `crates/shelbi-core/src/workflow.rs:14-20`.
  - Fix: Remove the parenthetical.
- [severity: unclear] The mockup's `readyReview` lists PR #401 while that card sits in IN REVIEW (lines 29, 75-80).
  - Reality: review slots receive only cards in a handoff status.
  - Evidence: `poller.rs:2878-2883` (comment: "a review workspace … only receives tasks sitting in a handoff-category status").
  - Fix: Put the readyReview entry on the SIGN-OFF card (#399), or leave it empty.

### guides/doing-more-with-agents/index.mdx
- [severity: unclear] "name `agent: adversarial` on a status to wire the shipped one in one line" (lines 22-24)
  - Reality: wiring the gate also needs a status declared in `statuses.yaml` plus transition edges: a unique forward edge into the gate and a bounce edge back. Without the declared forward edge, the ready marker skips the gate.
  - Evidence: `crates/shelbi-core/src/workflow.rs:310-338` (`forward_agent_active_gate`); `crates/shelbi-orchestrator/src/poller.rs:3397-3407` (edge allowlist).
  - Fix: "…and follow Add it to a workflow with `agent: adversarial` in place of `adversarial-review`."

### guides/doing-more-with-agents/add-to-workflow.mdx
- [severity: wrong] The statuses.yaml example omits `canceled` (lines 20-29).
  - Reality: the shipped `task` and `subtask` workflows reference `canceled`. Removing it from the catalog makes `task` (the `default_workflow`) fail to load, and the project load fails with it.
  - Evidence: `crates/shelbi-core/src/workflow.rs:688-713`; `crates/shelbi-state/src/lib.rs:1279-1285`.
  - Fix: Keep `- { id: canceled, name: Canceled, category: archived }` at the end of the list.
- [severity: outdated] "`# workflows/default.yaml`" with `- { id: review, owner: user, agent: orchestrator }` and no `canceled` (lines 45-54)
  - Reality: new projects edit `workflows/task.yaml`, whose review status is `{ id: review, owner: user, agent: review, tags: [review] }`. Copying the example drops the Review agent and review-slot serving.
  - Evidence: `crates/shelbi-core/src/workflow.rs:932-1031`.
  - Fix: Change the file to `workflows/task.yaml`. Keep `agent: review` + `tags: [review]` on `review`, and add `- { id: canceled, owner: user }`. Optionally mirror the shipped `review → done: [push_branch, merge]` and `→ canceled` edges in the transitions block.
- [severity: unclear] "`owner: agent` makes the status the orchestrator's to act on, so issues that land here are auto-dispatched" (lines 56-57)
  - Reality: the daemon's deterministic active-gate pass dispatches a card in an agent-owned active gate, not the orchestrator model. Gate dispatches get a pass/reject prompt section.
  - Evidence: `crates/shelbi-core/src/workflow.rs:350-372`; `crates/shelbi-orchestrator/src/poller.rs:5030`.
  - Fix: "…so the hub daemon auto-dispatches issues that land here to the named agent."

### guides/doing-more-with-agents/adversarial-review-agent.mdx
- [severity: wrong] "then hand off forward the normal way, by writing the review-ready marker" (lines 123-128)
  - Reality: the ready-marker handoff applies only to cards in `in-progress`, so a gate's ready marker is ignored. A gate passes by writing the **transition marker** with the next status id (here `review`). Shelbi appends a gate section to the dispatch prompt with the exact pass/reject commands, and that section says "do not write a ready marker".
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:6097-6170` (`compose_gate_prompt`); `poller.rs:2859`.
  - Fix: "…then pass it forward by writing the transition marker with the next status: `mkdir -p .shelbi && printf '%s\n%s\n' "$TASK_ID" review > .shelbi/shelbi-transition.tmp && mv .shelbi/shelbi-transition.tmp .shelbi/shelbi-transition`. (Shelbi's dispatch prompt appends the exact pass/reject commands for you.)"
- [severity: missing] "`instructions.md` **is** the agent's system prompt. There's no wrapper or config layer." (lines 35-36)
  - Reality: an agent directory may also hold `agent.yaml` (description, `preferred_runner`, per-runner `model`/`reasoning_effort`, `permissions_mode`), editable in Settings ▸ Agents. At dispatch the prompt also gets the task body and a handoff/gate section. `shelbi agent new` creates only `instructions.md` + `skills/`.
  - Evidence: `crates/shelbi-state/src/agents_config.rs:1-60`; the shipped `agents/adversarial/agent.yaml` has those keys; `crates/shelbi-cli/src/commands/agent.rs:181-200`.
  - Fix: "`instructions.md` is the agent's role prompt. An optional `agent.yaml` sets its runner, model, and permission posture; Shelbi appends the issue and a completion section at launch."
- [severity: wrong] "Your repo layout, house style, and the test command already live there" (lines 39-41, about `_shared/preamble.md`)
  - Reality: the shipped `_shared/preamble.md` contains only hub-safety rules. Project context is something you add.
  - Evidence: `crates/shelbi-state/src/default_shared_preamble.md.template`.
  - Fix: "Put your repo layout, house style, and test command there."
- [severity: unclear] "This creates `~/.shelbi/projects/<project>/agents/adversarial-review/`" (lines 24-25)
  - Reality: the path depends on config mode. In-repo projects use `<repo>/.shelbi/agents/<name>/`.
  - Evidence: `crates/shelbi-state/src/lib.rs:773-778`.
  - Fix: Add "(or `<repo>/.shelbi/agents/…` for in-repo projects)".
- [severity: unclear] "it has to be lowercase kebab-case (`a-z`, `0-9`, `-`, `_`)" (lines 29-30)
  - Reality: uppercase ASCII is accepted. Names can't start with `.` or `_`.
  - Evidence: `crates/shelbi-cli/src/commands/agent.rs:282-301`.
  - Fix: "letters, digits, `-`, `_`; can't start with `.` or `_` (kebab-case recommended)".
- [severity: unclear] Marker command `printf … > .shelbi/shelbi-transition.tmp && mv …` (lines 134-135, 177-179)
  - Reality: `.shelbi/` is created lazily, so the composed gate prompt runs `mkdir -p` first.
  - Evidence: `crates/shelbi-orchestrator/src/workspace.rs:6129-6133`.
  - Fix: Prefix with `mkdir -p .shelbi && `.
### ai-prompts.mdx
- [severity: wrong] "Then inspect the .shelbi/ config and project YAML in this repo, compare them against the docs" (line 46, "Debug my Shelbi setup" prompt)
  - Reality: in the default global config mode, which is what the setup card / `-y` writes, the project config is not in the repo. It lives at `~/.shelbi/projects/<id>.yaml`, with workflows, statuses and agents under `~/.shelbi/projects/<id>/`. Only in-repo mode commits `<repo>/.shelbi/project.yaml`. In global mode the repo's `.shelbi/` holds only worktrees, hooks and markers.
  - Evidence: `shelbi init --help` ("`-y` … detected plan uses the onboarding card's global layout"; `--mode global` "keeps everything under `~/.shelbi/projects/`"); `site/content/docs/configuration/project.mdx:19-20`.
  - Fix: "Then inspect the project's Shelbi config (`~/.shelbi/projects/<project>.yaml` and `~/.shelbi/projects/<project>/workflows|agents/` in the default global mode, or `<repo>/.shelbi/project.yaml` plus `~/.shelbi/projects/<project>/local.yaml` in in-repo mode), compare it against the docs…"
- [severity: unclear] "Read the Getting Started section, install Shelbi, run the detected preflight, and help me review its one-question setup card." (line 28)
  - Reality: "run the detected preflight" names no command; the preflight runs from `shelbi` / `shelbi project add`. On a fresh machine `shelbi` now opens the launch screen (see install.mdx). `/llms.txt` has no "Getting Started" heading: all of `guides/` is one section. The setup flow also asks more than one question (see first-project.mdx).
  - Evidence: `site/lib/docs.ts` (section = first directory); `site/contentlayer.config.ts:29-45`; `crates/shelbi-cli/src/wizard.rs:350-363,1184`.
  - Fix: "…read the Getting Started guide (https://shelbi.dev/docs/guides/getting-started.md), install Shelbi, run `shelbi project add` in this repository, and help me review the preflight and its setup card before I press Enter."
- [severity: outdated] "Shelbi is an open-source, terminal-native agent orchestrator…" (line 22)
  - Reality: v0.11.0 also ships a native macOS app (`Shelbi.app`, `shelbi-desktop` cask) as a second client.
  - Evidence: `site/content/docs/changelog.mdx:14-34`; `desktop/README.md:20-27`.
  - Fix (optional): "…an open-source agent orchestrator for the terminal (with a native macOS app)…"

### changelog.mdx
- [severity: wrong] "### July 9, 2026 / **v0.3.0. A batteries-included default install.** … six agent presets … two workflows instead of one. `task` … `subtask` … `default_workflow: task`" (lines 498-518)
  - Reality: v0.3.0 was tagged 2026-07-09 00:34 EDT. The reviewer presets (#357), task/subtask workflows (#359), wizard summary (#360) and default→task alias (#361) all landed after the v0.3.2 tag (22:08 EDT that day), so they first shipped in **v0.4.0**. v0.3.0 actually contained (v0.2.0..v0.3.0): kanban popover move shortcuts and mouse scroll (#340/#341), blank rows between cards (#343), stdin task bodies (#337), project-name normalization (#347), and the Homebrew `brew trust` doc (#346).
  - Evidence: `git log v0.3.2..v0.4.0` and `git log v0.2.0..v0.3.0`; tag dates from `git for-each-ref`.
  - Fix: Move the "batteries-included" paragraph under the July 10 / v0.4.0 entry (or drop the "v0.3.0." label), and add a one-line v0.3.0 entry for what it shipped.
- [severity: missing] No entry for v0.3.1 and v0.3.2 (both tagged 2026-07-09)
  - Evidence: `git log v0.3.0..v0.3.2`: #349 (test + version bump), #351 "defensive chokepoint so an un-normalized on-disk project name can't crash launch", #352 (test).
  - Fix: Add "v0.3.1/v0.3.2: patch releases; launch no longer crashes on an un-normalized on-disk project name."
- [severity: wrong] "### July 8, 2026 / **v0.2.0.**" (line 520)
  - Reality: v0.2.0 was tagged 2026-07-07 21:51:58 -0400, and all its commits are dated 07-07. Every other heading uses the local (EDT) tag date.
  - Evidence: `git for-each-ref --format='%(taggerdate:iso)' refs/tags/v0.2.0`.
  - Fix: "### July 7, 2026".
- [severity: wrong] "**`shelbi reload <target>`.** Reload one piece in place, a workflow, an agent's instructions, or config, without restarting the whole hub." (lines 482-483)
  - Reality: the target has always been a pane (`chat|tasks|activity|sidebar|workspace <name>|all`), never a workflow, agent or config. That was true at v0.4.0 and still is.
  - Evidence: #365 commit message ("shelbi reload chat|tasks|activity|sidebar / shelbi reload workspace <name>"); `git show v0.4.0:crates/shelbi-cli/src/main.rs` lines 235-243; `shelbi reload --help` ("What to reload: chat, tasks, activity, sidebar, workspace, or all").
  - Fix: "Respawn one pane in place (`chat`, `tasks`, `activity`, `sidebar`, or `workspace <name>`) without bouncing the whole hub."
- [severity: wrong] v0.4.0 "**Auto-resume of usage-limit-stalled workers.**" (line 489) and "**Sturdier under bad remotes.** `list` and `status` bound their per-workspace probes…" (line 493)
  - Reality: both landed after the v0.4.0 tag (07-10 22:25 EDT) and shipped in v0.5.0. The worktree-detach half of line 493 (#367) is correctly in v0.4.0.
  - Evidence: `3621526e 2026-07-13 feat(poller): auto-resume a usage-limit-stalled worker… (#371)`; `712cdcf6 2026-07-11 fix(list): bound workspace probes… (#370)`; neither is in `git log v0.3.2..v0.4.0`.
  - Fix: Move both paragraphs (except the worktree-detach sentence) to the July 15 / v0.5.0 entry.
- [severity: wrong] "`shelbi migrate-to-in-repo` converts an existing global project in one step" (line 603)
  - Reality: the command was always `shelbi project migrate-to-in-repo`, never top-level.
  - Evidence: commit 9aeaa144 (#115, 2026-07-01: "Adds `shelbi project migrate-to-in-repo [--project NAME] [--dry-run] [--yes]`"); `shelbi project --help` lists `migrate-to-in-repo`; `shelbi --help` has no top-level migrate.
  - Fix: "`shelbi project migrate-to-in-repo` converts…"
- [severity: missing] Nothing after v0.11.0 (2026-10-23), which is covered, though 68 commits have landed on main since
  - Reality: user-visible changes since v0.11.0:
    - the Changelog view and `shelbi changelog` command;
    - the sidebar nav changing from Orchestrator/Issues/Activity to Orchestrator/Issues/Changelog (🚀), with Activity moving to the palette;
    - TUI Settings (General, Keybindings, Workflows Form+Builder, Agents, Workspaces/Machines; #1902-#1921);
    - TUI About / Acknowledgements / Contact Us, and the sidebar footer ⚙/📣 toolbar replacing the `^P palette q quit` hint (#1934, #1958);
    - the launch screen (#1985);
    - the desktop Add Project wizard and Settings ▸ Workspaces (#1876, #1898, #1903-#1906);
    - board search asking GitHub (#1984), "start only issues assigned to you" (#1968), and inline task editing (#1991, #1992).
  - Evidence: `git log v0.11.0..a749b0d1`; `git show v0.11.0:crates/shelbi-app/src/view.rs` (`default_nav_items` = Orchestrator/Issues/Activity) vs `crates/shelbi-app/src/view.rs:365-383` now.
  - Fix: Add an "Unreleased (on main)" entry, or cover these in the next release entry, at minimum the Changelog view/command, the launch screen, TUI Settings and the footer toolbar.

### Mockups: site/components/KanbanMockup.tsx (`AppMockup`) and site/components/mdx-components.tsx
`<AppMockup>` is the only mockup used in docs MDX. `TerminalBanner` is registered in mdx-components.tsx but unused by docs pages, and `BoardAnimation` is marketing-only.

Engine-level mismatches with the real TUI (these affect every docs mockup):
- [severity: outdated] Sidebar nav `💬 Chat / 📋 Tasks / ⚡ Activity` (`NAV_ITEMS`, KanbanMockup.tsx:320-324)
  - Reality: the nav is `💬 Orchestrator / 📋 Issues / 🚀 Changelog`. Activity and Machines are reachable only from the palette.
  - Evidence: `crates/shelbi-app/src/view.rs:365-383` (verified); `crates/shelbi-app/src/command_model.rs:22-31` (`nav_glyph`); `crates/shelbi-tui/src/shell/mod.rs:11434-11447` (test "Activity isn't in the sidebar").
  - Fix: `NAV_ITEMS = [{view:"chat",glyph:"💬",label:"Orchestrator"},{view:"tasks",glyph:"📋",label:"Issues"},{view:"changelog",glyph:"🚀",label:"Changelog"}]`; keep Activity as a non-nav view.
- [severity: wrong] Board title row `Tasks · <project>   N total` (`titleRow`, lines 576-579)
  - Reality: the TUI draws `Issues · <project>   N total`.
  - Evidence: `crates/shelbi-tui/src/kanban.rs:2130`.
  - Fix: Change "Tasks" to "Issues".
- [severity: wrong] The Activity view reuses the board header (`Activity · project N total  Workflow: All ▾ Workspace: All ▾`) with the footer `j/k scroll  r refresh  esc tasks` (lines 608-610); used by concepts/events-log.mdx:131
  - Reality: the title is `Activity   <project>` (cyan "Activity") followed by an `All · Zen · Workspaces` filter-pill row. The footer is `j/k scroll · u/d page · g top · G bottom · r refresh · a/z/w filter`, with no `esc tasks`.
  - Evidence: `crates/shelbi-tui/src/activity.rs:453-468,541-570`; `shelbi config list-actions` (activity rows).
  - Fix: Give Activity its own header, pill row and footer.
- [severity: wrong] Board footer `h/l col  j/k row  ⏎ open  n new  f filter  r refresh` (lines 611-618)
  - Reality: there is no `n` (new) binding on the board. The real hint line is `h/l col   j/k row   ⏎ open   H/L move col   K/J reorder   ⇥ workflow   f filter   / search   r refresh`, plus `o workspace` when the selected card is assigned.
  - Evidence: `crates/shelbi-tui/src/kanban.rs:2730-2765`; `shelbi config list-actions` (no kanban "new" action); `crates/shelbi-tui/src/handlers/kanban.rs:94-105`.
  - Fix: Replace the hints with the real list and drop `n new`.
- [severity: outdated] Sidebar footer `^P palette  q quit` (line 1338) and no search box in the header (lines 1245-1251)
  - Reality: the hint lines are gone. The header is the project title plus a filled 2-row `🔍 Search  ⌃P` box. The footer is a toolbar (⚙ Settings, 📣 Send Feedback, `!` unread errors), with the `ZEN MODE ON` band below it.
  - Evidence: `crates/shelbi-tui/src/shell/sidebar.rs:8-21,72-90,1058-1100`.
  - Fix: Render the search box and the toolbar row in place of the hint line.
- [severity: wrong] A `▾ <machine>` group header is drawn for every scenario (line 1258ff), including single-machine boards
  - Reality: rows are grouped under machine headers only when the project spans more than one machine; a single-machine pool is a flat list. A collapsed header reads `▸ name (N, M active)`.
  - Evidence: `crates/shelbi-app/src/view.rs:570-580` ("a single-machine project collapses to a flat list"); `crates/shelbi-tui/src/shell/sidebar.rs:917-925`.
  - Fix: Skip the header when there's one machine. Affects feature-branch, forking, git-flow, trunk-based, add-to-workflow, first-task, enable-zen-mode and review-workspaces (all single `hub`).
- [severity: wrong] Review entries
  - Reality:
    - The Queued `·` is blue (line 1379), but the real one is DarkGray/muted.
    - The branch line has no marker (~line 1405), but the real one is `  ⎇ <branch>`.
    - The doc comments (lines 180-190) and the presets/usages use `location: "hub:3000"` / `"devbox:3001"` (machine:port) (line 2225ff; feature-branch.mdx:78, forking.mdx:79, git-flow.mdx:80). The real location is `machine:workspace-name`, e.g. `hub:review-0`. review-workspaces.mdx:170 already has it right.
  - Evidence: `crates/shelbi-app/src/view.rs:211-226` (Pending = DarkGray); `crates/shelbi-tui/src/shell/sidebar.rs:980-995`; `crates/shelbi-orchestrator/src/sidebar_model.rs:327` (`format!("{}:{}", ws.machine, ws.name)`).
  - Fix: Make the queued dot muted gray, prefix branch lines with `⎇ `, and use `machine:workspace` locations.
- [severity: wrong] Review-tagged workspaces listed under Workspaces (defaultAppState charlie/golf/hotel "Reviewer", lines 2206, 2215-2216); agent labels passed through lowercase
  - Reality: review-tagged slots are skipped from the Workspaces section and appear only through the Ready/Queued review sections. The agent label is the title-cased agent name, and the shipped agent is `review`, so "Reviewer" is not a real label. Lowercase `developer`, `qa`, `security`, `reviewer`, `adversarial-review` passed by usages would render title-cased.
  - Evidence: `crates/shelbi-orchestrator/src/sidebar_model.rs:141,197-206`; `crates/shelbi-app/src/view.rs:227-237` (`title_case`).
  - Fix: Drop review slots from Workspaces in presets/usages, and title-case agent labels in the component.
- [severity: wrong] Card meta branch hardcoded as `⎇ shelbi/<slug-of-title>` (line 510)
  - Reality: the shipped `task` workflow cuts `{{github_user}}/{{id}}` (e.g. `jlong/fix-login`), with fallback prefix `user/`.
  - Evidence: `crates/shelbi-core/src/workflow.rs:1021-1026`; `crates/shelbi-orchestrator/src/branch.rs:96-109`.
  - Fix: Render `⎇ <user>/<card.id>` (e.g. `you/…`).
- [severity: wrong] Default preset `workflow: "app"` badge (line 2121), inherited by every usage that spreads the default preset
  - Reality: the shipped workflows are `task` and `subtask`, so the badge would read ` task `. It only shows when more than one workflow is loaded and no filter is applied.
  - Evidence: `crates/shelbi-app/src/board.rs:270-275`; `crates/shelbi-core/src/scaffold.rs` (task.yaml/subtask.yaml).
  - Fix: Use `task` (or omit).
- [severity: wrong] Empty columns render as full 22-cell columns with `(0)` (affects starterAppState, enable-zen-mode, multi-workspace, first-task, review-workspaces)
  - Reality: an empty column auto-collapses to a narrow vertical strip (`ColumnExpansion::Auto` collapses at count 0). The engine already supports `collapsed: true`.
  - Evidence: `crates/shelbi-app/src/board.rs:440-468`; `crates/shelbi-tui/src/kanban.rs:2464-2556`.
  - Fix: Default empty columns to `collapsed: true`.
- [severity: missing] No Canceled column; the `Category` type (line 135) has no archived/dark-gray value
  - Reality: the default statuses are Backlog, Todo, In Progress, Review, Done and **Canceled** (archived, dark gray). With no workflow filter the board shows every catalog status, so a default board has six columns, with CANCELED collapsed when empty.
  - Evidence: `crates/shelbi-core/src/statuses.rs:186-221`; `crates/shelbi-app/src/board.rs:34-55`; `crates/shelbi-tui/src/kanban.rs:3062`.
  - Fix: Add an `archived` category (dark gray) and a collapsed CANCELED column to default-workflow scenarios.
- [severity: outdated] Header doc comments cite `crates/shelbi-tui/src/app.rs` `WorkspaceBadge::decoration_color()` / `Row::decoration()` and `sidebar.rs render_footer`
  - Reality: badges and rows now live in `crates/shelbi-app/src/view.rs`; the sidebar renderer is `crates/shelbi-tui/src/shell/sidebar.rs`. Contributor-facing only.
  - Evidence: `crates/shelbi-app/src/view.rs:138-237`.
  - Fix: Update the source references in the comments.

Per-usage prop bug (high impact):
- [severity: wrong] `review={[]}` is not an `AppState` field (the fields are `readyReview`/`queuedReview`), so it is silently ignored. Every spread-prop usage then inherits `defaultAppState`'s 3 Ready + 2 Queued phantom review entries, `zenMode: true`, `workflow: "app"` and its `terminalTitle`.
  - Affected: content/docs/guides/getting-started/first-task.mdx:181, multi-workspace.mdx:189, enable-zen-mode.mdx:217; also cli/issue.mdx:68, cli/workspace.mdx:56, concepts/agents.mdx:51. review-workspaces.mdx:135 inherits the same through its missing `queuedReview`.
  - Result: the first-task and multi-workspace mockups (before Zen is enabled) show the green `ZEN MODE ON` band. Sidebars list review entries ("Cold-start cache", "Validate webhook payloads"…) while the board's REVIEW column is empty. cli/issue and cli/workspace show the title bar "jlong@hub — my-project" with project "shelbi".
  - Evidence: `site/components/KanbanMockup.tsx:247-256` (verified: only `readyReview`/`queuedReview`), `:2001-2020` (`{...base, ...overrides}` over `PRESETS.default`), `:2117-2230`.
  - Fix: Replace `review={[]}` with `readyReview={[]} queuedReview={[]}`, add `zenMode={false}` where Zen isn't on, `workflow="task"` (or omit), and a matching `terminalTitle`. Better: make docs `AppMockup` default to the `starter` preset.

Every `<AppMockup` in content/docs/guides/**, with its props and plausibility:
- getting-started/workflows.mdx:28 `<AppMockup terminalTitle="jlong@hub — shelbi" />`: default preset. The project "my-project" mismatches the title. Zen is on, with the "app" badge, review slots under Workspaces, and port-style locations. Not plausible for this page.
- getting-started/first-task.mdx:141 (project=myapp, activeView=tasks, five columns, hub alpha/bravo/charlie idle + delta working, `review={[]}`): the board is plausible, minus CANCELED, but it inherits ZEN MODE ON and phantom review entries.
- getting-started/multi-workspace.mdx:148 (3 in-progress cards on alpha/bravo/charlie; hub + devbox; `review={[]}`): the same inheritance bug, so the Zen band appears before the Zen guide.
- getting-started/enable-zen-mode.mdx:191 (`zenMode`, one DONE card, `review={[]}`): the Zen band is right, but 5 phantom review entries sit beside an empty REVIEW column.
- getting-started/review-workspaces.mdx:135 (REVIEW card on `review-0`, readyReview `hub:review-0`, branch `jlong/dark-mode-toggle-polish`): location and branch match the real format. It still inherits 2 phantom Queued entries, the Zen band and the "app" badge, plus a "▾ hub" header for a single machine.
- understanding-workflows/feature-branch.mdx:19: charlie is listed idle under Workspaces while serving the Ready review at `hub:3000`. A serving review slot isn't listed under Workspaces, and the location should be `hub:<slot>`. The page's `review` status (`agent: orchestrator`, no tag) wouldn't route to a review slot. CANCELED strip missing.
- understanding-workflows/forking.mdx:22 (INCOMING/TRIAGE/IN REVIEW/SIGN-OFF/MERGED; readyReview "PR #401" in IN REVIEW; bravo agent "reviewer"): not plausible. The sidebar review sections list only tasks whose status id is `review` (`crates/shelbi-orchestrator/src/sidebar_model.rs:80-83`), and this workflow has none, so Ready/Queued would be empty. The label would read "Reviewer", and the DECLINED (archived) collapsed strip is missing.
- understanding-workflows/git-flow.mdx:22 (BACKLOG/TO DO/IN PROGRESS/REVIEW/ON DEVELOP): the same charlie/location issue. Catalog order puts a collapsed CANCELED strip before ON DEVELOP, and STAGING/RELEASED would also show unless the board is filtered to `feature`, yet the chip reads "Workflow: All".
- understanding-workflows/trunk-based.mdx:25 (BACKLOG/TO DO/IN PROGRESS/DONE; delta and echo Developer): plausible, apart from the missing CANCELED strip and the "▾ hub" header.
- doing-more-with-agents/add-to-workflow.mdx:122 (TO DO / IN PROGRESS / ADVERSARIAL REVIEW / REVIEW / DONE): BACKLOG is omitted though the guide's statuses include it (it would be a collapsed strip). The REVIEW card "Add CSV export" with `queuedReview: []` is inconsistent, since the real sidebar would list it under Queued for Review. Agent labels would be title-cased. The column order and the ADVERSARIAL REVIEW label/category match the guide.

Non-guide usages with the same issues, for the other audits' awareness: concepts/orchestrator.mdx:28, concepts/workspaces.mdx:28, concepts/events-log.mdx:131 (Activity selected in a nav that no longer has it), concepts/agents.mdx:49, cli/issue.mdx:41, cli/workspace.mdx:26.

## Verified accurate
Each page above has findings, and the claims and sections below were checked and found correct, so the fix task doesn't need to re-check them.

- getting-started/index.mdx: the prerequisites callout; all internal links (`/docs/guides/getting-started/install`, `/workflows`, `/docs/concepts/agents`, `/docs/guides/understanding-workflows`) resolve.
- getting-started/install.mdx:
  - Homebrew `jlong/shelbi/shelbi` (tap `jlong/homebrew-shelbi`); the APT keyring, source line, `stable` suite and amd64 package; .deb depends on `git` and `openssh-client`.
  - Release artifact names (`shelbi_Darwin_arm64.tar.gz`, `shelbi_Darwin_x86_64.tar.gz`, `shelbi_<v>_amd64.deb`, `checksums.txt`).
  - The daemon starts on demand and exits when idle. Cmd+C / Ctrl+Shift+C copy; Option/Alt-drag passthrough (`crates/shelbi-tui/src/shell/terminal_view.rs:452-456,686-714`).
  - `shelbi machine setup`, `shelbi --version`; `curl https://shelbi.dev/install.sh | sh` clones and runs `scripts/install.sh`; the `SHELBI_INSTALL_PATH` default `$HOME/bin/shelbi`; the codesign step.
  - The `#install-from-source-for-development` anchor and the `/docs/cli/machine` link.
- getting-started/first-project.mdx:
  - Runner question "Which agent?" with `<runner> (<version>)`. GitHub preflight requirements, File system fallback, label/metadata disclosure. File-system tasks path `~/.shelbi/projects/<name>/tasks/`.
  - Card keys Enter / `c` / `q` / Esc / Ctrl+C (`wizard.rs:1225-1239`); footer `Everything above is editable later: Ctrl+Space → "Edit"`.
  - Customize prompts and their order; empty workspace pool with the first-boot interview; `✓ Project <name> created.` then the TUI launch.
  - Welcome card title and Backlog column; hint text; palette `Ctrl+P`; the "Edit Project Settings" entry.
  - git-init prompt and decline message; `shelbi project add` uses the same card.
  - Links to `/docs/cli/issue#issue-store-migrate`, `/docs/concepts/github-issues`, `/docs/configuration/project`, `/docs/concepts/config-modes` resolve.
- getting-started/first-task.mdx:
  - `shelbi -p` and project resolution from cwd against `~/.shelbi/projects/*.yaml`.
  - `shelbi issue add` flags (`--id`, `--status`, `--description`, `--depends-on`, `--prefers-machine`) and the backlog default.
  - Auto-dispatch via `~/.shelbi/events.log`, first free workspace in YAML order via `shelbi issue start <id> --workspace <name>`.
  - Workspace badge glyphs and meanings (`·` `⏵` `?` `⚠` `⏸`; `crates/shelbi-app/src/view.rs:138-166`); dev slot goes idle at handoff; review-ready marker picked up by the poller.
  - Kanban keys h/l, j/k, Enter/Space, H/L, K/J, r, Esc/q.
  - Mockup column labels and colors (BACKLOG gray, TO DO blue, IN PROGRESS yellow, REVIEW magenta, DONE green); `shelbi issue move <id> --to done` syntax; all "Next" links resolve.
- getting-started/workflows.mdx:
  - `shelbi init` ships `task` + `subtask`, fresh projects get `default_workflow: task`, and a missing configured `default_workflow` fails project load. An explicit issue `workflow:` pointing at a missing file falls back to the built-in default at runtime.
  - The two-file split (inline `name:`/`category:` rejected, tested). Unknown ids are rejected with the available ids listed. Required fields `name`, `statuses`, per-status `owner`. The six categories as a closed set.
  - Rule 4: a user-owned status with an `agent:` dispatches only under Zen.
  - Transitions are any-to-any; an undeclared edge is a pure move; a failing action short-circuits the rest.
  - Action names and semantics (`crates/shelbi-orchestrator/src/actions.rs`): the `open_pr` base chain; `merge` via `gh pr merge` or a hub temp worktree with squash/merge; the restack cascade incl. multi-parent deferral; `delete_branch` skipped while a workspace has the branch checked out.
  - The Zen merge bar is action-based (`fires_merge_bar`).
  - `target:` doesn't change where branches are cut. The `{{github_user}}/{{id}}` branch template with login/`user` fallback; an explicit `branch:` wins.
  - `required_params` validation and exact error text (tested).
  - The 🔒 badge for blocked dependents; branches are cut from the parent, and the PR base is the parent's branch. Dispatch gating is NOT verified; see the finding.
  - The `task.yaml` example matches `task_workflow()` exactly. The `subtask.yaml` statuses, transitions, git and `required_params` match, apart from the description wording.
  - The ready-marker path `<worktree>/.shelbi/shelbi-ready`; the poller hands off to the first handoff status; event line `reason=workspace:ready-marker from_category=active to_category=handoff`.
  - Reviewer presets qa/security/adversarial ship as materialized agents; `tags: [review]` routing.
  - `shelbi workflow list/show/new/edit`, with the `show` STATUS/OWNER/AGENT table in statuses.yaml order. `issue add --workflow`, `issue move --to` (validates membership, lists valid ids), `issue list --workflow`.
  - Every internal link and anchor resolves except `#required-params`.
- getting-started/custom-workflow.mdx:
  - Agent file locations: `agents/orchestrator/instructions.md`, `agents/_shared/preamble.md`, global and in-repo paths; preamble+instructions composition (`compose_agent_prompt`).
  - Reaction rules keyed on `user:*`, `workspace:ready-marker`, `heartbeat`; the "you are the scheduler" framing; free-workspace selection in declaration order honoring `prefers_machine`.
  - `shelbi zen scan` is mechanical-only; `reason=orchestrator:zen-decline reason-text=<short>` exists; `shelbi issue list --status done` works; `shelbi agent show` prints instructions.md then skills.
  - All links and anchors resolve, including `/docs/concepts/orchestrator#customizing-the-prompt-per-project`.
- getting-started/multi-workspace.mdx:
  - `shelbi workspace list` header (NAME HOST AGENT INTEG STATE), `-` for an idle agent, runners resolved from the agent.
  - `issue add` flags `--prefers-machine`/`--id`/`--depends-on`. A `prefers_machine` card stays in `todo` and is never re-routed. Dispatch follows YAML declaration order and priority.
  - The 🔒 badge on blocked cards. Dispatch gating is NOT confirmed; see the `depends_on` finding under workflows.mdx.
  - Kanban via `Ctrl+P` → Issues, Shift+H/L move, Shift+K/J reorder.
  - Events-line shapes `backlog -> todo reason=user:cli from_category=backlog to_category=ready`, `in_progress` wire spelling, `workspace=alpha none -> working`; `shelbi events tail --follow`.
  - Badges `·`/`⏵`. The dev pane is torn down and idles at handoff; the orchestrator dispatches the freed slot immediately.
  - `shelbi workspace stop` releases the issue to `todo`. The "user spoke recently" dispatch guard (~30s). All links and anchors resolve.
- getting-started/review-workspaces.mdx:
  - `tags` bare-string shorthand and machine-tag inheritance. `review:` keys `setup`/`serve`/`ready`/`url`/`workdir`; `$PORT` = `$SLOT`; diff-only fallback without a `review:` block.
  - Review slots hidden from Workspaces; Queued for Review with nothing preempted; the poller rebases then moves to Review; the dev slot closes its session.
  - The `machine:workspace` badge. Review panel labels 🤓 Reviewer, 🔀 View Diff, ✍️ Edit in `<editor>`, 🌐 Open Browser (URL only); back button.
  - Editor precedence config `editor` → `$EDITOR` → vim.
  - Approve uses the forward transition and gates the merge. Reject appends the reason, bounces to the ready status, and refuses a blank reason.
  - `review.diff_command` with `{worktree}`/`{base}`/`{head}`; default `git difftool -d -y [-g] <merge-base> HEAD`; error when no `diff.tool`/`diff.guitool`; the non-zero exit message.
  - The Review agent's "load and serve, don't code" charter. Links/anchors (`#review-diff-command`, `/docs/configuration/workflow#review`, `/docs/configuration/global#configyaml`) resolve.
- getting-started/enable-zen-mode.mdx:
  - `zen.checks.local` semantics (run in the worktree, exit 0); `danger_paths.extend` vs `override`; built-in list plus shape-detected globs.
  - The `shelbi zen status` output format and danger-path list match exactly once `ci_timeout` is numeric. on/off/pause semantics match `zen --help` and the template.
  - Failure reason tokens (`zen:failed-checks cmd= exit=`, `zen:diff-too-large files= lines=`, `zen:danger-path paths=`, `zen:merge-conflict files=`, `zen:ci-timeout duration=`) match the activity parser.
  - `review -> done reason=orchestrator:zen-merge_pr=<n> from_category=handoff to_category=done` matches live logs. The CAS-lease landing and "protected / merge queue stays in review" claims match the template. Links resolve.
- understanding-workflows/index.mdx: all links and anchors resolve. Statuses are by reference: an inline `name:`/`category:` in a workflow is rejected (`crates/shelbi-state/src/workflows.rs:36-40`). The action-based merge bar matches `is_merge_transition`/`fires_merge_bar`.
- understanding-workflows/trunk-based.mdx: the YAML keys are a valid schema (`git.base_branch`/`merge_strategy`, `statuses` owner/agent, `initial_status`, `transitions`, `zen.checks.local`). `shelbi zen probe` exists. The action names are valid.
- understanding-workflows/feature-branch.mdx: the statuses.yaml catalog is exactly the six shipped statuses. The commented transitions in `default.yaml` match the scaffold. The legacy no-transitions fallback (`fires_merge_bar`) is right. The board labels (TO DO etc., `crates/shelbi-app/src/board.rs:63`) match.
- understanding-workflows/git-flow.mdx: `git.base_branch: develop` sets the cut base, and the `open_pr`/`merge` target falls back to it when no `target:` is given (`transition.rs:377-395`, `lifecycle.rs:126-132`). An explicit `branch:` field wins (`branch.rs:35`). The `merge` action never deletes the branch.
- understanding-workflows/forking.mdx: `close_pr` and `merge` exist with the described semantics. Links resolve.
- doing-more-with-agents/index.mdx: the six shipped agents (orchestrator, developer, review, qa, security, adversarial) match `shelbi agent list`. A gate needs a bounce edge, as stated.
- doing-more-with-agents/add-to-workflow.mdx: these all match the code:
  - the `active` category for the gate;
  - column order following statuses.yaml;
  - agents may only take declared edges once a `transitions:` block exists (`poller.rs:3397-3407`);
  - forward gate routing through the unique `in-progress→adversarial-review` edge;
  - pass to `review` / reject to `in-progress`;
  - empty `actions: []` allowed;
  - action descriptions matching `shelbi action --help`.
- doing-more-with-agents/adversarial-review-agent.mdx: these all match the code:
  - `shelbi agent new` creates `instructions.md` + empty `skills/`, and `edit`/`list`/`show` behave as described;
  - `_shared/preamble.md` is prepended;
  - the transition-marker path `<worktree>/.shelbi/shelbi-transition`;
  - the two-line format, the line-1 task-ownership check, and the `bounce`/`reject` verbs (`workspace.rs:874-920`, `poller.rs:3340-3420`);
  - `$TASK_ID` is in the pane env (`workspace.rs:5581`);
  - an undeclared edge is rejected and the marker cleared;
  - `shelbi agent list` shows "-" for unreferenced agents.
- Not a finding: `restack` in a transition's `actions:` errors at runtime (`transition.rs:579-584`). None of these pages puts it in YAML.
- ai-prompts.mdx:
  - The `.md` twin of every docs URL is real (`next.config.ts` rewrite `/docs/:slug(.*).md` → `app/docs-md/[...slug]/route.ts`), and `/llms.txt` / `/llms-full.txt` exist (`app/llms.txt`, `app/llms-full.txt`, `lib/llms.ts`). CopyPromptBanner prompts are emitted into the markdown.
  - Every URL in the prompts exists. The preflight, card and Customize (`c`) flow exist (`wizard.rs:366,1184`). The Zen description matches `shelbi zen --help`, and `zen.checks.local` exists.
- changelog.mdx:
  - Dates and versions for v0.11.0 (Oct 23), v0.10.0 (Oct 5), v0.9.0 (Aug 25), v0.8.0 (Aug 10), v0.7.2 (Aug 5), v0.7.1 (Aug 4), v0.7.0 (Aug 1), v0.6.0 (Jul 18), v0.5.0 (Jul 15), v0.4.0 (Jul 10) and v0.1.0 (Jul 7) match the tags. Spot-checked PR numbers (#543, #582, #519, #1507, #1683, #1855, #1609, #1397, #1229) match.
  - Commands and flags exist as described: `shelbi attach`/Ctrl+], `session ls|new|kill|send|snapshot`, `machine setup|status`, `relay` (hidden), `zen probe|ci-watch|scan|dry-run|on|off|pause|status`, `msrv-check`, `doctor`, `message status <id>`/`--wait`, `issue start`, `workspace add|rm`, `guard uninstall`, `agent list|show|new|edit`, `open`, `events tail`. The retired `spawn/archive/tail/merge/popup` are gone.
  - Config and UI claims check out: `theme:` auto|dark|light, `desktop.window_opacity|window_blur`, `review.diff_command`, the `disk:` block, kanban `o`, desktop ⌘⇧N and ⌘,.
  - The Homebrew `shelbi-desktop` line, `Shelbi-<version>-arm64.zip` and macOS 15+ match `docs/release/homebrew-tap.md` and `desktop/README.md`.
  - All internal links and anchors resolve.
- KanbanMockup.tsx: these match the TUI:
  - column header text/casing (`column_label`: BACKLOG / TO DO / IN PROGRESS / REVIEW / DONE);
  - category colors (gray/blue/yellow/magenta/green);
  - the `LABEL (N)` header with a dark-gray count;
  - the `Workspace: All ▾` / `Workflow: All ▾` chips;
  - the blank row between cards, two-line title wrap, and the `⎇` glyph;
  - `— Section —` headers;
  - the Working `⏵` green and Idle `·` dark-gray badges and the "idle" label;
  - Ready `✓` cyan;
  - the full-width `ZEN MODE ON` band;
  - Ready-then-Queued order.
- mdx-components.tsx: the registered components exist.
## Resolution

Fixed on branch `jlong/docs-audit-fixes` (task `docs-audit-fixes`), 2026-10-29, rebased on origin/main 9089caab. Each finding was re-verified against current source and/or the installed `shelbi` 0.11.0 before editing. Commit SHAs are pre-merge; the PR squash-merges them into one commit on `main`. Per-finding status: **fixed**, **skipped (reason)**, or **not reproducible**. `site/app/llms*.txt` and `docs-md` are generated from content at build, so they needed no hand edits. `npm run lint` and `npm run build` pass in `site/`.

### Coordinator follow-ups (applied across groups)
- **`depends_on` reconciled.** The groups disagreed. In source, the badge (`Issue::is_blocked`) is advisory, but `lifecycle::resolve_base_branch` (called by `ensure_branch_for_in_progress` on every move to In Progress) refuses the branch cut while any dep is in a backlog, ready or archived status, and stacks on an active/handoff dep's branch. first-task, multi-workspace and workflows now all say this. The audit's "display-only" claim (theme 9) was half right.
- **Stale link text.** Links to the retitled custom-workflow page now read "Customize the orchestrator prompt" (first-task, index, enable-zen-mode, understanding-workflows index, feature-branch, git-flow).
- **New prose em dashes** removed (git-flow). Ones inside verbatim UI or template text are kept.
- **Not fixed (outside docs scope):** the home page `HeroAnimation.tsx` palette data still lists Chat/Tasks/Activity, and `HeroAnimation`/`BoardAnimation` use `workflow: "app"`. These are marketing illustrations, not docs mockups.

### guides-a: index, install, first-project, first-task, ai-prompts, changelog (commit `ef58f6bc`)


#### Cross-cutting themes (as applied to my pages)
- 1 launch screen: applied in index, install, first-project, first-task, ai-prompts (treated as intended UX; verified `main.rs` default_entry + `launch/route.rs`; Add project form = name/repo/config location, file-system board, no preflight)
- 2 sidebar nav Orchestrator/Issues/Changelog: applied in first-task prose (mockup NAV_ITEMS: handled by mockup group)
- 4 branch `{{github_user}}/{{id}}`: applied in first-task
- 5 six statuses / Canceled: applied in first-task prose
- 6 review flow (push+PR, Queued, daemon auto-loader, Done = merge): applied in first-task
- 9 depends_on display-only: verified (only callers of `is_blocked` are badges + `issue list --ready`); applied in first-task
- 10 `review={[]}`: handled by mockup group

#### guides/getting-started/index.mdx
- index — detected-plan flow omits launch screen / issue-tracker question — fixed (also em dashes in "Where to go next" list)

#### guides/getting-started/install.mdx
- install — `brew … && shelbi` starts setup / "starts the guided setup" — fixed (launch screen + `shelbi project add`; "Verify or start later" rewritten per route table)
- install — "project YAML picks [runner] per workspace" — fixed (agents recommend, project overrides; workspace `runner:` ignored)
- install — no mention of desktop app — fixed (new "macOS desktop app" CodeTab + note)
- install — "installs the shelbi binary only" — fixed (binary + bundled configuration plugin)
- install — install.sh does more than build+copy — fixed (root prompt, SHELBI_DEFAULT_ROOT, cargo fetch, plugin install)
- install — tmux-era "only sidebar/Issues/Activity need reload" — fixed (script restarts running daemon, `--no-daemon`; `shelbi reload` for clients/orchestrator)

#### guides/getting-started/first-project.mdx
- first-project — "asks one question" / `&& shelbi` — fixed (`shelbi project add`; launch-screen callout; CopyPromptBanner updated)
- first-project — preflight sample remote/machine rows — fixed (`github.com/you/myapp`, `10 cores`; both-runners agent row noted)
- first-project — issue-tracker prompt mock — fixed (mirrors issue_tracker_setup.rs output)
- first-project — card rows (workspaces, github, no hub row) — fixed (card regenerated with real render_plan_card layout; file-system issues row noted; hub sentence reworded)
- first-project — "detected runner … for the workspaces it creates" — fixed (becomes orchestrator runner; Customize note that the Agent runner answer isn't saved)
- first-project — first-run hint shows every time — fixed (first project on this machine only)
- first-project — Welcome card on GitHub board (unverified) — fixed (verified: scaffold_welcome_task writes local markdown, github_store never reads it; qualified to file-system boards)
- first-project — missing-runner message — fixed (exact text + `✓ machine` row)

#### guides/getting-started/first-task.mdx
- first-task — fuzzy project picker — fixed (work_dir resolution + launch screen list)
- first-task — nav Chat/Issues/Activity, default view orchestrator — fixed (Orchestrator/Issues/Changelog; opens on Issues; Activity/Machines in palette)
- first-task — branch `shelbi/…` — fixed (`you/…`, noted `you` = GitHub login)
- first-task — "5-column board" — fixed in prose (six columns, empty fold); mockup columns handled by mockup group
- first-task — `review={[]}` mockup prop — skipped: handled by mockup group
- first-task — Review section (✓ Ready row, dev workspace window) — fixed (push+PR, Queued for Review `·`/`⎇`, daemon auto-load or Enter → Load for review, "No review workspace is configured.")
- first-task — two flavors of accept / orchestrator never moves to Done — fixed (Done move = `review → done` push+merge on workflow base branch; orchestrator merge then moves to Done; callout reworded)
- first-task — `--depends-on` gates dispatch (unverified) — fixed (verified not gating; 🔒 badge, stacked PR base, restack on parent merge)
- first-task — `--prefers-machine` soft hint — fixed (restricts to that machine, waits in Todo)
- first-task — kanban key table missing keys — fixed (Tab, o, /, f added)
- (also removed prose em dashes in badge table and chat sample)

#### ai-prompts.mdx
- ai-prompts — Debug prompt "inspect .shelbi/ config in this repo" — fixed (global vs in-repo paths)
- ai-prompts — setup prompt "detected preflight … one-question card" / no Getting Started section — fixed (getting-started.md URL, `shelbi project add`, preflight + card)
- ai-prompts — "terminal-native" — fixed (mentions native macOS app); also removed em dashes in summary/prompts

#### changelog.mdx
- changelog — v0.3.0 batteries-included content shipped in v0.4.0 — fixed (moved under July 10 / v0.4.0; new v0.3.0 entry from `git log v0.2.0..v0.3.0`)
- changelog — no v0.3.1/v0.3.2 entry — fixed
- changelog — v0.2.0 dated July 8 — fixed (July 7, merged under the existing July 7 heading with v0.1.0)
- changelog — `shelbi reload <target>` described as workflow/agent/config — fixed (pane targets, per #365)
- changelog — auto-resume (#371) and bounded probes (#370) listed in v0.4.0 — fixed (moved to v0.5.0; worktree-detach #367 kept in v0.4.0)
- changelog — top-level `shelbi migrate-to-in-repo` — fixed (`shelbi project migrate-to-in-repo`)
- changelog — nothing after v0.11.0 — fixed (new "October 29, 2026 / on main since v0.11.0" entry: Changelog view+command and nav change, launch screen, TUI Settings + footer/About/Contact, desktop Add Project wizard + Settings ▸ Workspaces + task pane, GitHub board search + assigned-only start)

#### Product issues noted
- Wizard Customize "Agent runner (used by every workspace)" answer (`selected_runner`) is never read by `to_project()`; only `orchestrator.runner` is saved (crates/shelbi-cli/src/wizard.rs:183-257). Docs now say so.
- Plan card footer says `Ctrl+Space → "Edit"`, but the palette is bound to Ctrl+P (no ctrl-space binding in `config list-actions`); the TUI's own first-run hint says Ctrl+P. Docs mirror the real card output.
- Shipped orchestrator prompt (default_orchestrator.md.template ~60-90, 504-520, 865-870) still says a human loads review cards from the sidebar, while the hub daemon's review auto-loader (poller.rs maybe_autoload_review_queue) does it automatically when a review slot is free.
- `--depends-on` doesn't gate dispatch (only 🔒 badge / `issue list --ready`), while `issue add --help` and older docs implied it does.

### guides-b: workflows, custom-workflow (commit `e4f72e2f`)


Every changed YAML example (the statuses catalog with `canceled`, the new illustrative `example.yaml`, task.yaml, and subtask.yaml with the corrected description) was loaded in a throwaway SHELBI_ROOT with `shelbi workflow list/show`, and all of them load. Owner and validation claims were re-tested there: a bare `owner: agent` fails, `owner: qa` migrates with a warning, duplicate transitions are rejected, an unknown agent is rejected, a malformed file is skipped by `list`, two handoff statuses only warn, and `merge_strategy: rebase` loads.

#### guides/getting-started/workflows.mdx
- "five-column board, one column per category": fixed (one column per status, six, empty columns fold to a strip)
- Default `<AppMockup>` preset (nav, columns, zen, project): handled by mockup group (JSX untouched)
- Workflow path is global-only: fixed (in-repo path added)
- `default` falls back to built-in: fixed (alias to `task` explained in "Choosing", "Per-task" and "built-in default" sections)
- Schema example labelled shipped `default.yaml`, statuses missing `canceled`: fixed (now `example.yaml`, illustrative; `canceled` added)
- `owner: agent` derives default from category: fixed (`agent:` required; rule 3 rewritten)
- "`owner` is `user` or `agent`, no other value parses": fixed (legacy named-owner migration documented)
- Validation list incomplete, `signoff` undeclared, two-handoff warning: fixed (list expanded, skip-vs-hard-fail noted, `signoff` catalog note + warning)
- Activity feed "handed off": fixed ("finished … [ready for review]")
- "All actions are hub-side": fixed (push_branch/open_pr run in the worktree on its machine; rest on hub; links /docs/cli/action)
- Transitions omit run/ready/ready_timeout/url, review:, zen:, rebase refusal: fixed (paragraph + links to configuration/workflow anchors; rebase noted in merge row)
- "canonical default workflow's transitions": fixed ("shipped `task` workflow's forward transitions", plus the cancel edges)
- Placeholders only in `git:`, "at load time": fixed (target: supports `{{var}}`; `{{id}}`/`{{github_user}}` built-ins; dispatch/merge time)
- Broken link `#required-params`: fixed (`#required_params`)
- subtask `base_branch: task/{{task}}` "resolves to parent's branch" + description quote: fixed (documents the literal `task/<parent-id>` and how to make it match; description now reads "parent task")
- `column: Designing`: fixed (`column: designing`, status id)
- "Cross-workflow moves are not allowed": fixed (`shelbi issue edit --workflow` / issue editor)
- Cycles "rejected at issue-load time": fixed (rejected on `issue add` / `issue depends`, with the error shape)
- depends_on gates dispatch / canceled parent surfaced in feed: fixed. Re-verification was more precise than the audit: 🔒 is advisory, but `issue list --ready` and `zen scan` exclude blocked issues, and the branch cut (`lifecycle::resolve_base_branch`) refuses while a parent is in backlog/ready/archived. That is documented. The "activity feed" claim is gone.
- Review status "orchestrator dispatches to review workspace": fixed (PR on in-progress→review, Queued for Review, hub daemon auto-loader or manual load, Ready for Review, Done runs merge)
- `InProgress` spelling: fixed everywhere (summary, table, prose, lifecycle diagram)
- No mention of Settings ▸ Workflows: fixed (short note under the path)
- CLI reference omissions: fixed (`shelbi action …`, `issue edit --workflow`, `issue move --skip-transition-actions`, `workflow new --edit`)
- Also fixed (cross-cutting themes): the judgment policy now points to `zenmode.md`, not the orchestrator prompt (theme 8); the review marker fires only from status id `in-progress`, with the forward-target order (gate, handoff, merge edge) (theme 7); the `merge` row and the `git:` comment now say the project's `merge_strategy` is used; `workflow: default` examples changed to `task`; em dashes removed from prose

#### guides/getting-started/custom-workflow.mdx
- Title/summary collide with YAML "workflow": fixed (now "Customize the orchestrator prompt"; filename and slug unchanged; intro and "workflow" wording now point to the YAML workflows page)
- Prompt rendered to CLAUDE.md, CLAUDE.md callout, "read that file": fixed (`.claude/agent-instructions.md`, `--append-system-prompt` / Codex startup prompt; callout says a legacy CLAUDE.md is ignored, delete it)
- `agent edit` materializes default: fixed (seeded by `shelbi init`; edit only opens it)
- Bootstrap flow list: fixed (status --full --handoff, events tail accelerator, events next/ack drain as the delivery guarantee, config upgrade --needs-judgment)
- Zen section / Tweak 2 edits the prompt: fixed (Tweak 2 now edits `zenmode.md` and keeps the first-line summary; "What to keep" says to keep the `## Zen Mode` pointer)
- Reporting example `✓ dispatched … shelbi/fix-login`: fixed (quotes the real one-line summary format)
- `prefers_machine` as a project YAML key: fixed (issue frontmatter; review slots and user-shell slots are skipped)
- reload "conversation context resets": fixed (handoff.md carries context; whole-project reload; workspaces keep running). Did NOT recommend `shelbi reload chat`: `commands/reload.rs` ignores targets and reloads the whole project.
- "edits stay put" omits the self-heal merge: fixed (runner-critical sections + `## Changelog` re-added)
- "four prompts": fixed ("every agent's prompt"; review added to the example list)

#### Product issues noted
- subtask.yaml `base_branch: task/{{task}}` never matches the shipped task branch `{{github_user}}/{{id}}`. Nothing maps it to the parent's real branch (`actions::resolve_task_base_branch` only substitutes params).
- `depends_on` doesn't hard-gate dispatch: the 🔒 badge is advisory. Only the branch cut refuses, while a parent is in backlog/ready/archived, and nothing tells the orchestrator prompt to skip blocked issues.
- A per-workflow `git.merge_strategy` is ignored (`actions::merge` uses `project.merge_strategy()`). `rebase` passes validation but fails at merge time.
- `shelbi workflow show default` prints the in-memory built-in even when `default` would alias to `task.yaml` at runtime (the CLI's `show` checks for the file before the alias).
- `shelbi reload --help` still advertises per-target reloads (`chat`, `tasks`, …), which the command now ignores, with a note.
- Not a product issue, but for the coordinator: link text still uses the old title "Author a custom (Shelbi) workflow" in first-task.mdx:286, understanding-workflows/index.mdx:49, feature-branch.mdx:164, and git-flow.mdx:223. The URL and the `#1-pin-certain-task-shapes-to-specific-workspaces` anchor are unchanged, so these links still work; only the text is stale.

### guides-c: multi-workspace, review-workspaces, enable-zen-mode (commit `0dd42bcf`)


#### guides/getting-started/multi-workspace.mdx
- multi-workspace — issue ids don't match auto-slugs — fixed (added `--id` to two `issue add` lines + a note on full-title slugs; verified `slugify` in mutate/add_edit.rs)
- multi-workspace — events lines missing `_agent=developer` — fixed (verified live `shelbi events tail`)
- multi-workspace — "✓ row in Ready for Review" / review in project work dir — fixed (Queued for Review `·`, hub auto-loads onto review slot, Review agent in the slot's own worktree, Enter opens panel, link to review-workspaces guide, no-slot fallback)
- multi-workspace — "you: merge it" then move card — fixed (Approve or move to done; `review → done` transition merges; warn against agent merge)
- multi-workspace — pane death surfaced by orchestrator — fixed (auto-restart supervisor, `supervision=gave-up reason=crash-loop`, `workspace stop`)
- multi-workspace — pool growth missing `workspace add` / in-repo path / starter review slot — fixed
- multi-workspace — (theme 9) `depends_on` "orchestrator skips it during dispatch" — fixed: badge is informational, Zen scan skips blocked backlog cards, promoting still dispatches; "When the loop stalls" bullet rewritten (verified: orchestrator prompt has no depends_on rule; only `list_ready`/Zen scan check `is_blocked`)
- multi-workspace — (theme 2) `Ctrl+P → Issues` / `Ctrl+P → Chat` — fixed (sidebar Issues / Orchestrator)
- multi-workspace — `review={[]}` mockup prop — handled by mockup group

#### guides/getting-started/review-workspaces.mdx
- review-workspaces — invalid transition YAML — fixed (braced form; now shows the shipped `task` edges `push_branch, open_pr` / `push_branch, merge`, plus an "excerpt" comment so copying doesn't drop the cancel edges); Approve text updated to "pushes and merges"
- review-workspaces — `slot:` fallback for `$PORT` — fixed (review slot must set `slot:`; verified workspace.rs `review_dispatch_extras` + "no assigned slot" prompt)
- review-workspaces — orchestrator routes onto review slot "preferring the one it ran on" — fixed (hub auto-loader, first free review slot; manual Enter/palette load; verified load.rs `autoload_review_queue`/`plan_review_autoload`)
- review-workspaces — steps present tagging as new setup — fixed (intro paragraph: init already creates tagged `review` slot + task workflow requires tag; real work is `slot:` + `review:` block)
- review-workspaces — "review panel's Actions" — fixed (Approve / Reject row)
- review-workspaces — remote slot hides View Diff / Edit — fixed (verified shelbi-app review.rs `build`)
- review-workspaces — (theme 6) entering review pushes + opens PR — fixed (added in Step 4 and Step 2)
- review-workspaces — Next section implied Zen auto-merges with a review slot — fixed (review-workspace gate noted)
- review-workspaces — em dashes in prose/summary — fixed in passing
- review-workspaces — AppMockup — handled by mockup group

#### guides/getting-started/enable-zen-mode.mdx
- enable-zen-mode — `ci_timeout: 15m` — fixed (`900` seconds; verified `duration_secs` u64 in shelbi-core model.rs)
- enable-zen-mode — first-run hotkey probe / `shelbi.yaml` — fixed (rebind via `defaults.global.zen_toggle` in `keys.yaml`; Enable Zen Mode? intro). NOTE: audit's suggested values `ctrl-backslash`/`none` don't parse in keys.yaml (silently fall back to alt-z, verified in throwaway root); doc uses `ctrl-g`, `ctrl-shift-z`, `'ctrl-\'`
- enable-zen-mode — "Three ways" missing palette / desktop, "any sidebar focus" — fixed
- enable-zen-mode — band location / off hint / ASCII sketch — fixed (bottom of sidebar under toolbar, absent when off/paused; desktop Zen pill; sketch redrawn with nav + Workspaces + toolbar + band; review sections omitted since empty sections don't render)
- enable-zen-mode — no review-workspace gate callout — fixed (warning callout before the bar)
- enable-zen-mode — pipeline ends `pr-merge` / "before it opens a PR" — fixed (probe -> orchestrator review -> pr-create if needed -> ci-watch -> move to done; pr-merge only for release flow; removed the CAS-lease landing paragraph, which describes pr-merge)
- enable-zen-mode — judgment categories in orchestrator instructions.md — fixed (`zenmode.md` paths, palette Edit Zen Mode, first-line summary)
- enable-zen-mode — ZEN badge / tinted row in sidebar activity feed — fixed (Activity view, `z` / Zen pill filter)
- enable-zen-mode — sample promoted row with category — fixed (`promoted <title>  backlog → todo`)
- enable-zen-mode — spaced vs underscore reason forms — fixed (raw-log note)
- enable-zen-mode — move to backlog "pulls card out of Zen's hands" — fixed (per-issue `zen: { enabled: false }`; only user `todo → backlog` demotions stop re-promotion)
- enable-zen-mode — crash-recovery auto-disable — fixed by documenting current behavior (verified: `__zen-orch-start/heartbeat/exit` are referenced only in comments; nothing invokes them). Section now says the orchestrator is relaunched and Zen keeps its mode; pause/off to review first
- enable-zen-mode — added `zen:review-reject` row to the failure-reason table; "merge by hand" -> accept by moving to done
- enable-zen-mode — AppMockup `review={[]}` — handled by mockup group

#### Product issues noted
- Zen crash-recovery hooks (`__zen-orch-start`, `__zen-heartbeat`, `__zen-orch-exit`) are unwired: no launch path invokes them, so a crashed orchestrator no longer flips Zen off; `status --full`/bootstrap still look for a `zen=off reason=crash-recovery` line and `zen status` still prints `last crash`.
- `keys.yaml` `defaults.global.zen_toggle` silently ignores `ctrl-backslash` and `none` (falls back to `alt-z`), yet the shipped `config.yaml` scaffold comment says "prefer setting this in keys.yaml … One of: alt-z, ctrl-backslash, ctrl-g, ctrl-shift-z, none". In keys.yaml the working forms are `'ctrl-\'` and `[]`. (configuration/global.mdx's chord table is for the legacy config.yaml key; another group owns it.)
- `depends_on` doesn't gate dispatch (only the 🔒 badge, `issue list --ready`, and the Zen scan honor it).
- `shelbi init`'s starter `review` slot has no `slot:`, so adding a `review:` serve recipe without also adding `slot:` makes the Review agent stop with a misconfiguration.

### guides-d: understanding-workflows, doing-more-with-agents (commit `d5201f87`)


Every changed YAML was validated in a throwaway SHELBI_ROOT (`shelbi init -y` and then `workflow list/show`, `status`). Checks run there: `task` fails to load when `done` is missing from statuses.yaml, and so does the project; a workflow that names a missing `reviewer` agent fails until `shelbi agent new reviewer` runs. All internal links and anchors in the edited pages resolve.

#### guides/understanding-workflows/index.mdx
- No findings. Consistency edit only: the trunk-based table row no longer says "pairs naturally with Zen auto-merge". It now says the hub merges as soon as the developer is done.

#### guides/understanding-workflows/trunk-based.mdx
- Zen auto-merge / zen.checks gate / "hold in InProgress" merge-condition sample — fixed. Verified poller.rs:2858-2915,2984-3000: a workflow with no handoff status auto-advances along the merge edge, and the merge runs before the move with no Zen involvement. Rewrote the intro, the board text, the workflow explanation, Orchestrator adjustments and the summary. Removed the `zen:` block and the merge-conditions sample. Pointed readers at the developer instructions as the gate, and at review + Zen when they want a checks gate.
- `actions: [push_branch, open_pr, merge, delete_branch]` — fixed. Changed to `[push_branch, merge, delete_branch]` and explained that no PR is opened (with no PR, `merge` integrates hub-side). The canceled edge is now `[delete_branch]`. Dropped the workflow-level `merge_strategy` (the `merge` action ignores it) and added `git.branch: '{{github_user}}/{{id}}'`.

#### guides/understanding-workflows/feature-branch.mdx
- Legacy `default.yaml` block, plus "agent: orchestrator lets Zen land the merge" — fixed. Replaced the block with the real shipped `task.yaml`: four transitions, the `git:` block, and `review` with `agent: review, tags: [review]`. Rewrote the explanation to cover the review auto-loader and Review agent, and updated Orchestrator adjustments (ready marker → hub pushes + opens PR; you move to Done → merge). Verified that Zen doesn't key off the status `agent:` field.
- Mockup branch `shelbi/cache-warm-up` (also git-flow `shelbi/rework-audit-log`, forking `shelbi/pr-401-cache-regex`) — handled by mockup group.

#### guides/understanding-workflows/git-flow.mdx
- statuses.yaml drops `done` — fixed: added `done`, plus a sentence on why all six shipped ids must stay (load failure reproduced).
- Workflow-level `merge_strategy: merge` — fixed. Removed it, and stated that `merge` uses project `git.merge_strategy` (actions.rs:503). The optional alternative now says to set it in project.yaml, notes that this is project-wide, and notes that Zen only lands squash (zen.rs:2043).
- Release `staging` active status, ready marker never fires — fixed. Rewrote the release workflow as human-driven plain status moves (staging/review/released, `owner: user`, `actions: []`): you open `develop → main` yourself and merge on GitHub with a merge commit. Explained why: the ready marker only fires from `in-progress`; push_branch/open_pr need an assigned workspace (git.rs:196); and a dispatched developer would rebase `develop` onto `main`. Removed `branch: develop` from the release issue and updated the routing rule and the See also entry.
- `base_branch: develop # feature/* branches` — fixed: added `branch: 'feature/{{id}}'`, fixed the comment, and explained the fallback naming.
- Hotfixes use `default` — fixed: now the shipped `task` workflow (intro and routing rule).
- Also: the feature workflow's `review` now uses `agent: review, tags: [review]` (theme 3).

#### guides/understanding-workflows/forking.mdx
- "Shelbi doesn't cut branches or push" / branch: PR head ref — fixed. The maintainer runs `git fetch origin pull/401/head:pr-401` and sets `branch: pr-401`. Added an example issue. Documented that a missing local branch gets cut fresh and empty (workspace.rs:6501-6536), and that the reviewer's hand-off rebases the branch and pushes it to origin under the same name.
- `push_branch` "review ref" — fixed: removed. The workflow now declares only `in-progress → review` with `actions: []`, and the page explains why a transitions block is needed (with no block, the legacy hand-off opens a PR; poller.rs:3666).
- `in-review` as the only active status — fixed: catalog keeps the shipped ids, relabeled (backlog=Incoming, todo=Triage, in-progress=In Review, review=Sign-off, done=Merged, canceled=Declined). Added `default_workflow: contribution`.
- Replaces the whole statuses.yaml — fixed (same relabeling approach; task/subtask still load, verified).
- `agent: reviewer` must exist — fixed: "create it first with `shelbi agent new reviewer`", or use the `security`/`adversarial` preset. Also noted that the reviewer gets the developer hand-off (ready marker).
- "(or a Zen-authorized reviewer agent)" — fixed: removed.
- Also: dropped `merge`/`close_pr`, since a `gh pr list --head pr-401` lookup can't find a fork PR. You merge or close the PR on GitHub and move the card. With no merge edge, Zen can't merge a contribution.
- Mockup readyReview on the IN REVIEW card — handled by mockup group.

#### guides/doing-more-with-agents/index.mdx
- "name `agent: adversarial` … in one line" — fixed: "follow Add it to a workflow with `agent: adversarial` in place of `adversarial-review`; you still declare the status and edges".
- Theme 7 sweep: added that a producer agent outside `in-progress` passes forward with the transition marker (the ready marker only fires from `in-progress`).

#### guides/doing-more-with-agents/add-to-workflow.mdx
- statuses.yaml omits `canceled` — fixed: added it, with a note on why.
- `workflows/default.yaml` with `review: agent: orchestrator` — fixed. Now `workflows/task.yaml`, `review` keeps `agent: review, tags: [review]`, and `canceled` is added. Transitions now mirror the shipped `review → done: [push_branch, merge]` plus both `→ canceled` edges, and the edge prose is updated to match.
- "`owner: agent` makes the status the orchestrator's to act on" — fixed: the hub daemon auto-dispatches the gate to the named agent with pass/reject commands (workflow.rs:350-372).
- Also: documented that the `in-progress → adversarial-review` edge must be the unique edge into an agent-owned active status, or the ready marker skips the gate (workflow.rs:340-358). Documented that the pass uses the transition marker with `review`.

#### guides/doing-more-with-agents/adversarial-review-agent.mdx
- Clean pass "by writing the review-ready marker" — fixed: the sample prompt now writes the transition marker with `review` and says not to write the ready marker. Verified compose_gate_prompt (workspace.rs:6114-6170) and poller.rs:2859.
- "instructions.md is the system prompt, no config layer" — fixed: it's now called the role prompt, the page says the issue and the completion section come with the dispatch, and it adds an `agent.yaml` bullet (not created by `agent new`, verified; editable in Settings ▸ Agents).
- `_shared/preamble.md` "already" holds the repo layout — fixed: "the shipped preamble holds only hub safety rules. Put … there." (verified against the scaffold).
- Agent path — fixed: added the in-repo `<repo>/.shelbi/agents/…` path (lib.rs:773).
- Name rules — fixed: letters, digits, `-`, `_`; can't start with `.` or `_`; kebab-case by convention (agent.rs:282-301).
- Marker commands lack `mkdir -p` — fixed in all three marker snippets.

#### Product issues noted
- A workflow-level `git.merge_strategy` is parsed, and the workflow editor in shelbi-app edits it, but the `merge` action (actions.rs:503) and Zen (zen.rs:2043) only read the project-level value.
- `push_branch`/`open_pr` require an assigned workspace (git.rs:196). A human-driven card (a git-flow release, for example) can't use them.
- The ready-marker hand-off fires only from status id `in-progress` (poller.rs:2859). A custom active-status id can't hand off by ready marker, and only gates get the transition-marker prompt.
- PR lookup for `merge`/`close_pr`/reconcile is `gh pr list --head <branch>` in the hub repo, so fork PRs are never found.
- Handoff-less workflows (trunk, subtask) merge with no Zen or checks gate. This is by design, but worth knowing.
- Minor: the unknown-agent workflow error lists `_shared` among the "available" agents.

### Mockups: KanbanMockup.tsx (component); page <AppMockup> blocks landed with each page group's commit (commit `335a2691`)


Verified against crates/shelbi-app/src/view.rs (default_nav_items, title_case, SidebarModel::rows), command_model.rs (nav_glyph), crates/shelbi-tui/src/shell/sidebar.rs, kanban.rs (render_title, render_footer, card_meta_line, render_collapsed_column, category_color), activity.rs + shelbi-app/src/activity.rs, shell/changelog.rs, shelbi-orchestrator/src/sidebar_model.rs, shelbi-core statuses.rs, keymap defaults/display (mac style), mutate output strings. Rendered every scenario offline (esbuild + react-dom/server + headless Chrome, Source Code Pro) to check alignment.

#### site/components/KanbanMockup.tsx (engine)
- Sidebar nav Chat/Tasks/Activity → `💬 Orchestrator / 📋 Issues / 🚀 Changelog` — fixed (Activity kept as a palette-only view; new Changelog pane added so the nav row works)
- Board title `Tasks · project` → `Issues · project` — fixed (workflow chip only while >1 workflow; filtered chip in cyan; chips drop when too narrow)
- Activity view reused board header/footer — fixed (own `Activity   project` title, `All · Zen · Workspaces` pills, real footer `j/k scroll · ⇞/⇟ page · g top · ⇧g bottom · r refresh · a/z/w filter`; feed rows rebuilt in the current chip/verb/title/pill layout, avatars removed)
- Board footer `n new` etc. — fixed (real hint line, mac display style, `o workspace` only when the selected card is assigned, all dim; whole hints drop when narrow)
- Sidebar footer `^P palette  q quit`, no search box — fixed (header = title + filled `🔍 Search ⌃P` box; nav block with half-row bleed; footer = ⚙/📣 toolbar with ZEN MODE ON band under it)
- `▾ machine` header on single-machine boards — fixed (headers + indent only with >1 machine; header text gray)
- Review entries (blue queued dot, no `⎇`, machine:port) — fixed (queued `·` dim, `⎇ branch` line, `machine:workspace` locations; location drops when the title leaves no room, as `right_align` does)
- Review slots listed under Workspaces / lowercase agent labels — fixed (presets drop review slots; engine title-cases agent names)
- Card branch `⎇ shelbi/<slug>` — fixed (`<user>/<id>`, user from the window title; per-card `branch` override)
- Default preset `workflow: "app"` — fixed (`task`)
- Empty columns full width — fixed (presets/usages mark them `collapsed`; collapsed strip now bold with a blank row before the count; expanded columns share a 111-cell board width like the TUI)
- No Canceled column / archived category — fixed (`darkgray` category; collapsed CANCELED in every default-catalog scenario)
- Stale source refs in doc comments — fixed
- `review={[]}` silently ignored — fixed at every usage (documented in the AppMockup comment); defaultAppState now Zen off and internally consistent
- Orchestrator transcript used `shelbi task add/start` — fixed (`shelbi issue add/start` with their real output)
- Extra: filled runs now render as full-line-height boxes and non-font glyphs (`⎇`, `✓`, `⏵`, emoji) are pinned to their cells, so fills don't seam and columns don't drift.

#### site/components/desktop/DesktopMockup.tsx
- Added the `darkgray` (Canceled) entry to `STATUS` so the widened `Category` type checks — fixed (one line, required by the type change)

#### site/components/mdx-components.tsx
- No findings needing change (registered components exist; TerminalBanner unused by docs is not a defect) — skipped

#### guides/getting-started/first-task.mdx
- `review={[]}` → phantom review entries, Zen band, "app" badge — fixed
- Five columns, no Canceled — fixed (six; empties collapsed); card id → `fix-login-bug-on-safari`
#### guides/getting-started/multi-workspace.mdx
- `review={[]}` / Zen band before Zen guide — fixed; ids/titles aligned with the page's `issue add` commands
#### guides/getting-started/enable-zen-mode.mdx
- Phantom review entries beside empty REVIEW — fixed (Zen band kept)
#### guides/getting-started/review-workspaces.mdx
- Inherited queued entries, Zen band, app badge, `▾ hub` — fixed; sidebar widened (`sidebarWidth={40}`) so the `hub:review-0` slot shows as the prose describes; alpha set idle (no in-progress card)
#### guides/getting-started/workflows.mdx
- Default preset with mismatched title / Zen / app badge / review slots under Workspaces — fixed (preset corrected; title prop dropped)
#### guides/understanding-workflows/feature-branch.mdx
- `shelbi/cache-warm-up`, `hub:3000`, charlie serving while listed idle, no Canceled — fixed (`you/t-046`, `hub:review-0` slot, charlie is a dev slot)
#### guides/understanding-workflows/git-flow.mdx
- `shelbi/rework-audit-log`, `hub:3000`, chip "All" while showing only feature columns, Canceled order — fixed (filtered to `feature`, `feature/<id>` branches, CANCELED strip before ON DEVELOP)
#### guides/understanding-workflows/forking.mdx
- `shelbi/pr-401…` branch, readyReview on IN REVIEW card, `reviewer` label, no DECLINED strip — fixed (ids/branches `pr-NNN`, review sections empty since no `review` status, single workflow so no chip/badge)
#### guides/understanding-workflows/trunk-based.mdx
- Missing Canceled strip, `▾ hub` — fixed (filtered to `trunk`)
#### guides/doing-more-with-agents/add-to-workflow.mdx
- BACKLOG omitted, REVIEW card not queued, lowercase labels — fixed (collapsed BACKLOG + CANCELED, queued entry, title-cased labels; board widened so the ADVERSARIAL REVIEW header fits)
#### cli/issue.mdx, cli/workspace.mdx
- `review={[]}` inheritance, title "my-project" vs project "shelbi" — fixed
#### concepts/agents.mdx
- `review={[]}`, mismatched title — fixed; agent label (concepts-config unverified) — verified: `right_label` = title-cased agent, now rendered that way
#### concepts/orchestrator.mdx, concepts/workspaces.mdx, concepts/events-log.mdx
- Mismatched `jlong@hub — shelbi` title; Activity selected in a nav that lacks it — fixed (title prop dropped; Activity renders with no nav row selected, as from the palette)

#### Notes for coordinator (not changed: outside my files)
- site/components/HeroAnimation.tsx palette items still read `💬 Chat / 📋 Tasks / ⚡ Activity`, and Hero/BoardAnimation use workflow badge "app"; the engine now draws Orchestrator/Issues/Changelog in their sidebars. Home page renders fine (verified hero stays 640px tall), but those labels are stale marketing data.

Counts: 33 fixed, 1 skipped, 0 not reproducible.

