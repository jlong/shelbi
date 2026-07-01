# In-Repo vs Global Project Config

## Context

Today every project's configuration lives in a global location: `/.shelbi/projects/{project}/` (and the project YAML at `/.shelbi/projects/{project}.yaml`). That dir holds the workflows, agent prompts, task board, settings templates, and runtime state. Nothing is committed to the project repo.

This is fine for solo use on a single machine. It breaks down when:

- **Sharing config with a team.** Workflow YAMLs, agent prompts, task templates, and danger-path policies are decisions teams make together. They should diff in PR review, not be re-typed on every new machine.

- **Onboarding a new dev to an existing project.** Today they have to re-run \`shelbi wizard\` and re-author every customization the project's main dev built up.

- **Reproducing a project across machines.** Cloning the repo doesn't give you the project's shelbi config; you have to bootstrap from somewhere out-of-band.

We want **the option** — not a forced migration — to commit project config to the repo at \`\<project\_root>/.shelbi/\`. The user picks at \`shelbi init\` / wizard time which mode the project uses. The global path stays supported for projects (or contributors) that prefer it.

The hard part is *what* belongs in the repo vs. what stays user-local. Some things are obviously shared (workflows, agent prompts). Some are obviously runtime state and should NEVER be committed (Zen toggle, crash flag, current task assignments). Tasks are the ambiguous middle.

## Design

### 1. Two categories: \`config\` and \`state\`

Sharply split the current \`\~/.shelbi/projects/<project>/` contents into two categories. The split is mode-independent — `config` always belongs with the project, `state` always belongs with the user. The mode just chooses *where* \`config\` lives.

**\`config\` (always shareable; goes to in-repo or global per mode):**

- \`workflows/\*.yaml\` — workflow definitions. Team decision.

- \`agents/<role>/instructions.md\` — agent prompts.

- \`agents/<role>/settings.json\` — agent settings (claude code hooks, etc.).

- \`agents/\_shared/preamble.md\` — shared agent preamble.

- \`workspace-settings.json.template\` — template applied to workspace worktrees.

- \`workflows/statuses.yml\` — status catalog.

- The project YAML's *project-level* fields (default branch, workflows reference, danger paths, zen config defaults). See §3 for how this is split.

**\`state\` (always user-local; lives in \`\~/.shelbi/projects/<project>/\` regardless of mode):**

- \`state.json\` — runtime state (Zen mode flag, last crash, notified diverged agents).

- \`tasks/\*.md\` — the kanban task board. Per-user backlog state. See §4 for why we keep this user-local even though it could in principle be shared.

- \`HANDOFF.md\` — workspace handoff scratch.

- \`.claude/\` — runtime claude-code state.

- \`workspaces/<name>/status.yaml\` — observed workspace state.

- \`events.log\` — orchestrator event log.

Anything currently in \`\~/.shelbi/projects/<project>/\` that's not in either list gets explicitly placed during this work; nothing is left undecided.

### 2. Modes — \`global\` (default, current) vs \`in-repo\`

A project is in **exactly one mode**, set at \`shelbi init\` time and stored in the project YAML.

**\`global\` mode (current behavior, default):**

- All config + state lives under \`\~/.shelbi/projects/<project>/\`.

- Project YAML at \`\~/.shelbi/projects/<project>.yaml\`.

- Discovery: reverse-lookup against \`\~/.shelbi/projects/\*.yaml\` (current behavior).

**\`in-repo\` mode:**

- Config lives at \`\<project\_root>/.shelbi/\` (in the repo, committed to git).

- State stays at \`\~/.shelbi/projects/<project>/\` (per-user, gitignored).

- Project YAML splits — see §3.

- Discovery: walk up from cwd looking for \`<repo>/.shelbi/project.yaml\` (like git's \`.git\` walk), with the global registry as fallback.

The mode flag goes in the project YAML itself: \`config\_mode: in-repo\` or \`config\_mode: global\` (omit = global).

### 3. Splitting the project YAML

The project YAML has fields that are shared (workflow choice, danger paths, default branch) and fields that are user-specific (machine list, workspace pool, hub host). In-repo mode requires splitting these so committing the project YAML to git doesn't leak per-user machine names.

**Shared (\`\<project\_root>/.shelbi/project.yaml\` in in-repo mode, full YAML in global mode):**

\`\`\`yaml
name: shelbi
default\_branch: main
config\_mode: in-repo
git:
base\_branch: main
merge\_strategy: squash
heartbeat:
interval\_secs: 180
zen:
checks:
\- cargo build --workspace
\- cargo test --workspace
danger\_paths:
\- .github/workflows/\*\*
\- scripts/install.sh
per\_workflow:
app-feature:
checks: \[...]
agent\_runners:
claude:
command: claude
flags: \[]
orchestrator:
runner: claude
workspace\_settings\_template: workspace-settings.json.template
\`\`\`

**User-specific (\`\~/.shelbi/projects/<project>/local.yaml\` in in-repo mode; absent in global mode where everything's in the main YAML):**

```yaml
machines:

