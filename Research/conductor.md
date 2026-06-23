# Conductor

## Positioning

In Conductor's own words: *"Run parallel coding agents on your Mac."* The
landing page elaborates that Conductor lets a developer "Run parallel Claude
Code, Codex, and Cursor agents in isolated workspaces on your Mac." Each
task "gets its own workspace, branch, files, terminal, diff, and review
path." The company's stated mission is to be "the interface to manage your
coding agents" — i.e. a control surface for orchestrating a small team of
AI coders working concurrently.

Restated plainly: Conductor is a closed-source macOS desktop app that wraps
git worktrees and the major CLI coding agents (Claude Code, Codex, Cursor)
into a unified UI. The developer presses ⌘N to spin up a new isolated
workspace — a fresh worktree pinned to its own branch — drops a task
prompt into it, and an agent of their choice works the problem in the
background. The user supervises multiple such workspaces from a sidebar,
reviews the diff each agent produces inside the app, opens a PR, and
archives the workspace when done. Conductor itself does not run the
inference; the user brings their own Claude Pro/Max, ChatGPT/Codex, or
Cursor subscription, and Conductor invokes those CLIs locally.

## Model

**Architecture.** Conductor is a native macOS desktop application (Mac
only as of June 2026; Windows on the roadmap behind a waitlist). It is
not a cloud service — agents execute on the user's Mac under the user's
shell account, with the same filesystem and credential access the user
has. The documentation explicitly notes: *"Workspace isolation is
development isolation, not a security boundary. Agents and commands still
run on your Mac with your user permissions."* This is the same trust
model as running Claude Code or Codex CLI directly — Conductor adds
orchestration UX, not a sandbox.

**Workspace = git worktree.** Each "workspace" in Conductor maps 1:1 to a
git worktree on disk, pinned to a single branch. A workspace is "an
isolated copy of a project and repository for one task, issue, experiment,
or pull request." When a workspace is created, Conductor fetches from
`origin` first so the new branch is rooted in the latest remote commit,
then creates the worktree. A branch can only be checked out in one
workspace at a time — to parallelize on the same branch, the user must
create derivative branches or switch branches in other workspaces first.
Workspaces are identified by two names: the **branch name** (primary
identifier, e.g. `feature/foo`) and a **workspace directory name**
(secondary identifier, e.g. `warsaw-v2`) used in the sidebar UI.

**What a workspace contains.** Per the workspaces-and-branches
documentation, each workspace bundles:

- A git worktree on disk rooted in the workspace's branch.
- A dedicated terminal session for the agent and the user.
- A running app process (when a `Run` script is configured for the repo).
- The agent's chat context and history, scoped to this workspace.
- Workspace-specific environment variables (notably `CONDUCTOR_PORT`,
  used to give each workspace's app a unique port so multiple workspaces
  can run their dev server simultaneously without colliding).
- A `.context` folder for notes and handoffs that the agent can write to
  without polluting the commit history (not committed by default).

**Agent style.** Conductor is a **single-agent-per-workspace** orchestrator
in the steady state: one Claude Code, Codex, or Cursor session is bound to
each workspace and works that worktree's branch in isolation. The
parallelism Conductor markets is *workspace-level* parallelism (many
agents, many tasks, many branches) rather than agent-team collaboration on
one task. However, Conductor does support a "single workspace, multiple
agents" mode where several agents share one worktree's branch — useful
for review/fix/test splits where one agent writes the change, another
reviews, and a third writes tests, all collaborating on the same diff.

**Supported model providers.** Through the underlying CLIs:
- **Claude Code** — Anthropic models, including (per recent changelog
  entries) Opus 4.7 (Apr 2026) and "Claude Fable 5" (Jun 2026).
- **Codex** — OpenAI's Codex CLI, with named GPT versions including "GPT-5.5"
  (Apr 2026) and a Conductor-branded "Conductor Allegro" config.
- **Cursor** — Added as a supported agent in v0.63 (June 2026). Requires
  `CURSOR_API_KEY`; uses Cursor's model lineup. Cursor sessions do *not*
  support Plan/Fast modes — only Claude Code and Codex do.
- **Custom providers** — The changelog mentions Bedrock and Vertex
  routing for Claude, letting enterprises consume Anthropic models
  through AWS or GCP instead of api.anthropic.com directly.

