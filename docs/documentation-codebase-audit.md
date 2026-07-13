# Documentation-to-Codebase Audit

Date: 2026-07-13

Branch: `jlong/audit-all-documentation-against-the-current-codebase`

Scope: repository documentation, the published site, shipped prompts and examples, CLI help, schemas, defaults, and supporting tests

## Executive summary

This audit found **20 deduplicated documentation issues**: **1 critical, 7 high, 10 medium, and 2 low**. The most serious problem is not isolated copy drift. The documented review-workspace contract, the shipped Reviewer prompt, and the current transition/loader implementation describe three incompatible lifecycles. A fresh Reviewer is instructed to use removed `review.*` configuration and a nonexistent `shelbi workspace serve` command, while documented review-entry commands actually run on the finishing developer workspace before that pane is closed.

Two other clusters deserve immediate attention. First, fresh and picked-up in-repo projects do not use the split `local.yaml` layout documented throughout the site; only migrated projects do. Second, the published events grammar predates the current project/workflow/category-enriched records. Anyone building tooling from the documented grammar will parse current output incorrectly.

| Severity | Count |
| --- | ---: |
| Critical | 1 |
| High | 7 |
| Medium | 10 |
| Low | 2 |
| **Total** | **20** |

| Documentation area | Critical | High | Medium | Low | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| Review, workspaces, and reload | 1 | 0 | 1 | 0 | 2 |
| Configuration, init, wizard, and migration | 0 | 2 | 2 | 0 | 4 |
| Events and orchestration continuity | 0 | 1 | 1 | 0 | 2 |
| Workflows and statuses | 0 | 1 | 1 | 0 | 2 |
| Installation and release | 0 | 2 | 0 | 0 | 2 |
| CLI and keybindings | 0 | 0 | 2 | 0 | 2 |
| Zen Mode | 0 | 0 | 1 | 0 | 1 |
| Marketing, UI examples, and comparisons | 0 | 1 | 1 | 0 | 2 |
| Shipped prompt/reference drift | 0 | 0 | 1 | 1 | 2 |
| Contributor/site metadata | 0 | 0 | 0 | 1 | 1 |

The report distinguishes confirmed errors from drift risks and environment-dependent behavior. All findings below are confirmed by repository implementation, generated `--help`, or focused tests unless explicitly labeled otherwise.

## Methodology

1. Enumerated repository Markdown/MDX, YAML examples, shipped prompt templates, site routes/components containing user-visible claims, install scripts, package/release guidance, and contributor instructions. Generated build output and task-control state were inventoried separately and excluded.
2. Built the workspace and captured help from the built binary, including root help and the `init`, `project`, `workspace`, `reload`, and `config dump-keybindings` surfaces.
3. Compared documentation against domain structs and serialization, shipped defaults, command parsers, state/path resolution, transition execution, review loading, the TUI poller, install/release automation, and focused tests.
4. Deduplicated repeated symptoms into a single finding when they have one implementation cause. Each finding lists every material documentation surface found during the audit.
5. Classified an omission only when the implemented behavior is user-facing or required to configure/operate a documented feature.

Verification commands used:

```text
cargo build --workspace
./target/debug/shelbi --help
./target/debug/shelbi init --help
./target/debug/shelbi project --help
./target/debug/shelbi workspace --help
./target/debug/shelbi reload --help
./target/debug/shelbi config dump-keybindings
```

The final validation commands and results are recorded near the end of this report.

## Coverage inventory

Status terms: **reviewed** means substantive Shelbi claims were compared with implementation; **reviewed, historical** means the page is chronology and was checked against current retained behavior where possible; **excluded** means it is not a documentation source; **unverifiable** identifies the part that cannot be established from this repository.

### Repository and contributor surfaces

| Surface | Status | Notes |
| --- | --- | --- |
| `README.md` | Reviewed | Install, quick start, architecture, build, and repository layout. |
| `AGENTS.md` | Reviewed | Contributor build, branch, release, and prose conventions. |
| `.claude/agent-instructions.md` | Reviewed as task guidance | Operational worker instructions, not product documentation. |
| `site/AGENTS.md` | Reviewed | Site structure, commands, content, and style guidance. |
| `site/README.md` | Reviewed | Contributor-facing site description; see L2. |
| `docs/release.md` | Reviewed | Current automation-oriented release runbook. |
| `docs/release-apt.md` | Reviewed | APT publication implementation and workflow inputs. |
| `docs/release/homebrew-tap.md` | Reviewed | Tap bootstrap and verification guidance. |
| `packaging/homebrew-shelbi/README.md` | Reviewed | Bootstrap tap workflow and consumer commands. |
| `examples/daily.yaml` | Reviewed | Session example; schema matches `Session`. |
| `examples/myapp.yaml` | Reviewed | Minimal legacy/global project example; valid but not advertised as fresh scaffold output. |
| `.github/workflows/*.yml`, `.goreleaser.yaml`, `scripts/release/*` | Reviewed as implementation evidence | Automation/configuration, not prose documentation inventory. |

### Published documentation site, all 49 pages

| Group | Reviewed pages |
| --- | --- |
| Top-level | `ai-prompts.mdx`, `changelog.mdx` (historical), `maintainers/release.mdx` |
| CLI | `agent.mdx`, `attach.mdx`, `config.mdx`, `daemon.mdx`, `events.mdx`, `init.mdx`, `merge.mdx`, `message.mdx`, `open.mdx`, `project.mdx`, `reload.mdx`, `send.mdx`, `status.mdx`, `task.mdx`, `wizard.mdx`, `workflow.mdx`, `workspace.mdx`, `zen.mdx` |
| Concepts | `agents.mdx`, `config-modes.mdx`, `events-log.mdx`, `orchestrator.mdx`, `review-workspaces.mdx`, `workspaces.mdx`, `zen-mode.mdx` |
| Configuration | `global.mdx`, `project.mdx`, `statuses.mdx`, `workflow.mdx` |
| Getting started | `index.mdx`, `install.mdx`, `first-project.mdx`, `first-task.mdx`, `multi-workspace.mdx`, `review-workspaces.mdx`, `workflows.mdx`, `custom-workflow.mdx`, `enable-zen-mode.mdx` |
| Doing more with agents | `index.mdx`, `add-to-workflow.mdx`, `adversarial-review-agent.mdx` |
| Understanding workflows | `index.mdx`, `feature-branch.mdx`, `forking.mdx`, `git-flow.mdx`, `trunk-based.mdx` |

