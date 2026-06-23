# Cursor Background Agents (Cloud Agents)

> Research note for the shelbi.dev `/vs/cursor-background-agents` page.
> Scope: the asynchronous, cloud-run agent feature inside the Cursor
> editor — **not** all of Cursor. Originally launched as "Background
> Agents" (Cursor 0.50, May 15 2025); renamed **Cloud Agents** in
> Cursor 2.0 (Oct 29 2025). Both names refer to the same feature line.
> This note uses "Background Agents" since that is the task slug and the
> term most readers still search for; "Cloud Agents" appears where the
> current docs use it. Vendor: **Anysphere** (maker of Cursor).

## Positioning

In Cursor's own words, Background/Cloud Agents are AI agents that "run
in the background" and "run in their own remote environments," letting
you "run many agents in parallel and have them tackle bigger tasks."
The May 2025 launch post framed them as useful for "fixing nits, doing
investigations, and writing first drafts of medium-sized PRs." The
current docs describe them as agents that "operate in isolated cloud
virtual machines rather than locally," with "full development
environments including cloned repos, installed dependencies, secrets,
startup commands, and network access," producing "merge-ready PRs with
artifacts to demo their changes." Cursor 2.0 markets the feature under
a "from developer to delegator" theme — you assign work, the agent runs
asynchronously, and you review the resulting pull request. Throughout,
the user retains live control: "at any point, you can view the status,
send a follow-up, or take over."

In plain prose: a Background Agent is a cloud-hosted copy of Cursor's
coding agent that clones your repository into a Cursor-managed Ubuntu
VM, works on its own branch, can run terminal commands / install
packages / run tests / browse the web, and opens a pull request when
done. You can fire one off from the editor, the web dashboard, Slack, a
GitHub or Bitbucket PR comment, or Linear, then close your laptop while
it works. It is an IDE/cloud-embedded async agent — the same job shelbi
solves, but delivered through Cursor's editor and hosted cloud rather
than a terminal.

## Model

### Agent style

Autonomous, long-horizon, and async-by-default. A Background/Cloud
Agent takes a task, sets up an environment, plans and executes through
a full tool loop (shell, file edits, tests, web), and finishes by
pushing a branch and opening a PR. It is the asynchronous counterpart
to Cursor's interactive in-editor Agent / Composer + Plan Mode, which
keep a human in the loop turn-by-turn. Cloud Agents "always run in Max
Mode" (extended context and full tool use) with **no toggle to disable
it**.

### Single-agent vs multi-agent

Each Background/Cloud Agent is a single agent working its own task in
its own environment, but the feature is built for **parallelism**:

- Run **many agents concurrently**, each on a separate branch/PR.
- Cursor 2.0 added fan-out of **up to 8 agents on a single prompt**,
  each operating on an isolated copy of the codebase so they don't
  conflict (git worktrees locally, or separate cloud VMs/sandboxes),
  with their results compared for review.
- Cursor 2.0 also introduced **Subagents** — in-session delegation
  inside one agent run. Subagents are distinct from cloud Background
  Agents but part of the same multi-agent direction.

### Where it runs

- **Default:** Cursor-managed **Cloud Agents** on Anysphere's infra —
  isolated **Ubuntu VMs** with internet access.
- **My Machines:** the customer supplies the execution machine; "the
  agent loop still runs in Cursor's cloud."
- **Self-Hosted Pool:** a customer-owned pool of execution machines,
  again with the agent loop orchestrated from Cursor's cloud.

So even in the BYO-compute modes, orchestration is cloud-side; this is
not a fully local/offline execution model.

### Supported model providers

Cursor is model-agnostic and routes to frontier models from
**Anthropic (Claude Sonnet / Opus), OpenAI (GPT family), and Google
(Gemini)**, plus Cursor's own purpose-built **Composer** model (shipped
with 2.0; Cursor claims ~4× generation speed vs comparable models and
most interactive turns under 30s in internal benchmarks). **Auto mode**
lets Cursor pick a cost-efficient model and is effectively unlimited on
paid plans; manually selecting a frontier model draws from the plan's
credit pool at API rates.