**Where it runs.** All agent execution is local to the Mac. Conductor
talks to GitHub and Linear over their public APIs, follows GitHub Actions
status, and integrates with Graphite-style stacks. There is no
cloud-runner, no remote-worker, no central scheduler service. If the Mac
sleeps, all workspaces freeze.

**Dispatcher (v0.63, June 2026).** A newer feature that automates the
loop of "agent finishes work → review → dispatch next task." The public
docs page is currently a 404; from the changelog entry and Cursor
launch context, it appears to be a routing layer that watches workspace
state and chains follow-up work to a free agent so the user doesn't
manually pick up each next task.

**Workflow vocabulary.** Conductor's docs use a few terms repeatedly that
clarify the mental model:
- *"Create a workspace for each shippable unit of work."*  The unit of
  parallelism is a reviewable diff, not an arbitrary subtask.
- *"Each workspace has its own files, branch, app processes, and agent
  context."*  Isolation is explicit and per-workspace.
- *"Contextual actions"* — Conductor surfaces a recommended next action
  (e.g. "Create PR", "Fix checks", "Resolve comments") based on workspace
  state, guiding the user through the workflow without requiring them to
  remember the next step.

## Key features

- **Isolated workspaces** — Every task gets a dedicated git worktree,
  branch, working tree, terminal session, environment, and agent context.
  Workspaces are identified by branch name (primary) and a workspace
  directory name (secondary, e.g. `warsaw-v2`).
- **Parallel agents** — Run many Claude Code / Codex / Cursor sessions
  side-by-side, each in its own workspace, without cross-contamination.
  The default workflow is "one workspace per shippable unit of work."
- **Multi-agent same-workspace mode** — Alternatively, multiple agents can
  share a single workspace branch to collaborate on the same task
  (review/fix/test patterns).
- **Diff viewer** — Built-in side-by-side and unified diff modes, file
  navigation, per-commit filtering, inline comments that pin to specific
  lines and feed back to the agent as context, and bi-directional sync
  with GitHub PR review comments (resolving threads in either place
  reflects in the other).
- **Checks** — Aggregated merge-readiness panel showing git status, CI /
  GitHub Actions status, deployments, PR comments, and outstanding todos.
  Conductor can warn or block merge when blockers (failed checks,
  unresolved todos) exist.
- **Agent modes** — Two explicit modes for Claude Code and Codex (Cursor
  doesn't support them): **Plan mode** (agent drafts a plan before
  editing — good for ambiguous, multi-file, or risky work) and **Fast
  mode** (skip planning — good for narrow edits and quick follow-ups).
  Mode is per-session; persistent guidance lives in committed
  `AGENTS.md` / `CLAUDE.md` files.
- **Codex personalities and goals** — Codex-only controls for steering
  tone and intent within a workspace.
- **Steering (v0.50, May 2026)** — A mid-stream nudge mechanism for
  guiding an agent that's gone off-course without restarting the session.
- **Checkpoints** — Snapshot-and-roll-back points within an agent
  session for recovering from bad edits.
- **Spotlight / per-repo Spotlight (v0.55, May 2026)** — Command-palette
  style quick navigation and search across workspaces, scoped per
  repository.
- **Browser preview / HTML previews (v0.55, v0.62)** — In-app preview of
  built artifacts and HTML output without leaving the workspace.
- **Dispatcher (v0.63, June 2026)** — Automated task routing across the
  agent pool; chains follow-up work when an agent frees up.
- **Workspace from issue (v0.66, June 2026)** — Create a workspace
  directly from a GitHub or Linear issue with the issue body pre-loaded as
  task context.
- **GitHub integration** — Open PRs from inside Conductor (⌘⇧P), follow
  Actions / status checks for the branch, draft PR descriptions, fix
  failing checks, sync review threads.
- **Linear integration** — Create workspaces from Linear issues; surface
  Linear state in the workspace context.
- **Graphite support** — Compatibility with Graphite-style stacked
  branches and PR stacks.
- **Custom model providers** — Route Claude through Amazon Bedrock or
  Google Vertex; configure custom OpenAI-compatible endpoints.
- **History pane / archives** — Finished workspaces archive out of the
  sidebar but remain restorable with full chat history intact.