### Comparison and marketing surfaces

| Surface | Status | Notes |
| --- | --- | --- |
| `site/content/vs/conductor.mdx` | Reviewed; third-party claims unverifiable | Shelbi-side statements checked. No external product/network verification was performed. |
| `site/content/vs/cursor-background-agents.mdx` | Reviewed; third-party claims unverifiable | Same limitation. |
| `site/content/vs/devin.mdx` | Reviewed; third-party claims unverifiable | Contains the no-daemon discrepancy in H7. |
| `site/content/vs/herd.mdx` | Reviewed; third-party claims unverifiable | Shelbi workflow/CLI claims checked. |
| `site/content/vs/openhands.mdx` | Reviewed; third-party claims unverifiable | Contains the no-daemon discrepancy in H7. |
| `site/content/vs/sketch.mdx` | Reviewed; third-party claims unverifiable | Contains the no-daemon discrepancy in H7. |
| `site/content/vs/_template.mdx` | Excluded from published pages; reviewed as author template | No route is generated for the underscore-prefixed template. |
| `site/app/(marketing)/page.tsx`, `site/app/docs/page.tsx`, `site/app/vs/page.tsx`, `site/app/discord/page.tsx` | Reviewed | Visible landing/navigation copy. |
| `site/components/{Hero,HeroAnimation,BoardAnimation,FeatureGrid,ValueProps,InstallCloser,InstallCommand,KanbanMockup}.tsx` | Reviewed | Components carrying product behavior, commands, or TUI examples. |
| Other `site/app` routes and presentational components | Reviewed, no substantive product claim | Routing, metadata, search, MDX rendering, theme, headers, and footers. |
| `site/public/install.sh` | Reviewed | Hosted installation entry point. |
| `site/public/wordmark.svg`, fonts, `site/app/icon.svg`, CSS | Excluded | Static/presentational assets with no behavioral claim. |

### Shipped prompts, skills, defaults, and help text

| Surface | Status |
| --- | --- |
| `default_orchestrator.md.template` | Reviewed |
| `default_developer.md.template` | Reviewed |
| `default_review.md.template` | Reviewed |
| `default_qa.md.template` | Reviewed |
| `default_security.md.template` | Reviewed |
| `default_adversarial.md.template` | Reviewed |
| `default_zenmode.md.template` | Reviewed |
| `default_workspace_settings.json.template` | Reviewed against hook rendering and state protocol |
| `skills/load_run_detection.SKILL.md` | Reviewed |
| Clap command/argument doc comments and generated root/subcommand help | Reviewed |
| `crates/shelbi-core/src/scaffold.rs` embedded comments/examples | Reviewed |

### Excluded generated or operational material

| Surface | Status | Rationale |
| --- | --- | --- |
| `site/.next/**`, `site/.contentlayer/**` | Excluded | Generated duplicates of source pages/build output. |
| `target/**`, `site/node_modules/**` | Excluded | Build products and dependencies. |
| `.shelbi/**` task records/logs | Excluded | Orchestrator operational state, not repository documentation. |
| `docs/planning` orphan-branch content | Unavailable/excluded | Per repository guidance this is a separate orphan branch and must not be merged or treated as current shipped documentation. |

## Findings

### C1. Review workspace documentation and shipped Reviewer are incompatible with the implemented lifecycle

- **Severity:** Critical
- **Type:** Verified error spanning documentation, shipped prompt, and implementation
- **Documentation locations:**
  - `site/content/docs/concepts/review-workspaces.mdx`, “How a task enters review” and “Transition commands”
  - `site/content/docs/guides/getting-started/review-workspaces.mdx`, “What happens at handoff”
  - `crates/shelbi-state/src/default_review.md.template`, “Resolve the run command” and “Start the server”
  - `crates/shelbi-state/src/skills/load_run_detection.SKILL.md`, steps 1 and 4
- **Documented claim:** Moving into a review-owned status automatically selects a review-tagged workspace, runs the transition's `run`/`ready` commands there, starts the Reviewer, and serves the application. The shipped Reviewer is told to read `review.setup`/`review.serve` and invoke `shelbi workspace serve`.
- **Actual behavior and evidence:**
  - Ready-marker handling advances the task and immediately calls `execute_transition` while `task.assigned_to` is still the finishing developer slot: `crates/shelbi-tui/src/poller.rs:1598-1755`, especially `1734-1746`.
  - `execute_transition` resolves the command host, worktree, and environment exclusively from `task.assigned_to`: `crates/shelbi-orchestrator/src/transition.rs:131-203`.
  - Only later is the developer worktree detached and pane closed: `crates/shelbi-tui/src/poller.rs:1720-1777`.
  - Loading a review task is a separate sidebar activation path: `crates/shelbi-tui/src/app.rs:573-585` calls `load_task_by_id`; `crates/shelbi-orchestrator/src/load.rs:1-13,36-117` selects and starts the review slot but does not execute transition commands.
  - The shipped `task` workflow has only `push_branch` and `open_pr` on review entry, not `run` or `ready`: `crates/shelbi-core/src/workflow.rs:605-693`.
  - `review.setup` and `review.serve` are no longer fields in `Project`, and generated `shelbi workspace --help` exposes only `list`, `set-runner`, `stop`, and `status`: `crates/shelbi-core/src/model.rs:51-120`; `crates/shelbi-cli/src/commands/workspace.rs:22-66`.