### Context, memory, and environment

- **Repo clone + branch:** clones from **GitHub, GitLab, Azure DevOps,
  or Bitbucket**, works on a separate branch, pushes a PR. Requires
  "read-write privileges to your repo and any dependent repos or
  submodules."
- **Environment config:** defined via `.cursor/environment.json`
  (agent-led setup), saved **snapshots**, or a **Dockerfile**. Cursor
  stresses that "an agent that can't run tests, query services, or
  reach APIs cannot close the loop on its work," so environment fidelity
  is load-bearing.
- **Secrets:** workspace/team-scoped, managed in the dashboard Secrets
  tab (recommended over committing `.env.local`); snapshots may include
  env files if created with them.
- **Tools:** MCP server support for external tools/APIs; command-based
  hooks from `.cursor/hooks.json`; multi-repo workflows; remote desktop
  and browser control for testing.

## Key features

- Cloud-run async agents in isolated Ubuntu VMs — start a task, close
  the laptop, review the PR later; no local machine or internet needed
  while it runs.
- **Parallel execution** — run many agents at once; Cursor 2.0 fans out
  up to 8 agents on a single prompt across isolated workspaces.
- **Auto PR creation** — agents work on a branch and open a
  "merge-ready" pull request; alternatively check out the branch or
  apply changes locally.
- **Full dev environment** — run terminal commands, install packages,
  run tests, query services, reach APIs; "build, test, and interact
  with the changed software."
- **Environment configuration** via `.cursor/environment.json`, saved
  **snapshots**, or a **Dockerfile**.
- **Secrets management** — workspace/team-scoped secrets via the
  dashboard Secrets tab.
- **Desktop & browser control** — remote desktop control for testing;
  produces **screenshots, videos, and logs** as PR artifacts.
- **MCP server support** — connect external tools/APIs via Model
  Context Protocol.
- **Command-based hooks** from `.cursor/hooks.json` (formatters, audit
  scripts) run in cloud agents; team/enterprise-managed hooks on higher
  plans.
- **Multi-surface launch & control** — start/monitor from Cursor
  Desktop, Cursor Web (cursor.com/agents), **Slack** (`@cursor`),
  **GitHub/Bitbucket** PR/issue comments (`@cursor`), **Linear**
  (`@cursor`), and an **API**.
- **Live take-over** — "view the status, send a follow-up, or take
  over" an agent at any point.
- **Network/access controls** — outbound domain restrictions,
  **Tailscale** connectivity, private source-control support; enterprise
  auto-run / browser / network controls.

## Pricing

### Plan tiers

- **No standalone price.** Background/Cloud Agents are bundled into
  Cursor subscriptions; there is no separate per-agent SKU.
- **Hobby (Free):** "no credit card required"; limited Agent requests
  and Tab completions. Not a daily-driver tier for cloud agents.
- **Pro / Individual ($20/mo, ~$16 annual):** explicitly lists **"Cloud
  agents"** plus extended Agent limits, frontier models,
  MCPs/skills/hooks, and "Bugbot on usage-based billing." Includes a
  ~$20/mo credit pool.
- **Pro+ ($60/mo):** larger included credit pool (~$60/mo).
- **Ultra ($200/mo):** largest individual pool (~$400/mo of usage).
- **Teams ($40/user/mo, ~$32 annual):** "Cloud agents and automations
  with shared team context," centralized billing/admin, team
  marketplace, Bugbot code review, usage analytics, team-wide privacy
  mode, **SAML/OIDC SSO**.
- **Enterprise (custom):** pooled usage, invoice/PO billing, **SCIM**
  seat management, repository/model/MCP access controls,
  auto-run/browser/network controls, audit logs, service accounts, AI
  code tracking API, priority support.

### Billing mechanics

- Since ~June 2025, Cursor uses a **credit-based** model: each paid plan
  includes a dollar pool of model usage.