- **Repository settings (v0.62, June 2026)** — Per-repo configuration of
  setup commands, environment variables, agent defaults.
- **Sync agent configurations (v0.57, May 2026)** — Share agent
  configuration (CLAUDE.md, AGENTS.md, settings) across workspaces of the
  same repo.
- **Sound and color customization (v0.52, May 2026)** — Cosmetic
  preferences including audio notifications and color theming.

## Pricing

**The app itself is free.** Per the landing page and independent
coverage: "The app is free — you bring your own Claude or Codex
subscription. Logged into Claude Pro / Max? Conductor uses it." Sign in
to the underlying CLI tool (Claude Code, Codex, Cursor) once on the Mac
and Conductor uses that auth — there is no separate Conductor account
required for individual use.

**Bring-your-own-subscription model.** Users pay Anthropic, OpenAI, and/or
Cursor directly for the underlying agent CLIs. Conductor is the wrapper;
it does not add a per-seat or per-task fee for individual use. The
independent madewithlove.com review (March 2026) flagged that running
four agents in parallel multiplies the user's token spend ~4×, "with no
built-in usage controls" — i.e. cost discipline is the user's
responsibility, tracked through Anthropic / OpenAI billing dashboards,
not within Conductor itself.

**Anthropic billing turbulence in mid-2026.** The Conductor blog post
*"Claude subscription update for Conductor"* (June 15, 2026) notes that
Anthropic deferred changes to how third-party integrations consume Claude
subscriptions, letting Conductor users continue using their existing
Claude Pro / Max subscriptions without new constraints. This is worth
noting because the BYO-subscription pricing model depends on Anthropic
(and OpenAI) continuing to allow third-party CLI wrappers to consume
subscription quotas.

**Enterprise tier exists, pricing not public.** The `/enterprise` page
exists but states only that Conductor is "the interface to manage your
coding agents" and invites prospects to "Reach out to bring Conductor to
your organization." There is a "Privacy and security" subsection on the
enterprise page, but the public excerpt does not detail SSO, SOC 2,
on-prem options, or pricing. The model is direct sales for organizations
above an unspecified threshold.

**No published `/pricing` page** as of June 2026 — the URL returns 404.
This is consistent with the "free app + enterprise contact sales" posture
common for AI-tooling companies in this segment.

**OSS status.** Closed source. (The team's prior product, **Melty**, was
open-source; Conductor is not. The team did open-source Melty before
pivoting, so they understand the OSS posture — Conductor's closed-source
choice is deliberate.)

**Funding context.** Conductor (Melty Labs) raised a $22M Series A from
Spark Capital and Matrix Partners, announced March 30, 2026. The team is
6 people in San Francisco, hiring across product engineering, design
engineering, product design, and backend at $175K–$300K bands and open
to new graduates. This implies the free-app strategy is venture-funded
distribution, with enterprise contracts expected to be the revenue engine
over time.

**Acquisition activity.** In April 2026 Conductor announced that **cmd**,
another developer tool, joined the Conductor team — a soft acqui-hire or
absorption rather than a publicly priced acquisition. Suggests Conductor
is consolidating talent and adjacent product surfaces.

## Strengths

1. **Polished native Mac UX.** Independent reviewers consistently single
   out the app's design quality — keyboard-driven (⌘N for new workspace,
   ⌘⇧D for diff, ⌘⇧P for PR), tight sidebar workspace switching, in-app
   diff and review, sound and color customization (v0.52). It's a real
   desktop app, not an Electron port of a web dashboard. The
   madewithlove review called it "polished UX" and noted features like
   "checkpoints for rollback, spotlight testing, multi-model comparison"
   as standouts versus a bare-CLI workflow.

2. **Real workspace isolation that works.** Mapping every task to a git
   worktree on a dedicated branch genuinely prevents the
   stomp-on-each-other problem of running multiple agents in one repo
   checkout. The madewithlove review reported "zero conflicts" across
   four parallel agents fixing bugs in separate files of a React Native
   app, completed in ~10 minutes. Per-workspace `CONDUCTOR_PORT`
   assignment means each workspace can run its own dev server without
   port collisions — concrete proof the team has thought through what
   actually breaks when you run N apps in parallel.