- **User impact:** A fresh default Reviewer cannot follow its own required serving workflow. Custom review-entry commands run in the developer worktree immediately before Shelbi detaches/closes it, not in the long-lived review worktree the docs promise. Review automation can fail, run against the wrong checkout, or be killed after appearing to start successfully.
- **Recommended correction:** First choose and implement one lifecycle contract. The least surprising model is: promote/detach developer, load and assign the review workspace, then execute entry commands in that review worktree and start the agent. Add an end-to-end test proving command host/worktree, ready probe, and cleanup. Replace or remove the obsolete skill and Reviewer instructions, and document whether review loading is automatic or requires sidebar activation. Do not correct prose alone while the shipped prompt remains unusable.

### H1. Fresh and picked-up in-repo projects do not use the documented split layout

- **Severity:** High
- **Type:** Verified error and cross-path inconsistency
- **Documentation locations:** `site/content/docs/concepts/config-modes.mdx`, “Layout at a glance,” “Picking up a teammate's project”; `site/content/docs/configuration/project.mdx`, “Where it lives”; `site/content/docs/cli/init.mdx`, introductory layout and pick-up; `site/content/docs/guides/getting-started/enable-zen-mode.mdx`, “Add a zen block”
- **Documented claim:** In-repo mode always stores user-local fields in `~/.shelbi/projects/<name>/local.yaml` and shared fields in `<repo>/.shelbi/project.yaml`; wrong-side fields are rejected.
- **Actual behavior and evidence:** Fresh in-repo setup deliberately writes a full flat registry file at `~/.shelbi/projects/<name>.yaml` containing `repo` and `config_mode: in-repo`, plus only a minimal committed `name:` file: `crates/shelbi-cli/src/commands/init.rs:420-480`; test `scaffold_writes_in_repo_config_for_in_repo_mode` at `1025-1058`. Pick-up does the same at `crates/shelbi-cli/src/commands/init.rs:554-681`. The loader always prefers the flat YAML and only reads `<name>/local.yaml` when the flat file is absent: `crates/shelbi-state/src/lib.rs:635-676,753-797`. By contrast, migration really writes the split layout and retires the flat file: `crates/shelbi-state/src/migrate.rs:171-267`.
- **User impact:** Users of fresh or picked-up projects are directed to edit a `local.yaml` that does not exist and, if created, is ignored while the flat registry file remains. The documented shared/local validation boundary is not applied to those projects. Teams can believe a setting is shared or local when runtime resolution says otherwise.
- **Recommended correction:** Decide whether the flat registry file is a supported third layout or an incomplete migration. Prefer making init and pick-up write the same split layout as migration, then add load/round-trip tests. Until code changes, document the actual precedence and path differences explicitly and update every config-edit example.

### H2. Events documentation describes an obsolete record grammar

- **Severity:** High
- **Type:** Verified stale grammar and examples
- **Documentation locations:** `site/content/docs/concepts/events-log.mdx`, “Line format,” examples, “Following the log,” and “Parsing the stream”; `site/content/docs/cli/events.mdx`, introduction; repeated examples in `guides/getting-started/multi-workspace.mdx`, `enable-zen-mode.mdx`, and `concepts/workspaces.mdx`
- **Documented claim:** Workspace records begin `worker=<name>`, task records begin `task=<id>`, and the second token reliably distinguishes the two record families. Examples omit project/workflow/category fields and use `in_progress`.
- **Actual behavior and evidence:** Workspace events are `<ts> project=<project> workspace=<name> ...`: `crates/shelbi-state/src/workspace_status.rs:389-411`. Task events are `<ts> project=<project> task=<id> workflow=<name> <from> -> <to> reason=... from_category=... to_category=...`: `crates/shelbi-state/src/workspace_status.rs:576-614`. The log also contains daemon, heartbeat, Zen-mode, Zen dry-run, clarification, and pane-alive shapes. Canonical-shape tests are at `crates/shelbi-state/src/workspace_status.rs:2403-2672`. The concept page also points to the removed `worker_status.rs`/`append_worker_event`; the implementation is `workspace_status.rs`/`append_workspace_event`.
- **User impact:** Shell pipelines and external consumers built from the guide will misclassify or drop current events. Operators comparing live output with examples may diagnose valid records as malformed.
- **Recommended correction:** Replace the grammar with a record-family table covering every emitted shape, use canonical current examples, and recommend `shelbi events tail --format envelope` for machine consumers. Link each shape to its writer/test. Keep historical examples clearly labeled if retained.

### H3. Core workflow guidance omits the archived category and canceled status

- **Severity:** High
- **Type:** Verified schema/default drift
- **Documentation locations:** `site/content/docs/guides/getting-started/workflows.mdx`, opening model, default catalogue, and “Categories”; `site/content/docs/cli/workflow.mdx`, overview and generated scaffold; `site/content/docs/concepts/orchestrator.mdx`, category table; `crates/shelbi-state/src/default_orchestrator.md.template`, “How shelbi works” and CLI summary
- **Documented claim:** Workflows have five semantic categories and the default/new workflow has five statuses. The shipped orchestrator maps the active status to literal `in_progress`.
- **Actual behavior and evidence:** `StatusCategory` has six values, including `Archived`: `crates/shelbi-core/src/workflow.rs:918-930`. The canonical catalogue has six statuses, including `canceled`, and uses the ID `in-progress`: `crates/shelbi-core/src/statuses.rs:169-207`. Shipped `default`, `task`, and `subtask` workflows include the archived path: `crates/shelbi-core/src/workflow.rs:517-775`. The configuration statuses page already documents all six, creating a direct site contradiction.
- **User impact:** Workflow authors can omit cancellation/archival paths or use the wrong literal ID in agent-driven commands. The difference between five visible active board lanes and six semantic categories is obscured.
- **Recommended correction:** State that the active board normally renders five lanes while the schema has six categories, with archived statuses hidden from the active flow where applicable. Add `canceled` to default catalogues/scaffolds and update shipped prompt literals to `in-progress`. Generate these tables from `StatusCategory` and the scaffold defaults if practical.

