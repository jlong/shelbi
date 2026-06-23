# OpenHands (formerly OpenDevin)

> Competitive research for the shelbi.dev `/vs/openhands` page.
> Factual and dated — no marketing fluff, no Shelbi advocacy.
> Compiled 2026-06-23. Every claim traces to a dated URL in **Sources**.

---

## Positioning

### In their own words

OpenHands' homepage tagline is **"The Open Platform for Cloud Coding
Agents,"** under the headline **"Turn coding agents into org-wide
automation."** The marketing claim front-and-center is that OpenHands can
**"autonomously fix 87% of bug tickets same-day."** The framing draws a sharp
line against autocomplete/copilot tools: OpenHands says it does not help you
"write code faster" but instead "helps you ship changes end-to-end" — agents
that "plan, write, and apply changes across your codebase," "take actions
across entire codebases," "run tasks in parallel," and "execute changes in
real environments."

The GitHub one-liner and the README have, over 2025-2026, shifted toward a
control-plane framing: **"The self-hosted developer control center for coding
agents and automations,"** able to "run OpenHands, Claude Code, Codex, Gemini,
or any ACP-compatible agent across local, remote, and cloud backends." The
mid-2026 **Agent Canvas** launch reframes the GUI as a workspace for turning
"coding agents [into] org-wide automation" wired into Slack, GitHub, and
Linear.

### Restated in plain prose

OpenHands is an **open-source (MIT) autonomous software-engineering agent**
plus a **commercial cloud/enterprise platform** built around it by the company
**All Hands AI**. The open-source core is a single coding agent — the CodeAct
agent — that runs inside a sandboxed runtime where it writes and executes
code, runs shell commands, browses the web, and edits files, observing the
result of each action and iterating until the task is done. You hand it a
task (a bug ticket, a migration, a feature) and it works autonomously and
opens a reviewable pull request.

Layered on that core, All Hands AI sells a hosted SaaS (**OpenHands Cloud**)
and a self-hostable enterprise/automation product (**Agent Canvas** /
**OpenHands Enterprise**) that turns the agent into scheduled, event-triggered
automation across an organization's tools (Slack, GitHub, Jira, Linear,
Notion, HubSpot). Across 2025-2026 the project deliberately broadened from "an
open-source Devin clone" into a **vendor-neutral control plane** that can also
orchestrate competing agents (Claude Code, OpenAI Codex, Gemini CLI) through
the Agent Client Protocol. In short: a single capable agent at the core, an
open ecosystem and automation platform at the edges.

---

## Model

### Lineage and the OpenDevin rename

The project was created **March 12, 2024** as **OpenDevin**, an explicit
open-source homage to Cognition's Devin (which had been announced days
earlier). Through 2024 it grew rapidly as a community project. In **late
2024** it was **renamed to OpenHands** when three of its most active
contributors — **Robert Brennan, Xingyao Wang, and Graham Neubig** (Neubig is
a CMU professor) — founded **All Hands AI** to steward and commercialize the
project. The company has since raised an **~$18.8M Series A**. Naming
artifacts of the rename persist across the web ("OpenHands, f.k.a. OpenDevin";
mirror repos still titled "OpenHands-FKA-OpenDevin").

- **Research paper:** _"OpenHands: An Open Platform for AI Software Developers
  as Generalist Agents"_ (arXiv:2407.16741).
- **Repo:** `github.com/All-Hands-AI/OpenHands`, **MIT-licensed**, ~**70–78K
  GitHub stars** as of June 2026, ~103 releases logged, **v1.8.0** noted on
  2026-06-10.

### Agent style — CodeAct

The flagship agent is the **CodeActAgent**, built on the **CodeAct** paradigm.
Rather than selecting from a fixed menu of structured JSON tool calls, the
agent's unified action space is **executable code**: it **writes and runs
Python** (and shell commands) as its primary way to act on the world. The
research rationale is twofold:

- **Expressiveness** — arbitrary code can compose logic, loops, and library
  calls that a fixed tool schema cannot.