3. **Agent-agnostic.** Supports Claude Code, Codex, and Cursor in one
   surface, with custom provider routing for Bedrock and Vertex. Users
   aren't locked into one vendor's CLI; they can pick the right model per
   task, A/B compare outputs across providers in adjacent workspaces, or
   route enterprise traffic through AWS/GCP for compliance. The "model
   picker" UI makes this a per-session choice, not a global setting.

4. **Tight GitHub / Linear / Graphite integration.** Create workspaces
   from issues (`workspace from issue`, v0.66), open PRs from inside the
   app (⌘⇧P), follow CI/Actions on the branch, sync review comments
   bidirectionally (resolving a comment in Conductor reflects on GitHub
   and vice versa), work with stacked PRs via Graphite. The
   issue→workspace→agent→diff→PR→merge loop is first-class and largely
   keyboard-driven.

5. **Fast release cadence and visible team responsiveness.** ~20
   versions shipped in the last ~2 months (v0.48.2 in April 2026 →
   v0.68.0 mid-June 2026), with steady additions of meaningful
   capabilities (Dispatcher v0.63, Steering v0.50, Cursor support v0.63,
   workspace from issue v0.66, browser preview v0.62, repo settings
   v0.62). The madewithlove review specifically noted Conductor shipped
   "fine-grained GitHub permissions" quickly after security criticism —
   evidence of a responsive feedback loop with users, not just shipping
   features in isolation.

## Limitations / gaps

1. **Mac only.** No Linux or Windows support as of June 2026. Windows is
   on the waitlist roadmap (signup at conductor.build); there is no
   stated Linux plan. This rules out teams whose engineers are on Linux
   laptops or on Windows + WSL, and rules out running on remote build
   servers, EC2 instances, or CI runners. For organizations with mixed
   developer hardware, Conductor's footprint is necessarily partial.

2. **Single-machine orchestration.** All workspaces live on the one Mac
   running Conductor. There is no cross-machine pool of workers, no
   ability to route tasks to a Linux build host, a GPU box, or a shared
   team server. If the Mac sleeps, closes, runs out of RAM, or has its
   power cable yanked, all agents stop. Long-running tasks are bounded
   by the developer's laptop session.

3. **Worktree bootstrap friction for untracked files.** Git worktrees by
   design don't carry untracked files. Each new workspace starts without
   the `.env`, `node_modules`, virtualenvs, build caches, or local
   credential files from the user's main checkout. Reviewers note this
   requires per-repo setup scripts or manual bootstrap before the agent
   can actually run the app — a recurring papercut that compounds when
   the developer creates many short-lived workspaces. Conductor mitigates
   with "Repo settings" (v0.62) for setup-command config and `.env`
   handling, but the friction remains a known gotcha.

4. **No built-in cost controls.** Running N agents in parallel multiplies
   API/token spend N× with no per-workspace budget, no daily cap, no
   alert when a session is burning tokens unproductively. Cost discipline
   is offloaded to the user's Claude / OpenAI billing dashboards. For
   teams or individuals on metered API plans (vs. flat-rate Claude
   Pro/Max), this is a real operational concern.

5. **Workspace isolation is not a security boundary.** Documented
   explicitly: *"Workspace isolation is development isolation, not a
   security boundary. Agents and commands still run on your Mac with
   your user permissions."*  There is no sandboxing of agent shell
   commands beyond what the underlying CLI tool (Claude Code, Codex,
   Cursor) provides. A prompt-injected agent has the same blast radius
   as the user would have: it can read SSH keys, browser cookies, AWS
   credentials, anything on disk that the user can read.

6. **Context loss between sessions.** Agents don't have persistent memory
   across workspaces or sessions. Per the independent review, conventions
   and project context have to be re-supplied via committed
   `CLAUDE.md` / `AGENTS.md` files — no Conductor-level memory or shared
   knowledge layer that one agent's discovery propagates to the next.
   The `.context` folder per workspace is for in-workspace handoffs, not
   cross-workspace memory.

7. **Closed source.** No self-hosted or audit-the-source option;
   enterprises that need code-level review of the orchestration layer
   would have to take Conductor's word for it. The enterprise page has a
   "Privacy and security" section but does not publicly enumerate SOC 2,
   ISO 27001, SSO support, data-residency, or audit logs.