### H4. `shelbi wizard` is documented as the interactive form of `shelbi init`, but it is a different setup path

- **Severity:** High
- **Type:** Verified command behavior drift
- **Documentation locations:** `site/content/docs/cli/wizard.mdx`, entire workflow; `site/content/docs/cli/init.mdx`, introduction and “See also”; `site/content/docs/guides/getting-started/first-project.mdx`, setup prompts and generated YAML
- **Documented claim:** `shelbi wizard` walks through the same global/in-repo setup as `shelbi init`, including the mode choice, and the shown YAML/output represents a fresh project.
- **Actual behavior and evidence:** Explicit `shelbi wizard` calls the legacy global-only wizard (`crate::wizard::run`), while no-subcommand first run calls `commands::init::scaffold_with_prompt`: `crates/shelbi-cli/src/main.rs:332-390,396-444`. The legacy wizard writes global config only and has no mode picker: `crates/shelbi-cli/src/wizard.rs:102-295`. It also appends a review-tagged workspace, sets `default_workflow: task`, and materializes task/subtask workflows, none of which appears in the guide's sample: `crates/shelbi-cli/src/wizard.rs:206-257,285-294,505-526`.
- **User impact:** A user choosing `shelbi wizard` for documented in-repo setup silently receives global mode. Fresh-config examples understate the workspace pool and workflow files, making later review behavior and file locations surprising.
- **Recommended correction:** Either route `wizard` through the init flow or document it as the legacy global-only project editor. Re-record prompts, output, and YAML from a real fresh run. Avoid calling the two commands siblings unless they share implementation and postconditions.

### H5. Source installation documentation omits its root choice and default persistent daemon installation

- **Severity:** High
- **Type:** Verified omission of side effects
- **Documentation locations:** `site/content/docs/guides/getting-started/install.mdx`, “Install from source for development”; hosted command supplied by `site/components/InstallCommand.tsx`
- **Documented claim:** The source installer builds, copies, and signs the binary; the guide does not state that it chooses a Shelbi root or installs a login service. Package-manager installs are correctly described as binary-only.
- **Actual behavior and evidence:** `scripts/install.sh` prompts for or derives the baked root, honors `SHELBI_DEFAULT_ROOT`, and exports it into the build at `scripts/install.sh:50-101`. It installs the launchd/systemd user daemon by default at `118-142`; only `--no-daemon` disables this. `site/public/install.sh:1-35` clones and executes that script, so the hosted pipe inherits the default service installation.
- **User impact:** Running the advertised source command can create and enable a persistent login service without the guide setting that expectation. Noninteractive installs also bake a path chosen by fallback rules that users are not shown how to control.
- **Recommended correction:** Document root precedence/prompting, `SHELBI_DEFAULT_ROOT`, the default daemon install, supported supervisors, and `--no-daemon`. Show a hosted/noninteractive form that can actually pass the desired environment or use a checkout invocation when flags are required.

### H6. The published maintainer runbook can publish the same tag twice

- **Severity:** High
- **Type:** Verified operational conflict and stale release state
- **Documentation locations:** `site/content/docs/maintainers/release.mdx`, “Current Release Scope,” “Tag And Publish,” and rollback commands
- **Documented claim:** Push the release tag, then run `goreleaser release --clean` locally. The page also says the tap, APT repository/domain, and owners are unresolved and releases must not ship until resolved.
- **Actual behavior and evidence:** A pushed `v*.*.*` tag automatically starts the release job, whose GoReleaser step publishes the GitHub release: `.github/workflows/release.yml:1-7,156-207`. Conditional jobs then open the Homebrew PR and publish APT: `.github/workflows/release.yml:211-315`. The repository runbook correctly says pushing the tag starts publishing and does not instruct a second local publish: `docs/release.md:24-38`. The changelog and public install page already describe released Homebrew/APT channels, contradicting the unresolved/do-not-ship section.
- **User impact:** A maintainer following the published site can race CI, encounter duplicate release failures, or mutate already-published assets from a local machine. Stale “unresolved” gates make it unclear which runbook is authoritative.
- **Recommended correction:** Replace the site runbook with the current tag-triggered flow or make `docs/release.md` the single source rendered into the site. Remove local publish commands after tag push and update rollback steps for CI-owned releases. Resolve or accurately describe downstream variables/owners.

### H7. Marketing repeatedly claims Shelbi has no daemon

- **Severity:** High
- **Type:** Verified current-product misrepresentation
- **Documentation locations:** `site/content/vs/devin.mdx`, `openhands.mdx`, and `sketch.mdx`; historical wording in `site/content/docs/changelog.mdx` should remain explicitly dated
- **Documented claim:** Current Shelbi needs “no daemons, no servers,” only tmux/SSH/git/agent CLIs.
- **Actual behavior and evidence:** `shelbi daemon` is a public root command with install/uninstall/status/restart management: `crates/shelbi-cli/src/main.rs:191-204`; `crates/shelbi-cli/src/commands/daemon.rs:1-58`. The source installer enables it by default: `scripts/install.sh:118-142`. Worker message delivery and centralized event appends prefer its Unix socket: `crates/shelbi-state/src/workspace_status.rs:1014-1142`.
- **User impact:** Buyers/operators receive a false deployment and process model, especially relevant on locked-down workstations and remote hosts. It also contradicts the daemon CLI and install guide.
- **Recommended correction:** Say Shelbi has no application server/database but does use an optional or normally installed lightweight per-user hub daemon, depending on the intended support contract. Explain that tmux remains the worker runtime and what degrades when the daemon is absent.

### M1. Project configuration reference omits implemented public fields