- **Auto mode** is effectively unlimited and doesn't draw from the pool;
  **manually selected frontier models** draw credits at API rates;
  overages are billed in arrears at API rates "with no penalty markup."
- Cloud Agents run in **Max Mode**, which "consumes credits faster per
  request" (extended context).

### Usage-based requirement

- Background agents historically required **usage-based spending
  enabled**, with reports of a **$10–$20 minimum funding** prompt on
  first use.
- In practice, one independent reviewer found atomic-task agent runs
  "fully covered by the Pro subscription … no additional costs were
  charged" — i.e., real-world cost depends heavily on task size and
  model choice.

### Licensing

Cursor is a **proprietary commercial** product from Anysphere; not open
source.

## Strengths

- **True async, hands-off execution.** Cloud VMs mean work continues
  with the laptop closed; no local environment, internet, or compute
  required while the agent runs.
- **Parallelism at scale.** Many agents at once (up to 8 per prompt in
  2.0) on isolated workspaces — well-suited to fanning out small,
  independent tasks.
- **Tight PR-centric workflow.** "Merge-ready" PRs with branches,
  diffs, and screenshots/videos/logs fit existing code-review habits.
- **Broad launch surfaces.** First-class triggers from editor, web,
  Slack, GitHub/Bitbucket comments, Linear, and an API — low friction to
  delegate from where work already lives.
- **Mature platform integration.** Inherits Cursor's MCP, hooks,
  secrets, multi-repo support, and enterprise controls; backed by a
  large, well-funded vendor that claims 99.9% cloud-agent reliability in
  2.0.

## Limitations / gaps

- **Struggles on large/complex tasks.** Independent reviewers note
  agents do best on "small, atomic units"; "with larger tasks or more
  complex features, the agent might struggle to complete them
  effectively."
- **Forced Max Mode = cost opacity.** Cloud Agents always run in Max
  Mode (no opt-out), which "consumes credits faster," and background
  agents historically gate behind enabling usage-based spending plus a
  funding minimum — harder to predict spend than a flat seat.
- **Broad repo permissions.** Requires **read-write** access to the repo
  and dependent repos/submodules so it can clone and push — a larger
  trust/security surface than read-only or local-only tooling.
- **Cloud orchestration & home-dir gaps.** The agent loop runs on
  Cursor's cloud even in BYO-compute modes; some client-side and
  **user-level hooks** (`~/.cursor/hooks.json`) don't work because
  "cloud VMs don't have access to your local home directory," and
  IDE-specific (Tab/workspace) hooks don't function.
- **Environment setup burden.** Non-trivial repos need careful
  `environment.json` / snapshot / Dockerfile setup before agents are
  productive, since an agent that "can't run tests, query services, or
  reach APIs cannot close the loop."
- **Proprietary & churning.** Closed source; the richest experience is
  inside the Cursor editor, and the feature has churned through rapid
  renames/redesigns (Background → Cloud Agents in under six months).

## Setup & requirements (concrete)

To use Background/Cloud Agents in practice you need:

- A **paid Cursor plan** (Pro/Individual $20/mo and up; "Cloud agents"
  is a listed Pro inclusion). The free Hobby tier is too limited.
- **Usage-based spending enabled**, historically with a **$10–$20
  minimum funding** prompt on first use, since cloud agents bill model
  usage on top of the included pool when frontier models are selected.
- A **connected source-control account** — GitHub, GitLab, Azure
  DevOps, or Bitbucket — with **read-write** access to the repo and any
  dependent repos/submodules so the agent can clone and push.
- A **runnable environment definition** so the agent can build and test:
  `.cursor/environment.json` (agent-led setup), a saved **snapshot**,
  or a **Dockerfile**, plus any **secrets** registered in the dashboard
  Secrets tab.
- Optional but common: **MCP servers** (`.cursor/mcp` config),
  command-based **hooks** (`.cursor/hooks.json`), and integration with
  **Slack / GitHub / Linear** for triggering and notifications.

Cursor's guidance is that environment fidelity is the main predictor of
success — an agent that cannot run tests or reach the services it needs
"cannot close the loop on its work."