- **Token efficiency** — the CodeAct work reports roughly **55–87% fewer input
  tokens** and **41–70% fewer output tokens** on some agent benchmarks versus
  JSON-tool agents, with accuracy gains on GAIA and HotpotQA.

The control loop is **action → observation → action**: the agent emits an
action (run code, run bash, edit a file, browse a URL), the runtime returns an
observation (stdout/stderr, file contents, page text), and the agent
conditions its next action on that result.

### Event-sourced state

Every action and observation is recorded as an **immutable event** in an
**event stream**. This event-sourcing design provides deterministic replay,
fault recovery, complete audit/debug history, and a clean substrate for
managing conversation state — and it is what lets the platform reconstruct or
resume a run.

### Single-agent core, with sub-agent delegation

OpenHands is "explicitly focused on software development" rather than general
multi-agent orchestration. The primary execution model is a **single CodeAct
agent**. On top of that it supports **microagents** — independent
conversations that inherit the parent's configuration and workspace — used for
task decomposition and limited delegation/parallelism. Microagents come in
two main flavors:

- **Knowledge microagents** — keyword-triggered snippets that inject
  domain/framework context into the agent when relevant terms appear.
- **Repo microagents** — repository-specific instructions stored under
  `.openhands/` that customize how the agent behaves in a given codebase.

Recent product work pushes further toward concurrency and self-checking:

- **Parallel agents** — "orchestrate multiple agents to safely work in
  parallel," plus a **Large Codebase SDK** for dependency mapping across large
  systems (an Enterprise feature).
- **Verification Stack** (announced 2026-06-22) — layered automated verifiers
  and **critic models** so agents "fail fast" and catch their own mistakes
  before producing output.

### Where it runs (runtime / sandbox)

The agent executes inside an isolated **runtime sandbox**. Supported sandbox
backends include:

- **Docker** (default isolated runtime).
- **Apptainer** (HPC/Singularity-style containers).
- **Process / Local** runtime (unsandboxed, for running directly on a laptop).
- **API-based / remote** runtimes (cloud-hosted execution).

Inside the sandbox the agent can use a browser-based **VS Code IDE**, a **VNC
desktop**, and a persistent **Chromium** browser for full-stack/web tasks.
**MCP (Model Context Protocol)** is supported for adding external tools. The
2026 SDK/Agent-Canvas architecture splits responsibilities into:

- an **Agent Server** — a REST API that can host multiple agents on one
  machine, with clients able to connect to several servers at once; and
- an optional **Automation Server** — for scheduling and event-triggered
  workflows.

### Supported model providers (model-agnostic, via LiteLLM)

OpenHands is explicitly model-agnostic; routing goes through **LiteLLM**, so
in principle any LiteLLM-supported provider works. Providers named in the
docs include:

- **Anthropic / Claude**
- **OpenAI**
- **Google Gemini & Vertex AI**
- **Azure OpenAI**
- **AWS Bedrock**
- **Groq**
- **OpenRouter**
- **Moonshot AI**
- **MiniMax** (powers the cloud free option; a May 2026 partnership uses
  MiniMax M2.x)
- **Local LLMs** (Ollama / LM Studio)
- the **OpenHands LLM Provider** — their at-cost hosted gateway
- a **LiteLLM Proxy**, and LiteLLM-supported providers generally

As of **May 2026** the product added **LLM profiles** and **in-conversation
model switching** (choose/swap the model mid-chat, and SDK routing primitives
to send different work to different models).

### Benchmarks (SWE-bench Verified)

SWE-bench Verified is the 500-task, human-validated subset of SWE-bench and is
the standard yardstick for coding agents. OpenHands' score is heavily a
function of the **base model** and the **evaluation protocol** (pass@1 vs
pass@k). Reported figures, by date/source:

- **~53.0%** — CodeAct **v2.1** + Claude 3.5 Sonnet (as of 2025-04-16).
- **~72%** — CodeAct + Claude (independent comparison, mid-2026).
- **~77%** — with Claude Sonnet 4.5; **~77.6%** resolved with Claude Opus 4.5
  under pass@3.