- **Severity:** Medium
- **Type:** Missing documentation
- **Documentation location:** `site/content/docs/configuration/project.mdx`, top-level, machine, and agent-runner field tables
- **Documented omission:** No reference for top-level `workspace_settings_template`, machine `forward`, or runner `prompt_injection`. The SSH `host` row also says both “required” and “falls back to name.”
- **Actual behavior and evidence:** `workspace_settings_template` is a shared project field: `crates/shelbi-core/src/model.rs:79-83,152-166`. `Machine::forward` is a public optional address used by remote forwarding: `model.rs:944-953`. `AgentRunnerSpec::prompt_injection` and `PromptInjectionKind` select auto/argument/stdin behavior: `model.rs:1025-1107`. `Machine::host()` explicitly falls back to name: `model.rs:985-994`.
- **User impact:** Users cannot configure custom settings templates, non-default reverse-forward addresses, or prompt delivery for custom runners from the authoritative reference.
- **Recommended correction:** Add wire-form tables, defaults, shared/local classification, and examples for all three fields; make `host` unambiguously optional with fallback semantics.

### M2. Guidance and a shipped prompt recommend commands that do not exist

- **Severity:** Medium
- **Type:** Verified stale commands
- **Documentation locations:** `site/content/docs/concepts/config-modes.mdx:174-175,283-295`; `crates/shelbi-state/src/default_orchestrator.md.template:676-701`
- **Documented claim:** Users can run `shelbi project rename`; `shelbi quit` tears down the orchestrator pane.
- **Actual behavior and evidence:** Generated project help and `ProjectCmd` contain only `add` and `migrate-to-in-repo`: `crates/shelbi-cli/src/commands/project.rs:18-70`. The root `Cmd` has no `quit` variant: `crates/shelbi-cli/src/main.rs:38-306`. Quit exists only as TUI/palette behavior, not a shell command.
- **User impact:** Collision recovery and handoff instructions end at “unknown command,” precisely when the user is trying to recover or stop the system cleanly.
- **Recommended correction:** Remove the commands or implement them. Until a rename command exists, document the safe manual rename for both flat and split layouts. Refer to the palette/TUI action for quitting.

### M3. Migration docs say the old global YAML is deleted, but it is retained as a rollback copy

- **Severity:** Medium
- **Type:** Verified behavior mismatch
- **Documentation location:** `site/content/docs/concepts/config-modes.mdx:177-195`, step 4
- **Documented claim:** Migration deletes `~/.shelbi/projects/<name>.yaml`.
- **Actual behavior and evidence:** Migration renames it to `<name>.yaml.migrated` by design: `crates/shelbi-state/src/migrate.rs:24-27,89-99,328-337`; test at `migrate.rs:688-706` asserts the rollback copy.
- **User impact:** Users may delete what they think is an orphan, or miss the available rollback artifact. Auditing the post-migration state is needlessly confusing.
- **Recommended correction:** Say “retire to `.yaml.migrated`,” explain that the loader ignores the suffix, and incorporate the copy into rollback/cleanup guidance.

### M4. Global keybinding reference invents a `review` mode and misstates accepted actions

- **Severity:** Medium
- **Type:** Verified schema/reference drift
- **Documentation location:** `site/content/docs/configuration/global.mdx`, “Modes” and “Actions”
- **Documented claim:** `review` is a valid keymap mode with navigation/body-scroll actions; “most modes” accept generic refresh/page/home actions.
- **Actual behavior and evidence:** `Action` has exactly six modes: global, sidebar, kanban, popover, activity, and palette; `MODE_NAMES` omits review: `crates/shelbi-state/src/keymap/actions.rs:20-27,99-102`. Each mode has a closed action enum, so generic undocumented actions are not accepted. `shelbi config dump-keybindings` emits the authoritative set from `Action::all`.
- **User impact:** Valid-looking `keys.yaml` entries are diagnosed/skipped, leaving default bindings in effect. Review navigation cannot be rebound as promised.
- **Recommended correction:** Generate the modes/actions table from `dump-keybindings` or `Action::all`. Remove review until it has a dispatchable action mode and list exact per-mode actions, including currently omitted palette `backspace` and popover move actions.

### M5. Zen hotkey storage and status-output examples are stale

- **Severity:** Medium
- **Type:** Verified path and output drift
- **Documentation locations:** `site/content/docs/concepts/zen-mode.mdx`, “First-run hotkey probe”; `site/content/docs/guides/getting-started/enable-zen-mode.mdx`, “Flip the switch” and sample `zen status`
- **Documented claim:** The hotkey probe persists its choice to `~/.shelbi/shelbi.yaml`; `zen status` groups and labels resolved danger paths by source as shown.
- **Actual behavior and evidence:** The probe writes legacy `config.yaml::keymap.zen_toggle`, which keymap loading migrates to `keys.yaml`: `crates/shelbi-tui/src/zen_probe.rs:160-180`; `crates/shelbi-state/src/keymap/loader.rs:288-315`. `shelbi.yaml` is only the launch index. Status prints `zen mode:`, a flat checks list, `ci timeout:`, and one resolved danger-path list, not the documented source-grouped block: `crates/shelbi-cli/src/commands/zen.rs:303-409`.
- **User impact:** Users edit the wrong file and compare real output against a format the command never emits.
- **Recommended correction:** Point to `keys.yaml` as the durable binding source and explain the one-time `config.yaml` migration. Replace the status transcript with captured output, or implement source attribution if it is a desired diagnostic feature.

### M6. The CLI reference does not cover all visible root commands

- **Severity:** Medium
- **Type:** Missing documentation
- **Documentation location:** Published `site/content/docs/cli/*` set and docs landing page
- **Documented omission:** There are no reference pages for visible `spawn`, `list`, `tail`, `diff`, `archive`, `orchestrate`, `orchestrator`, `action`, or `popup` commands. The docs landing page implies a CLI reference set but does not distinguish public, legacy, and internal transport commands.
- **Actual behavior and evidence:** Generated root help exposes all of these commands: `crates/shelbi-cli/src/main.rs:38-306`. `action` is especially user-facing because workflow documentation tells users to configure its primitives, and `orchestrator events drain` appears in the shipped orchestrator prompt.
- **User impact:** Users discover important or legacy commands only through `--help`, cannot tell their support level, and lack argument/exit/output contracts for automation.
- **Recommended correction:** Add pages for supported commands and a clearly labeled legacy/internal section for the rest. Alternatively hide internal commands and aliases from root help. Add a CI inventory check comparing Clap's visible command list with CLI-page frontmatter.