## How a run works (end-to-end)

Drawn from the docs and independent hands-on write-ups, a typical
Background/Cloud Agent run looks like this:

1. **Trigger.** You start an agent from a surface that supports it —
   Cursor Desktop (Cloud dropdown), Cursor Web at cursor.com/agents,
   Slack `@cursor`, a GitHub/Bitbucket PR or issue comment `@cursor`,
   Linear `@cursor`, or the API — and give it a task prompt plus a
   target branch/base.
2. **Provision.** Cursor spins up an isolated Ubuntu VM, clones the
   repo (and dependent repos/submodules) using the read-write grant,
   and runs the configured setup: install commands, startup commands,
   and injected secrets, per `.cursor/environment.json` / a snapshot /
   a Dockerfile.
3. **Work loop.** The agent (in Max Mode) plans and executes through a
   full tool loop — editing files, running shell commands, installing
   packages, running tests, querying services, browsing the web, and
   optionally driving a remote desktop/browser for verification.
4. **Observe / steer.** While it runs you can watch status, read its
   reasoning and command output, send a follow-up message, or take
   over the session entirely. Multiple agents can be in flight at once.
5. **Hand off.** On completion the agent pushes its branch and opens a
   **pull request** with a change summary and artifacts (screenshots,
   videos, logs). You can also check the branch out locally or apply
   the changes to your working tree instead of taking the PR.
6. **Review & merge.** You review the PR like any human-authored one;
   Bugbot (on paid plans) can add an automated code-review pass.

The design optimizes for "delegate a small, well-scoped task → get a
reviewable PR back," not for keeping a developer in a tight
edit-compile loop.

## Background Agents vs in-editor Agent vs Subagents

Cursor ships several agent surfaces; only the first is the subject of
this note. Distinguishing them matters for an accurate `/vs` page:

- **Background / Cloud Agents** — *async, cloud.* Run in remote Ubuntu
  VMs, always Max Mode, produce PRs, controllable from many surfaces.
  This is the asynchronous "delegate and walk away" surface.
- **In-editor Agent / Composer + Plan Mode** — *interactive, local.*
  The synchronous, human-in-the-loop coding agent inside the editor;
  Plan Mode drafts a plan before editing. Lower latency, tighter
  control, but tied to your open session and machine.
- **Subagents** — *in-session delegation.* Introduced in Cursor 2.0; a
  primary agent spins up helper subagents within a single run to
  parallelize sub-tasks. Distinct from cloud Background Agents (which
  are separate VM-isolated runs), though both express the same
  multi-agent direction.

## Composer model & benchmarks

Cursor 2.0 shipped **Composer**, a purpose-built coding model used by
the agent surfaces. Cursor's own (internal-benchmark) claims:

- ~**4× generation speed** versus similarly capable models.
- Most **interactive turns complete in under 30 seconds**.
- **99.9% reliability** for cloud agents in 2.0.

These are vendor figures, not independently audited, and Composer is
one option among the Anthropic / OpenAI / Google frontier models the
agents can route to. For long-horizon Background Agent tasks, model
choice (Composer vs a frontier model in Max Mode) trades speed/cost
against capability, and directly affects credit consumption.

## Security & compliance posture

- **Isolation:** each agent runs in an isolated Ubuntu cloud VM; Cursor
  2.0 added running agent commands "in the secure sandbox by default on
  macOS," and uses **git worktrees** to isolate parallel local agents.
- **Secrets:** workspace/team-scoped, stored via the dashboard rather
  than committed; not exposed across teams by default.
- **Network controls:** outbound **domain restrictions**, **Tailscale**
  connectivity, and private source-control support; enterprise plans add
  auto-run, browser, and network controls.
- **Identity & governance (Teams/Enterprise):** **SAML/OIDC SSO**,
  **SCIM** seat management, repository/model/MCP access controls, audit
  logs, service accounts, team-wide **privacy mode**, and an AI
  code-tracking API.
- **Access scope caveat:** the read-write repo grant (including
  dependent repos/submodules) is the main security trade-off to weigh
  against the convenience of auto-PRs.