- **68.4%** — CodeAct **v3** on a Claude Opus 4.6 base (single-run figure).
- **56.44%** — SWE-Dev hard split (feature-driven development tasks).

The takeaway: OpenHands is consistently among the **strongest open agents** on
SWE-bench Verified and competitive with proprietary peers, but the headline
number depends on which frontier model you point it at.

### Security and human-in-the-loop

The docs carry a dedicated **security / action-confirmation** surface. Because
the agent runs real code and shell commands, OpenHands provides:

- **Sandbox isolation** as the first line of defense — by default the agent
  acts inside a Docker container, not on the host, so destructive commands are
  contained.
- **Action confirmation mode** — an optional human-in-the-loop gate that pauses
  the agent before it executes potentially risky actions, requiring approval.
- **Security analysis** — integrated checks over the agent's proposed actions
  (e.g., flagging risky operations) layered into the runtime.
- **Workspace scoping** — the agent's filesystem access is bounded to the
  mounted workspace/projects path rather than the whole machine.

These controls matter because the autonomous, code-executing model is inherently
higher-risk than a suggestion-only copilot; they are also why the unsandboxed
"Process/Local" runtime carries an explicit caution in the docs.

### Interfaces / run modes

- **CLI** — interactive terminal client.
- **Web GUI** — browser app, recently rebranded **Agent Canvas**.
- **Headless mode** — "Run OpenHands without UI for scripting, automation, and
  CI/CD pipelines."
- **GitHub Action** — invoke the agent from CI, on issues and PRs.
- **IDE integrations** — VS Code, JetBrains, Zed.
- **Python SDK** — the "OpenHands Software Agent SDK" for embedding/building.
- **ACP** — drive third-party agents (Claude Code, Codex, Gemini CLI) through
  the Agent Client Protocol.

### Recent timeline (2025-2026, from the blog)

- **2025-03-17** — _One Year of OpenHands_ retrospective.
- **2026-01-29** — _Introducing the OpenHands Index_.
- **2026-05-20** — May 2026 product update (LLM profile management, security,
  QoL).
- **2026-05-26** — _OpenHands for Customer Success_ (non-developer use case:
  Slack/HubSpot/Notion).
- **2026-05-27** — _Simple, In-conversation Model Choice_ + MiniMax M2.7
  partnership.
- **2026-06-10** — v1.8.0 release.
- **2026-06-16** — _Introducing Agent Canvas: From Coding Agents to Org-wide
  Automation_.
- **2026-06-18** — _Controlling any Coding Agent with the OpenHands Agent
  Canvas and SDK_ (ACP support for Claude Code, Gemini CLI, etc.).
- **2026-06-22** — _The Verification Stack_.

---

## Key features

- **Autonomous end-to-end task execution** — plans, edits code, runs commands,
  runs tests, and opens reviewable pull requests rather than only suggesting
  snippets.
- **CodeAct unified action space** — the agent acts by writing/executing
  Python + bash, observing real output, and iterating in a closed loop.
- **Sandboxed runtime** — Docker / Apptainer / Process / remote backends, with
  an in-sandbox VS Code IDE, VNC desktop, and persistent Chromium browser.
- **Model-agnostic LLM support** — Claude, OpenAI, Gemini/Vertex, Bedrock,
  Azure, Groq, OpenRouter, Moonshot, MiniMax, Ollama/local, via LiteLLM; BYOK.
- **In-conversation model choice + LLM profiles** (2026) — swap models
  mid-task and route different work to different models.
- **Microagents** — knowledge microagents (keyword-triggered context) and repo
  microagents (`.openhands/` repo instructions) for customization/delegation.
- **Parallel agents + Large Codebase SDK** — orchestrate multiple agents on a
  large codebase with cross-system dependency mapping (Enterprise).
- **Verification Stack** (2026) — layered automated verifiers and critic
  models so agents catch their own mistakes and "fail fast."