### M7. Two unrelated handoff mechanisms are documented as one orchestrator continuity path

- **Severity:** Medium
- **Type:** Cross-document inconsistency
- **Documentation locations:** `site/content/docs/cli/status.mdx`, introduction and examples; `site/content/docs/cli/reload.mdx`, continuity section; `crates/shelbi-state/src/default_orchestrator.md.template:154-180,676-729`
- **Documented claim:** `shelbi status --full --handoff` is “the exact shape the orchestrator reads when it wakes up,” while reload continuity uses `agents/orchestrator/handoff.md`.
- **Actual behavior and evidence:** `status --handoff` drains uppercase `HANDOFF.md` from the project work directory: `crates/shelbi-cli/src/commands/status.rs:37-40,253-261`. Reload asks the running agent for lowercase `agents/orchestrator/handoff.md`, injects it into the next prompt, and deletes it through a separate API: `crates/shelbi-orchestrator/src/handoff.rs`; `crates/shelbi-cli/src/commands/reload.rs:8-38`. The shipped prompt tells the agent to run the uppercase drain at bootstrap but later write only the lowercase file when asked.
- **User impact:** Operators and prompt authors cannot tell which file is authoritative, who creates it, or which command consumes it. A continuity note can be left in the wrong location and silently ignored.
- **Recommended correction:** Name the two mechanisms separately, document producer/consumer/lifetime for each, and stop calling the uppercase drain the exact reload bootstrap unless it is wired into that path. Prefer consolidating to one mechanism.

### M8. Marketing and docs landing examples use stale shipped defaults

- **Severity:** Medium
- **Type:** Verified UI/example drift
- **Documentation locations:** `site/app/docs/page.tsx:53-61`; `site/components/HeroAnimation.tsx:159-178`; `site/components/KanbanMockup.tsx:389-391,504-510,716-742,2225-2231`
- **Documented claim:** Shelbi ships three agents, the Tasks palette entry is live `shelbi list`, and task branches are `shelbi/<task-id>`.
- **Actual behavior and evidence:** Six default agents are materialized: `crates/shelbi-state/src/lib.rs` default-agent materialization and the six `default_*.md.template` files; this is correctly documented by `site/content/docs/concepts/agents.mdx`. `shelbi list` is the legacy spawn-agent list, while the board is `shelbi task list`: root help and `crates/shelbi-cli/src/main.rs:42-55`. Fresh task workflow branch prefix is `task`, and branch resolution prefers workflow/project/login prefixes: `crates/shelbi-core/src/workflow.rs:591-605,687-690`; `crates/shelbi-orchestrator/src/branch.rs:39-58`.
- **User impact:** The primary visual introduction teaches the legacy command and renders branch names users will not see from a fresh project. The docs landing understates available reviewer roles.
- **Recommended correction:** Source demo labels/defaults from shared fixtures or update them to `shelbi task list`, six agents, and `task/<id>` (or explicitly label illustrative custom prefixes).

### M9. Workflow reference incorrectly restricts reference-only status entries

- **Severity:** Medium
- **Type:** Internal documentation contradiction
- **Documentation locations:** `site/content/docs/configuration/workflow.mdx`, callout below the schema and status field table; `site/content/docs/guides/getting-started/workflows.mdx`, default YAML explanation; generated scaffold header in `crates/shelbi-core/src/scaffold.rs:254-258`
- **Documented claim:** Workflow status entries contain only `id`, `owner`, and optional `agent` because all display metadata lives in `statuses.yaml`.
- **Actual behavior and evidence:** The same reference later documents status `tags`, and the schema serializes/deserializes them: `crates/shelbi-core/src/workflow.rs:839-880`. Tags are required for capability routing and appear in default review status definitions.
- **User impact:** Authors may put routing tags in the catalogue or workspace only and fail validation/routing expectations.
- **Recommended correction:** Say identity/display/category live in `statuses.yaml`, while per-workflow status references allow `id`, `owner`, optional `agent`, and optional `tags`. Update scaffold comments from the same schema description.

### M10. Reload and workspace path descriptions retain removed panes and old locations

- **Severity:** Medium
- **Type:** Verified UI/path drift
- **Documentation locations:** `site/content/docs/cli/reload.mdx`, introduction; `site/content/docs/guides/getting-started/install.mdx`, source reload note; `site/content/docs/guides/getting-started/first-project.mdx`, rename-workspace warning
- **Documented claim:** Reload owns hidden tasks/review/machines panes and install changes require reloading sidebar/Tasks/Review; renamed worktrees are left under `~/.shelbi/workspaces/<name>/`.
- **Actual behavior and evidence:** Reload targets are `chat`, `tasks`, `activity`, `sidebar`, `workspace`, and `all`; there is no review target: `crates/shelbi-orchestrator/src/lib.rs:122-153,1272-1307`; generated `shelbi reload --help`; report fields in `crates/shelbi-cli/src/commands/reload.rs:153-180`. Workspace status lives under the Shelbi root, but git worktrees live at `<machine.work_dir>/.shelbi/wt/<workspace>`: `crates/shelbi-core/src/model.rs:955-994`; `crates/shelbi-orchestrator/src/workspace.rs`.
- **User impact:** Troubleshooting sends users to nonexistent panes/targets and the wrong directory when cleaning up renamed slots.
- **Recommended correction:** Describe current panes and targeted reloads exactly, include Activity, remove Review as a pane/target, and distinguish status directories from worktrees.

### L1. The shipped run-detection skill duplicates removed review configuration