## Product & company timeline (dated)

- **2025-05-15** — Background Agent ships in **early preview** with
  Cursor **0.50**; "agents run in their own remote environments,"
  parallel execution, Settings → Beta toggle.
- **~2025-06** — Cursor moves to **credit-based** unified pricing.
- **2025-07-31** — Independent hands-on (madewithlove) documents
  real-world strengths/limits and the $10 minimum-spend gate.
- **2025-08-21** — **Linear** `@cursor` background-agent integration
  announced.
- **2025-10-29** — **Cursor 2.0** + **Composer** model; Background
  Agents **renamed Cloud Agents**; multi-agent fan-out (up to 8),
  secure-sandbox-by-default on macOS, faster cloud-agent startup, and a
  99.9% reliability claim.

## Where shelbi differs

These are factual axes of difference, not claims of superiority.

- **Surface.** shelbi is terminal-native; Cursor's Background/Cloud
  Agents are embedded in the Cursor editor and Cursor's web/cloud
  surfaces.
- **Where it runs.** Cursor Background Agents default to
  **Cursor-managed cloud VMs** (with read-write
  GitHub/GitLab/Bitbucket/Azure access), and even BYO-compute modes keep
  the agent loop in Cursor's cloud; shelbi's execution model is not that
  hosted-cloud-by-default model.
- **Licensing.** Cursor is a **proprietary commercial** product from
  Anysphere; the comparison page should state shelbi's own licensing
  posture rather than imply parity.
- **Billing shape.** Cursor charges a per-seat subscription plus
  **usage-based, credit-pool** model billing (forced Max Mode for cloud
  agents); shelbi's pricing/billing model is its own and should be
  stated directly.
- **Editor coupling.** Cursor's agents are richest inside its IDE and
  ecosystem (Composer model, Subagents, Bugbot, MCP/hooks); shelbi is
  not coupled to a single proprietary editor.

## Open questions to verify before publishing

Gaps in the public material that the `/vs` page author may want to
nail down with a live account or fresh docs check:

- **Exact current minimum-spend gate** for cloud agents — the $10–$20
  figure comes from mid-2025 reports and may have changed.
- **Per-run compute cost** — sources disagree on whether cloud-agent
  compute is billed separately from model tokens; current docs frame it
  as model-usage credits in Max Mode, with no explicit per-minute
  compute line item, but this is worth confirming.
- **Concurrency caps** — how many cloud agents a given plan can run in
  parallel (the "up to 8 per prompt" figure is the 2.0 fan-out, not
  necessarily an account-wide concurrency limit).
- **Data-retention / training posture** for code processed in cloud
  VMs under privacy mode vs default — not fully covered in the pages
  read here.
- **GA vs preview status** of My Machines / Self-Hosted Pool execution
  modes.

## Sources

- https://cursor.com/docs/background-agent (read 2026-06-23) — current Cloud Agents docs
- https://docs.cursor.com/background-agent (redirects to cursor.com/docs) (read 2026-06-23)
- https://cursor.com/changelog/0-50 (read 2026-06-23) — Background Agent launch, dated May 15 2025
- https://cursor.com/changelog/2-0 (read 2026-06-23) — Cursor 2.0 / multi-agent / Cloud Agents rename, dated Oct 29 2025
- https://cursor.com/pricing (read 2026-06-23) — plan tiers and "Cloud agents" inclusion
- https://www.cloudzero.com/blog/cursor-ai-pricing/ (published 2026-05-18, read 2026-06-23) — credit system & tier breakdown
- https://madewithlove.com/blog/using-cursor-background-agents/ (published 2025-07-31, read 2026-06-23) — independent hands-on, costs & limitations
- https://www.cometapi.com/cursor-2-0-what-changed-and-why-it-matters/ (read 2026-06-23) — independent Cursor 2.0 / multi-agent analysis
- https://linear.app/changelog/2025-08-21-cursor-agent (dated 2025-08-21, read 2026-06-23) — Linear `@cursor` agent integration