- name: hub
  kind: local
  work_dir: /Users/jlong/Workspaces/shelbi

- name: devbox
  kind: ssh
  host: devbox
  work_dir: /home/jlong/Workspaces/shelbi
  workspaces:

- name: alpha
  machine: hub
  runner: claude
```

Rationale: every dev has their own machines and workspace names. The shared \`project.yaml\` is what gets committed; the local \`local.yaml\` is per-user.

### 4. Tasks stay user-local

Tasks are the most natural "wait, why aren't they shared?" candidate. Reasons to keep them user-local for now:

1. **Volume**. Every promote / dispatch / move event touches a task markdown file. Sharing means every kanban interaction is a git commit, which spams history and creates merge churn.
2. **Personal queues.** Two devs working on the same project will have different priorities, different in-flight assignments. Sharing the same kanban forces coordination that the team may not want.
3. **Tooling alternatives.** Teams that want a shared backlog usually already have one (GitHub Issues, Linear, Jira). Shelbi's task board is per-developer planning, not a team-of-record system.

**Open path for later**: a "shared queue" mode where a subset of tasks (those marked \`shared: true\` in frontmatter, or under a \`tasks/shared/\` subdir) gets committed while the rest stays local. Out of scope for this plan; possible follow-up.

### 5. Discovery and project resolution

\`shelbi-state::resolve\_project\_for\_cwd\` currently scans \`\~/.shelbi/projects/\*.yaml\` and matches the cwd against each project's \`work\_dir\`, deepest match wins. Extend to also walk up from cwd looking for \`.shelbi/project.yaml\`:

\`\`\`

1. From cwd, walk up looking for `<dir>/.shelbi/project.yaml`.
   If found: load `name` from it; that's the active project.
   Merge with `~/.shelbi/projects/<name>/local.yaml` if present.
2. Fallback: scan `~/.shelbi/projects/*.yaml` (current behavior).
   Reverse-lookup by `work_dir`.
3. Both miss → "no project specified" error.
   \`\`\`

Step 1 takes precedence so cd-ing into a repo with in-repo config "just works" without env vars.

### 6. \`shelbi init\` mode picker

Wizard Phase 2 (project setup) gains a mode question:

> Where should this project's shelbi config live?
>
> 1. In the repo (\`<repo>/.shelbi/\`) — committed to git, shared with the team.
> 2. Global (\`\~/.shelbi/projects/<name>/`) — per-user, not committed. **\[default]**

Recommend in-repo when:

- Repo has team contributors (heuristic: \`git config --get remote.origin.url\` resolves AND \`git log --format=%ae | sort -u | wc -l\` > 1).

- Repo already has \`.shelbi/\` directory.

If the wizard detects a half-set-up state (a \`<repo>/.shelbi/\` exists but no \`project.yaml\`, or vice versa) it surfaces the situation and asks for clarification rather than silently picking.

Add a flag for non-interactive use: \`shelbi init --mode in-repo\` / \`--mode global\`.

### 7. Migration command

\`shelbi project migrate-to-in-repo \[--project NAME]\` moves an existing global-mode project to in-repo mode:

1. Read the global \`~~/.shelbi/projects/<name>.yaml\` + \`~~/.shelbi/projects/<name>/\` contents.
2. Split per §1 (config to repo, state stays).
3. Write \`<repo>/.shelbi/project.yaml\` (shared fields).
4. Write \`\~/.shelbi/projects/<name>/local.yaml\` (user fields).
5. Move \`workflows/\`, \`agents/\`, templates from global to repo.
6. Leave state files (\`state.json\`, \`tasks/\`, etc.) in place.
7. Update \`config\_mode: in-repo\` in the project YAML so discovery uses the new path.
8. Print a \`.gitignore\` snippet the user should add to the repo to avoid accidentally committing future state leakage.

Idempotent: re-running on an already-migrated project is a no-op (or self-heals any half-migrated state).

Reverse command (\`migrate-to-global\`) is a follow-up — not strictly needed for v1 since git revert covers it.

### 8. \`.gitignore\` discipline

The repo's \`.gitignore\` should exclude anything that could leak per-user state into git. The migration command (or \`shelbi init --mode in-repo\`) writes / appends:

\`\`\`

# shelbi runtime state — keep out of git

.shelbi/state.json
.shelbi/tasks/
.shelbi/HANDOFF.md
.shelbi/.claude/
.shelbi/workspaces/
.shelbi/events.log
.shelbi/local.yaml
\`\`\`

The committed files are exactly what's NOT in this list: \`.shelbi/project.yaml\`, \`.shelbi/workflows/\`, \`.shelbi/agents/\`, \`.shelbi/workspace-settings.json.template\`.

## Open questions
## Resolved decisions

1. **Default mode: heuristic-driven, no fixed default.** Interactive \`shelbi init\` always asks, prefilling the answer from the team-contributor heuristic (remote origin exists AND ≥2 committer emails → in-repo; else global). Non-interactive \`shelbi init\` requires \`--mode in-repo\` or \`--mode global\` explicitly — no silent default. Rationale: eliminates the "I just ran init and now my repo has an untracked .shelbi/ dir" surprise for solo users, and avoids picking wrong for teams that don't match the heuristic exactly.

2. **Name collisions: auto-suffix on collision.** When \`shelbi init --pick-up\` sees a canonical name already registered globally on the user's machine, it registers the newly-picked-up project with a numeric suffix (\`shelbi-2\`, then \`shelbi-3\`, etc.). The committed \`project.yaml\` still carries the canonical name; only the local registry entry is suffixed. Follow-up worth confirming during implementation: does the CLI surface the suffixed name in prompts / errors, and can the user \`shelbi project rename\` to a friendlier local alias? First-cut: yes to both.

3. **Migration direction: one-way only.** Ship \`shelbi project migrate-to-in-repo\` and no reverse command. Going back to global is a \`git revert\` on the migration commit plus a state cleanup the user does by hand. Full-fidelity round-trip isn't worth building for a rare direction; if it turns out to be common, add \`migrate-to-global\` then.

4. **\`config_mode\` field lives in whichever YAML the discovery code finds first.** In-repo mode: declared in \`<repo>/.shelbi/project.yaml\`. Global mode: declared in \`~/.shelbi/projects/<name>.yaml\`. Not both — single source of truth per mode. Discovery walk-up finds the in-repo YAML first and reads its \`config_mode\`; if it walks off the top without finding one, it falls back to the global registry and reads \`config_mode\` from there. Rationale: duplicating the field in both YAMLs creates a two-source-of-truth problem where they can disagree; the mode is implied by *where* the file was found anyway, so the field is defensive documentation rather than the discriminator.

5. **\`shelbi reload\` semantics: re-read config, let in-flight workspaces finish with their existing prompt.** New dispatches after the reload get the fresh prompt; workspaces already working on a task keep the prompt they were dispatched with until they hand off. Rationale: predictable behavior, no context yanked out from under a worker mid-task, immediate effect for anything new.

## Open questions

_All resolved for v1. Anything that surfaces during implementation goes here._
## Phasing

1. **Phase 1 — Schema split.** Land the \`Project\` struct's split into "shared" and "user-local" halves in \`shelbi-core\`. Both halves load from a single YAML in global mode; from two YAMLs in in-repo mode. Tests cover both.
2. **Phase 2 — Discovery walk-up.** Extend \`shelbi-state::resolve\_project\_for\_cwd\` to walk up for \`.shelbi/project.yaml\` before falling back to the global registry. Discovery prefers in-repo when found.
3. **Phase 3 — Path helpers.** Every place that builds a path under \`\~/.shelbi/projects/<name>/` for *config* (workflows, agents, templates) gets routed through a helper that returns either the in-repo or global path based on mode. State-path helpers stay pointed at the global location regardless of mode.
4. **Phase 4 — Migration command.** \`shelbi project migrate-to-in-repo\`. Idempotent. Writes \`.gitignore\` snippet.
5. **Phase 5 — Wizard / init mode picker.** \`shelbi init --mode\` flag. Wizard Phase 2 asks the mode question with the team-contributor heuristic-based recommendation.
6. **Phase 6 — Docs + examples.** Update the docs to document both modes, the gitignore discipline, and the migration command. Include a worked example of cloning a team project that uses in-repo mode (clone + \`shelbi init --pick-up\` or similar bootstrap).

Phase 1–3 are the engine. Phase 4–5 expose it. Phase 6 makes it discoverable.