- **Severity:** Low
- **Type:** Drift multiplier, already operationally covered by C1
- **Documentation location:** `crates/shelbi-state/src/skills/load_run_detection.SKILL.md`
- **Documented claim:** The reusable skill independently specifies `review.setup`, `review.serve`, and `shelbi workspace serve` as the supported detection/launch API.
- **Actual behavior and evidence:** Those APIs are absent as detailed in C1. The same obsolete procedure is copied into `default_review.md.template`, so updating one file can leave the other stale.
- **User impact:** Future prompt self-healing or agent composition can reintroduce the bad procedure after one copy is corrected.
- **Recommended correction:** Make the skill the single source included by the Reviewer, or remove it. Add a test that extracts every literal `shelbi ...` command from shipped prompts/skills and validates it against Clap, with an allowlist for illustrative project-specific commands.

### L2. Site contributor README points to a non-repository plan and supplies no usable setup

- **Severity:** Low
- **Type:** Contributor documentation gap
- **Documentation location:** `site/README.md`
- **Documented claim/omission:** The entire setup guidance is “See the plan: `Shelbi/Plans/shelbi-website.md` (ContextStore),” a path not present in this worktree. It does not list install/dev/lint/build commands.
- **Actual behavior and evidence:** `site/package.json` defines `dev`, `build`, `start`, and `lint`; `site/AGENTS.md` contains the real contributor commands and architecture. The referenced plan is unavailable in the repository.
- **User impact:** A contributor entering through the conventional README cannot bootstrap the site without discovering separate agent-only guidance.
- **Recommended correction:** Replace the pointer with minimal prerequisites and commands, then link to `site/AGENTS.md` for deeper conventions. If ContextStore remains relevant, label it optional/internal rather than the sole source.

## Cross-document inconsistencies and drift risks

1. **Three project layouts are described as two.** Configuration pages promise one global and one split in-repo layout; init/pick-up implement a flat in-repo registry variant; migration implements the split variant. Path examples cannot remain correct until this is resolved.
2. **Review is alternately automatic and human-triggered.** Review concept/guide pages say status entry routes automatically; the shipped orchestrator says the human activates the sidebar; `load.rs` implements activation separately from transition execution.
3. **Five visible lanes are conflated with semantic categories/statuses.** The status reference says six, while workflow/orchestrator material says five. Marketing mockups can validly show five active lanes, but schema prose must not call that the complete category vocabulary.
4. **Events examples are copied widely.** The same pre-project-prefix task line appears in events, workspace, multi-workspace, and Zen guides. A canonical generated fixture would reduce repeated drift.
5. **There are two release runbooks.** `docs/release.md` matches the CI-owned tag flow; the published site retains a manual GoReleaser flow. Rendering one source in both locations would prevent an operational split brain.
6. **CLI reference is hand-maintained separately from Clap.** Visible commands, default IDs, panes, and flags drift without a command/page inventory check.
7. **Shipped prompts are documentation with runtime consequences.** `default_orchestrator`, `default_review`, `default_zenmode`, and `load_run_detection` repeat commands and lifecycle claims from the site, but current tests do not validate those literals against the CLI/schema.
8. **Historical changelog wording can look current out of context.** “No daemon” was part of the June 23 entry, followed by a daemon feature on June 30. Keep chronology, but current comparison pages must not reuse the old deployment claim.

## Missing documentation for implemented user-facing behavior

The following gaps are either findings above or related items best fixed in the same follow-up:

- `workspace_settings_template`, `machines[].forward`, and `agent_runners.*.prompt_injection` schema and examples (M1).
- Public/legacy/internal contracts for `action`, `popup`, `orchestrate`, `orchestrator`, and legacy spawn commands (M6).
- Flat in-repo registry precedence used by fresh init/pick-up, if it remains supported (H1).
- The `.yaml.migrated` rollback artifact (M3).
- The daemon's normal installation/degraded-mode contract in current marketing and source installation (H5/H7).
- Exact record families in `events.log`, including project/workflow/category, heartbeat, Zen, clarification, and pane lifecycle records (H2).
- Exact producer/consumer distinction between `HANDOFF.md` and `agents/orchestrator/handoff.md` (M7).
- Review-entry command execution location and the fact that sidebar activation currently loads a review slot (C1).

## Documentation verified accurate

The audit also verified substantial areas that do not require correction. Representative evidence follows; this is not a claim that every sentence in these pages is immutable.