- **Event-sourced conversations** — immutable action/observation stream
  enabling replay, recovery, and full audit history.
- **Broad integrations** — GitHub (incl. GitHub Action), GitLab, Jira, Slack,
  Linear, Notion, HubSpot.
- **Multiple run modes** — interactive web GUI (Agent Canvas), CLI, headless
  for CI/CD, IDE plugins, and a Python SDK.
- **Agent Client Protocol (ACP)** — use the same control plane to run Claude
  Code, OpenAI Codex, Gemini CLI, or any ACP agent.
- **Automation Server** — schedule agents and trigger them on events (new
  issue, Slack message) for org-wide automation.
- **MCP support** — extend the agent with Model Context Protocol tools.
- **Strong SWE-bench performance** — competitive-to-SOTA among coding agents
  (see Benchmarks above).

---

## Pricing

OpenHands follows a **freemium / open-core** model: the agent and most run
modes are free MIT-licensed open source; All Hands AI monetizes a hosted cloud
and an enterprise/self-host tier. **You always pay separately for LLM tokens**
(≈$0 with local models; typically ~$6–$200+/mo with cloud APIs depending on
usage).

### 1. Open Source (Local) — Free

- Runs locally on your own machine; **1 user**.
- Includes the "best-in-class" OpenHands Agent, web GUI, Terminal UI, CLI, Git
  integrations, model-agnostic config, and community support.
- No usage caps imposed by All Hands AI; you bring your own model/keys.

### 2. Individual (OpenHands Cloud, SaaS) — Free

- Hosted cloud access from desktop and mobile; **1 user**.
- **Max 10 conversations/day.**
- Adds hosted cloud, API access, and Jira/Slack integrations on top of the
  free tier.
- LLM: **BYOK**, or use "OpenHands models **at-cost** with **no markup**" on a
  pay-as-you-go basis; a free MiniMax-backed option exists within the tier.

### 3. Enterprise — Custom pricing (contact sales)

- Deploy as SaaS **or self-hosted in your own VPC** (no data leaves your
  environment); **unlimited users**; **unlimited daily conversations**.
- Adds SAML/SSO, unlimited concurrent conversations per user, the **Large
  Codebase SDK**, priority support with a shared Slack channel, and a **named
  customer engineer**.
- The enterprise self-host path (Kubernetes, RBAC) requires a **commercial
  license** on top of the MIT core.

### Billing-model notes

There is **no per-seat, per-task, or per-run** list price published. Free
tiers are gated by daily conversations (cloud) or uncapped (local); enterprise
is a custom quote. Net cost to a user is dominated by **LLM inference**, not by
OpenHands licensing.

---

## Strengths

- **Genuinely open source (MIT) with full ownership.** Code, data, and infra
  can stay entirely under the user's control; self-host for privacy-sensitive
  codebases with no vendor lock-in on the agent itself.
- **Top-tier benchmark performance.** Among the strongest open agents on
  SWE-bench Verified — independent reviews cite ~72% with Claude Sonnet 4.5,
  with higher figures reported on newer models / pass@k — competitive with or
  ahead of proprietary peers like Devin.
- **Real execution, not suggestions.** A sandboxed runtime that actually runs
  code, tests, shell, and a browser means the agent verifies its own work and
  can complete tasks end-to-end (PRs, migrations, incident triage).
- **Deep model flexibility.** Truly provider-agnostic via LiteLLM, with BYOK,
  local models, and mid-conversation model switching — tune for cost, privacy,
  or capability per task.
- **Transparency & auditability.** The event-sourced action/observation stream
  gives full visibility into the agent's reasoning and every command it ran,
  plus deterministic replay/recovery.
- **Active, well-funded, research-backed development.** Frequent releases, a
  credible team (Neubig et al.), open standards (MCP, ACP), and a clear
  commercial roadmap.

---

## Limitations / gaps