8. **Workspace-level parallelism, not within-task parallelism.** The
   "many agents on one task in one workspace" mode exists, but it's
   coarse — agents share files on the same branch and can step on each
   other if not coordinated. Conductor does not provide a fine-grained
   fan-out/fan-in primitive for "run 5 agents on subtasks of one ticket
   and merge their work." Tools like Microsoft's open-source
   `conductor` CLI (no relation, confusingly same name) target that
   workflow-DAG niche; conductor.build targets workspace orchestration.

## Where Shelbi differs

These are **factual axes of difference**, not claims of superiority. Both
tools solve similar problems and make different design tradeoffs.

1. **Platform surface.** Conductor is a native macOS desktop application
   with a sidebar UI; the user drives it through the app window. Shelbi
   is a command-line tool plus a Kanban TUI, with an Orchestrator agent
   (a Claude Code session) acting as scheduler. There is no native
   desktop GUI.

2. **Worker model.** Conductor creates a new workspace (= new worktree)
   per task and disposes of it on archive — workspaces are ephemeral
   per-task units. Shelbi declares a fixed **pool of named workers**
   ahead of time, each owning a long-lived persistent worktree; tasks are
   assigned to workers, and a worker handles one task at a time before
   being recycled to the next. The pool size is configured, not
   per-task.

3. **Cross-machine orchestration.** Conductor is single-Mac: all agents
   run on the machine where the app is installed. Shelbi supports
   workers on different machines via `prefers_machine` task routing,
   letting an orchestrator route work to whichever host fits (Mac,
   Linux, remote).

4. **Workflow model.** Conductor's workflow is workspace-centric:
   create → agent works → diff → PR → merge → archive, surfaced as
   sidebar items with contextual action suggestions. Shelbi uses an
   explicit **Kanban board** with five columns (backlog, todo, in_progress,
   review, done) and an event log; the user is the priority-setter and
   reviewer, the Orchestrator is the scheduler, and column transitions
   drive auto-dispatch.

5. **Pricing model and source posture.** Conductor is closed-source, the
   app is free, individuals bring their own Claude/Codex/Cursor
   subscription, and enterprise pricing is contact-sales. Shelbi's
   licensing and distribution posture is a separate decision the Shelbi
   site should state on its own terms.

6. **Process / runtime substrate.** Conductor manages agent processes
   inside its own app — workspaces map to in-app sessions whose terminal
   and chat live in the Conductor UI. Shelbi uses **tmux panes** as the
   runtime substrate for each worker; a worker is a long-lived tmux
   pane the user can attach to directly. Closing or restarting the
   Shelbi front-end TUI does not terminate worker panes — they run
   independently under tmux. This is a different design choice with
   different operational properties (tmux is universally scriptable and
   survives detach; a native app surface is more discoverable for users
   who don't already live in tmux).

7. **Handoff signal.** Conductor signals workspace state changes through
   its sidebar UI and contextual-action prompts; the user clicks through
   the workflow. Shelbi uses a **file-based review marker** (a worker
   writes its task id to a known path) plus an **events log**
   (`~/.shelbi/events.log`) that the orchestrator and any tooling can
   tail. The Shelbi handoff is plain-text and scriptable; the Conductor
   handoff is UI-mediated.

## Sources

- https://www.conductor.build/ (read 2026-06-23)
- https://www.conductor.build/docs/ (read 2026-06-23)
- https://www.conductor.build/docs/concepts/workspaces-and-branches (read 2026-06-23)
- https://www.conductor.build/docs/concepts/workflow (read 2026-06-23)
- https://www.conductor.build/docs/concepts/agent-modes (read 2026-06-23)
- https://www.conductor.build/docs/core/parallel-agents (read 2026-06-23)
- https://www.conductor.build/docs/reference/diff-viewer (read 2026-06-23)
- https://www.conductor.build/docs/reference/checks (read 2026-06-23)
- https://www.conductor.build/changelog (read 2026-06-23)
- https://www.conductor.build/enterprise (read 2026-06-23)
- https://www.conductor.build/blog (read 2026-06-23)
- https://www.ycombinator.com/companies/conductor (read 2026-06-23)
- https://madewithlove.com/blog/conductor-running-multiple-ai-coding-agents-in-parallel/ (read 2026-06-23, published 2026-03-24, independent review)