- **Repository architecture and contributor commands:** README/AGENTS workspace layout matches root `Cargo.toml` members and `site/package.json`; `cargo build/test/clippy` and `npm run lint/build` are valid scripts.
- **Root resolution precedence:** global documentation's `--root`, `SHELBI_ROOT`, legacy `SHELBI_HOME`, and fallback behavior matches `crates/shelbi-state/src/paths.rs` and generated root help.
- **Status catalogue schema:** `site/content/docs/configuration/statuses.mdx` correctly documents ID/name/category, uniqueness, category vocabulary, and validation represented in `crates/shelbi-core/src/statuses.rs:1-403`.
- **Core task CLI surface:** the add/list/show/depends/move/assign/unassign/start/resume/prio/edit/rm command set and principal flags in `site/content/docs/cli/task.mdx` match `crates/shelbi-cli/src/commands/task.rs` and Clap help. Event examples on that page should be updated with H2, but command syntax is sound.
- **Workspace CLI surface:** list, set-runner, stop, and status syntax in `site/content/docs/cli/workspace.mdx` matches `crates/shelbi-cli/src/commands/workspace.rs`; the page correctly says there is no serving subcommand, which exposes the shipped Reviewer contradiction.
- **Agent CLI and six shipped agents:** `site/content/docs/cli/agent.mdx` and `concepts/agents.mdx` match agent materialization/edit behavior and the six bundled templates in `crates/shelbi-state/src`.
- **Workflow actions and branch resolution concepts:** transition `actions`, `run`, `ready`, target, base branch, branch prefix, and merge strategy fields generally match `crates/shelbi-core/src/workflow.rs` and `crates/shelbi-orchestrator/src/transition.rs`; C1 concerns when/where review entry executes, not the wire fields themselves.
- **Daemon CLI and forward retry tuning:** `site/content/docs/cli/daemon.mdx` command set matches `commands/daemon.rs`; retry environment variables/defaults match `crates/shelbi-ssh/src/lib.rs:18-55` and TCP port configuration in the SSH forwarding implementation.
- **Message/send distinction:** `site/content/docs/cli/message.mdx` and `send.mdx` correctly distinguish durable task-message records from tmux keystroke injection, matching their command modules.
- **Project migration command flags:** `--dry-run` and `--yes`, idempotent planning, moved config directories, and `.gitignore` prompt behavior match `crates/shelbi-cli/src/commands/project.rs` and `crates/shelbi-state/src/migrate.rs`; only deletion wording and layout consistency are findings.
- **Release automation repository runbook:** `docs/release.md`, `docs/release-apt.md`, and release scripts accurately describe tag/version validation, conditional downstream jobs, artifacts, and signing at a representative level against `.github/workflows/release.yml`, `.github/workflows/release-apt.yml`, and `.goreleaser.yaml`.
- **Session and minimal project examples:** `examples/daily.yaml` matches the session schema. `examples/myapp.yaml` parses as a valid minimal global/legacy project shape; it is not labeled as current wizard output, so omission of the fresh review slot is not itself an error.
- **Branching-model guides:** feature-branch, trunk-based, git-flow, and forking examples use supported workflow fields and action names. Their organizational recommendations are policy, not implementation claims.
- **Specialized reviewer prompts:** QA, Security, and Adversarial templates align with status-owned agent selection and transition marker behavior; no nonexistent built-in commands were found in those three templates.

## Prioritized remediation plan

### P0: Restore a coherent review loop

1. Specify the review lifecycle contract, including automatic versus activated loading and the worktree in which entry commands run.
2. Fix implementation and add an end-to-end test covering ready marker, developer detach, review assignment, command execution, ready probe, agent launch, and cleanup.
3. Rewrite the Reviewer prompt and run-detection skill from the supported contract, then update both review guides and review-related UI descriptions.

### P1: Make setup and operational docs safe

1. Unify fresh init, pick-up, and migration on one in-repo layout; add load/edit/round-trip tests and then correct all path examples.
2. Replace the event grammar and copied examples from canonical writer-test fixtures.
3. Reconcile semantic six-category/status documentation and shipped orchestrator literals.
4. Decide whether `wizard` aliases init or remains legacy global-only; document one clear onboarding path.
5. Update the source install guide and current marketing with daemon/root side effects.
6. Retire the manual site release runbook in favor of the CI-owned repository runbook.

### P2: Complete generated references

1. Generate project field tables from schema metadata or maintain a schema-to-doc coverage test for all public serialized fields.
2. Generate keymap tables from `Action::all`/`dump-keybindings`.
3. Add a Clap-to-CLI-page inventory test and classify legacy/internal commands.
4. Validate literal commands in shipped prompts/skills against Clap.
5. Capture `zen status`, wizard/init, and event examples in snapshot tests used by MDX.

### P3: Consolidate examples and contributor material

1. Centralize branch-prefix, shipped-agent, board-lane, and palette demo fixtures used by marketing mockups.
2. Separate and document the two handoff files or consolidate implementation.
3. Correct reload/worktree terminology and the site contributor README.
4. Add a documentation CI job that builds the site, checks internal links, scans stale command literals, and reports schema/CLI inventory changes.

## Limitations and environment-dependent observations

- Comparison pages make claims about external products. This audit verified their Shelbi-side claims only. Competitor capabilities, pricing, hosted behavior, and current product names were not verified from external sources and are marked unverifiable in the coverage inventory.
- No macOS launchd or Linux systemd service was installed/uninstalled during the audit. Supervisor behavior was verified from shell/Rust source and tests; platform policy and permissions remain environment-dependent.
- No remote SSH host, live GitHub PR, branch protection, or APT/Homebrew publication target was exercised. Remote forwarding, CI watch, PR actions, and release behavior were verified from command code, workflow configuration, and tests rather than live external systems.
- Historical changelog claims were checked against current retained code and nearby dated entries, not reconstructed commit-by-commit. Historical statements that no longer describe the current product were not classified as errors when clearly scoped to a date.
- TUI visual descriptions were compared with renderer/state code and mockup fixtures, but no pixel-level screenshot regression run was performed. The report flags behavioral/UI terminology drift, not cosmetic differences.
- The `docs/planning` ContextStore mirror is an orphan branch explicitly excluded by repository instructions. It was not checked out, rebased, or merged into this task branch.

## Final validation record

- `cargo build --workspace`: passed.
- `cargo test -p shelbi --bin shelbi commands::init::tests -- --test-threads=1`: passed, 14 tests.
- `cargo test -p shelbi --bin shelbi commands::config::tests -- --test-threads=1`: passed, 17 tests.
- `cargo test -p shelbi-tui review_marker -- --test-threads=1`: passed, 4 tests.
- `cargo test -p shelbi-state event_writes_canonical_shape -- --test-threads=1`: passed, 2 tests.
- `cd site && npm run lint`: passed.
- `cd site && npm run build`: passed after allowing the existing `next/font` Google Fonts fetch; 173 static pages generated.
- Full `cargo test --workspace` is not clean in this environment. In the sandbox, the daemon socket test fails with `Operation not permitted`, then poisons a shared test mutex. Outside the sandbox and serially, that socket test passes, but `commands::open::pane::tests::run_clears_expected_teardown_at_startup_before_natural_exit` fails because its expected worktree is missing, after which 87 tests fail on the poisoned mutex (242 passed before/following the initiating failure). The focused audit-relevant suites above pass independently. No application code was changed by this audit.
- `git diff --check`: passed; the final diff contains only this report.