- **Setup complexity.** Docker / Docker-in-Docker requirements, sandbox
  configuration, API-key setup, and environment tuning "take real effort";
  Docker-socket access can require troubleshooting on restricted/corporate
  systems. Not plug-and-play for users unfamiliar with Docker.
- **Strong-model dependency.** Performance drops sharply with weaker models;
  reviewers say reliable results effectively require Claude 4.5-class or
  GPT-4o-class models, which carries real API cost.
- **Frontend/UI work is weaker.** Reviewers note backend tasks are handled more
  reliably than visual/frontend code generation.
- **Agent failure modes.** Can enter loops retrying the same failing approach
  and need user intervention to break the cycle; the beta Planning Mode
  "occasionally ignores approved plans and improvises."
- **Cost opacity.** Inference bills can "sneak up" during long Claude runs, and
  self-hosting adds hidden operational/maintenance cost. The free cloud tier is
  capped at 10 conversations/day.
- **Support model for OSS users.** The open-source/individual path is
  community-supported (GitHub issues), not an SLA — commercial support and
  enterprise features (SSO, Large Codebase SDK, named engineer) are gated
  behind custom-priced Enterprise.

---

## Where Shelbi differs

_Factual axes of difference, not a value judgment. Shelbi is "an open-source
agent orchestrator for the terminal, built on tmux."_

- **Scope.** OpenHands ships **its own coding agent** (the CodeAct agent) plus
  a runtime, GUI, cloud SaaS, and enterprise automation platform. Shelbi ships
  **no agent of its own** — it is an orchestrator that drives whatever agent
  CLI you already run (Claude Code, Codex, aider, "anything with a CLI").
- **Architecture & dependencies.** OpenHands centers on a **Docker/sandbox
  runtime** (Docker-in-Docker, Agent/Automation Servers, optional Kubernetes
  at the enterprise tier). Shelbi has **no daemons and no servers** — it needs
  only `ssh`, `tmux`, `git`, and your agent CLI on each machine.
- **Where work runs.** OpenHands runs one agent per conversation inside a
  sandbox (cloud-hosted or self-hosted). Shelbi runs **many workers in parallel
  across multiple machines** (laptop + remote boxes over SSH), each in its
  **own git worktree on its own branch**, supervised from a single orchestrator
  and reviewed in a two-pane TUI.
- **State model.** OpenHands stores conversation state as an **event-sourced
  stream** inside the platform. Shelbi keeps **all state as plain markdown/YAML
  files** (tasks, logs, worker status) — grep-able, version-controllable,
  editor-readable — with no database.
- **Commercial model.** OpenHands is open-core: an MIT agent plus a paid hosted
  cloud and a commercially-licensed Enterprise/VPC tier (SSO, named engineer).
  shelbi is a single self-installed binary (`cargo build` / `cargo install`),
  with no hosted SaaS or per-seat tier in scope.

---

## Sources

- https://www.openhands.dev/ (read 2026-06-23)
- https://www.openhands.dev/pricing (read 2026-06-23)
- https://www.openhands.dev/blog (read 2026-06-23)
- https://docs.openhands.dev/ (read 2026-06-23)
- https://docs.openhands.dev/llms.txt (read 2026-06-23)
- https://github.com/All-Hands-AI/OpenHands (read 2026-06-23)
- https://github.com/All-Hands-AI/OpenHands/blob/main/README.md (read 2026-06-23)
- https://arxiv.org/abs/2407.16741 — _OpenHands: An Open Platform for AI Software Developers as Generalist Agents_ (referenced 2026-06-23)
- https://www.openhands.dev/blog/one-year-of-openhands-a-journey-of-open-source-ai-development (dated 2025-03-17, read 2026-06-23)
- https://pablordoricaw.github.io/multi-agent-systems-research/deep-dives/openhands/ — independent architecture deep-dive (read 2026-06-23)
- https://vibecoding.app/blog/openhands-review — independent review, dated 2026-04-01 (read 2026-06-23)
- https://techsy.io/en/blog/openhands-vs-devin-vs-manus — independent comparison, updated 2026-06-13 (read 2026-06-23)