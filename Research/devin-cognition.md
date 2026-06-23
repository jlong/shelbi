# Devin (Cognition)

> Competitive research for the shelbi.dev `/vs/devin` page.
> Factual and dated — no marketing fluff, no Shelbi advocacy.
> Compiled 2026-06-23. Every claim traces to a dated URL in **Sources**.

***

## Positioning

In Cognition's own words, Devin is **"the first autonomous software
engineer"** — an agent that can "plan, write, test, and ship production code
on its own, working inside your codebase and the tools your team already use."
The landing page headline is simply **"Devin | The AI Software Engineer,"** and
the docs describe it as "the AI software engineer, built to help ambitious
engineering teams crush their backlogs."

Cognition's corporate framing (cognition.com) is that "the purpose of
technology is to expand human capacity — not by replacing meaningful work but
by working alongside people as an exponential collaborator," with the stated
ambition of letting engineers "function more like architects" while Devin
absorbs repetitive implementation. The company positions its work as
"defining the biggest shift in computing since the invention of software."

Restated in plain prose: Devin is a **hosted, commercial autonomous coding
agent**. You hand it a unit of work — a Linear/Jira ticket, a bug report, a
migration, a feature request — and it independently plans the work, boots a
sandboxed cloud VM equipped with a shell, code editor, and browser, writes and
runs the code, debugs its own failures, and opens a pull request for a human to
review and merge. It is sold as a **teammate you delegate to**, not a copilot
you pair with. Since launch the product has grown from a single cloud agent
into a suite: a cloud agent (**Devin Cloud**), a local terminal agent with
subagents (**Devin CLI**), and a full IDE (**Devin Desktop**, the rebranded
Windsurf editor Cognition acquired in 2025) — all backed by Cognition's own
SWE-series coding models plus third-party frontier models.

***

## Model

### Agent style

Devin is an **autonomous, delegation-style** agent rather than an inline
copilot. The core loop is **plan → execute → test → report**, run inside an
isolated cloud virtual machine that hands the agent the same tools a human
engineer uses:

- a **Shell** for running commands, builds, and reading logs;

- a **code editor / embedded IDE** with real-time editing and standard IDE
  shortcuts;

- an **interactive browser** for reading documentation and doing visual QA.

A loose scoping heuristic Cognition publishes in its docs: *"if you can do it
in three hours, Devin can most likely do it"* — i.e. each session targets
roughly a few hours of human-equivalent work. Sessions are first-class
objects: they can be created, listed, messaged, tagged, and terminated via the
REST API (v1/v2/v3).

### Single-agent vs multi-agent

Devin began as a single agent but is now explicitly **multi-agent** on several
axes:

- **Parallel sessions** — each Devin session runs in its own isolated VM, so a
  user can assign many tickets at once. Cognition's example: assigning \~10
  independent tickets simultaneously, each in its own sandbox, executing in
  parallel ("a human engineer might complete 2–3 tickets per day; Devin can
  deliver implementations on 10+ for review").

- **Subagents** — the Devin CLI can "delegate tasks to independent subagents
  that work in the foreground or background," and Devin Local can spawn
  parallel sub-sessions that report back to a main agent, mirroring the
  parallel architecture the cloud product uses.

- **Agent Command Center** — Devin Desktop adds an orchestration surface for
  running and monitoring multiple agents, built on the **Agent Client Protocol
  (ACP)**, so Devin Desktop can manage *any* ACP-compatible agent — a platform
  play rather than a point solution.

### Where it runs

- **Cognition's cloud**, primarily: the web app at `app.devin.ai`, with
  isolated per-session VMs. Linux is the default; **Windows VM support** landed
  May 2026, and **Android emulation** is available for mobile work.

- **Locally**, via the Devin CLI and inside the Devin Desktop IDE on the
  developer's own machine (these clients still talk to Cognition's backend).

- **Enterprise deployments** add **Enterprise Cloud** and **Customer Dedicated
  Deployment** (VPC / private networking) options.

### Supported model providers

Cognition is deliberately model-plural after the Windsurf acquisition:

- **SWE-1.6** — Cognition's own proprietary coding model (released
  **2026-04-07**), offered free on paid plans; part of the SWE-1 / SWE-1.5 /
  SWE-1.6 line.

- **Anthropic Claude**, **OpenAI GPT**, and **Google Gemini** frontier models,
  available on paid plans.

- **Open-source models**, offered free alongside SWE-1.6.

- **Adaptive** — Cognition's "intelligent model router that automatically
  selects the best AI model for each task," so a session isn't pinned to a
  single provider.

### Context, memory, and environment

- **Snapshots / Blueprints** — repo environments are reproducible via
  "Snapshots" defined in YAML "Blueprints," so a session boots into a
  pre-built dev environment instead of cold-installing each run.

- **Knowledge** — a persistent base described as "a collection of instructions
  and advice that Devin can reference in all sessions," editable via API.

- **AGENTS.md** — per-repo instruction files supply project-specific context.

- **Repo indexing** — repositories can be indexed (individually or in bulk) to
  power **Ask Devin** (semantic Q&A) and **DeepWiki** (auto-generated docs).

- **Learning over time** — Cognition states Devin "learns from codebase
  context and past sessions" and improves through exposure to examples; a
  Nubank case study cites task-specific fine-tuning that **doubled completion
  rates** and delivered a **\~4× speedup**.

***

## Key features

- **End-to-end autonomy from a ticket.** Takes a Linear/Jira/Slack task as
  input and produces a reviewed pull request as output — planning, coding,
  running tests, and debugging CI failures along the way.

- **Parallel multi-session execution.** Many sessions at once, each in its own
  sandboxed VM; subagents (foreground/background) for fan-out within a session.

- **Interactive Planning.** Devin drafts a plan/roadmap the user can edit
  before execution begins (introduced with Devin 2.0, April 2025).

- **DeepWiki.** Auto-generated, continuously-updated wiki for any indexed repo:
  architecture diagrams, documentation, and source links; also exposed over an
  **MCP** endpoint and surfaced inside Devin Desktop.

- **Ask Devin / repo search.** Semantic question-answering over indexed
  repositories.

- **Code migrations & modernization.** Large-scale refactors, framework and
  **Java upgrades**, and even **COBOL modernization** are documented,
  first-class use cases.

- **PR review.** Automated review of pull requests with bug identification.

- **Auto-Triage** (2026-05-18). Monitors incoming reports/exceptions,
  investigates with connected tools, correlates related reports, and can open a
  PR automatically — Sentry/Datadog-style incident response.

- **Scheduled sessions & Automations.** Recurring or event-driven "chores" run
  on a schedule without manual prompting.

- **Devin Desktop** (2026-06-02). The former Windsurf IDE, now bundling Devin
  Cloud access, the **Cascade** agent, autocomplete, previews, quick review,
  and the Agent Command Center; supports **JetBrains** and **Zed** via ACP
  plugins. Cascade adds modes, hooks, skills, workflows, and memories/rules.

- **Devin CLI.** Local terminal agent with subagents, skills, rules
  (`AGENTS.md`), MCP support, shell integration, and a `/handoff` command to
  push a local task up to a cloud Devin. ACP support for JetBrains/Zed.

- **Broad integrations.** Source control: GitHub, GitLab, Bitbucket, Azure
  DevOps (plus GitHub Enterprise Server and GitLab Self-Managed). Ticketing /
  chat: Jira, Linear, Slack, Microsoft Teams. Observability / data: Sentry,
  Datadog, Confluence, Notion, AWS, Azure, Snowflake, Databricks, PostgreSQL,
  MongoDB, Stripe, Segment, Airtable, Asana, Google Drive.

- **REST API + SDK.** Programmatic session create/list/message/terminate
  (v1/v2/v3 APIs), playbooks, secrets, attachments, and consumption/metrics
  endpoints (daily consumption, PR metrics, active-user metrics).

- **Visual QA & desktop/browser automation.** Drives a real browser — and a
  Windows desktop — to test applications visually, not just via unit tests.

- **Data Analyst agent.** A specialized agent variant for data-analysis tasks.

- **Enterprise governance.** SAML/OIDC SSO (Okta, Azure/Entra ID), custom
  RBAC, IdP group mapping, IP access lists, audit logs, customer-managed keys,
  a trust center, a FedRAMP-oriented security guide, and dedicated/VPC
  deployment.

***

## Pricing

Cognition has reworked pricing substantially over 2025–2026 (the original
plan was a flat **\~$500/month**, single-tier). The **current** published
self-serve tiers (`devin.ai/pricing`, read 2026-06-23):

| Tier                | Price                        | Headline contents                                                                                                                                                           |
| ------------------- | ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Free**            | $0/mo                        | "Light quota to code with agents"; limited model availability; unlimited inline edits + Tab completions                                                                     |
| **Pro** *(POPULAR)* | $20/mo                       | Higher quotas; OpenAI + Claude + Gemini frontier models; free SWE-1.6 + OSS models; Devin Cloud access; up to **10** concurrent sessions; extra usage at API pricing        |
| **Max** *(NEW)*     | $200/mo                      | Everything in Pro with "significantly higher quotas"; **unlimited** concurrent sessions                                                                                     |
| **Teams**           | $80/mo + $40/mo per dev seat | Unlimited members; sharing/collaboration; centralized billing; admin dashboard + analytics; priority support; unlimited concurrent sessions                                 |
| **Enterprise**      | Custom / quote               | Everything in Teams plus highest-priority support, dedicated account management, SAML/OIDC SSO, centralized admin controls, dedicated + VPC deployment, teamspace isolation |

Independent sources note Enterprise also covers SOC 2 attestation, audit logs,
and optional data residency.

### Usage metering — ACUs

Consumption is metered in **Agent Compute Units (ACUs)** — a normalized unit
that bundles VM time, model inference, and networking, where
**\~1 ACU ≈ 15 minutes of active Devin work**. History of the metering model:

- **April 2025** — pay-as-you-go entry point introduced: $20/month including a
  small ACU allotment, on-demand ACUs at roughly **$2.00–$2.25 each**.

- The legacy **Team** plan was **$500/month** including **\~250 ACUs**
  (≈ $2.00/ACU).

- The current pricing page emphasizes **daily/weekly refreshing quotas** over
  headline per-ACU numbers, with overage "at API pricing." Quota burn varies
  with model, task complexity, and reasoning required.

### Commercial program

- **AI Productivity Guarantee** (2026-06-04): Cognition will fund usage if
  Devin delivers less value than paid for, up to **$10M** in coverage, paired
  with tooling that estimates Devin's output in human-equivalent engineering
  hours.

### Licensing

Devin is a **proprietary, hosted product**. There is **no open-source
edition**; the only local-install surfaces are the (proprietary) Devin CLI and
Devin Desktop clients, which still depend on Cognition's backend.

***

## Strengths

- **Genuine end-to-end autonomy.** Among coding agents, Devin is one of the
  furthest toward "ticket in, reviewed PR out" with minimal babysitting —
  planning, execution, testing, and self-debugging in one loop inside a real
  VM.

- **Parallelism at scale.** The per-session-VM model plus subagents lets one
  human supervise many concurrent tasks — a strong fit for backlog burn-down
  and large mechanical migrations (Java upgrades, COBOL, framework bumps).

- **Deep enterprise integration & governance.** Broad SCM / ticketing /
  observability integrations, SSO, RBAC, audit logs, customer-managed keys,
  and dedicated/VPC deployment make it credible for large, regulated orgs —
  reinforced by the Cognizant partnership and named enterprise deployments.

- **Repo-understanding tooling.** DeepWiki and Ask Devin (indexing-based
  architecture docs + semantic search) are well-regarded for onboarding both
  agents and humans onto unfamiliar codebases.

- **Model flexibility post-Windsurf.** The Adaptive router plus access to
  SWE-1.6 and Anthropic/OpenAI/Google models means it isn't bottlenecked on a
  single provider, and the Windsurf acquisition gave it a real IDE surface
  (Devin Desktop / Cascade) alongside the cloud agent.

***

## Limitations / gaps

- **The "last 30%" problem.** Independent testing repeatedly finds Devin
  delivers partially-complete features that need human finishing — e.g. a dark
  mode task judged "only 70% complete," requiring two rounds of feedback
  before it covered all components.

- **Sensitivity to ambiguity.** Success drops sharply on under-specified work.
  A 2026 review's measured success rates by task type:

  | Task category                  | Success rate |
  | ------------------------------ | ------------ |
  | Ambiguous bug fixes            | \~35%        |
  | Clear/well-specified bug fixes | \~78%        |
  | Small, ambiguous features      | \~25%        |
  | Refactoring                    | \~45%        |
  | New architecture               | \~15%        |

- **Weak architectural judgment.** Reviewers report Devin struggles with
  meaningful refactoring; in one case it refactored an 1,800-line class into
  "arguably worse" code with "unnecessary indirection without improving
  testability."

- **Complex-debugging regressions.** On system-level production issues (e.g.
  memory leaks) it has fixed one real bug while misidentifying another and
  introducing a regression — i.e. it can confidently ship wrong fixes.

- **Security review still required.** Reviewers stress all Devin-generated code
  must be reviewed for security because it "does not reliably identify or
  prevent security vulnerabilities."

- **Cost unpredictability for light use.** Because billing is consumption-based
  (ACUs), low-volume users can see high effective cost-per-task (one analysis
  cited \~$100/task at \~5 tasks/month), making flat-rate copilots cheaper for
  occasional use.

***

## Product & company timeline (dated)

A factual chronology assembled from the changelog/blog and independent
reporting. Dates use the sources' own formatting where given.

- **2024** — Devin launches publicly as "the first AI software engineer"
  (Cognition Labs); initial pricing centers on a \~$500/month team plan.

- **2025-04-03** — Devin gets a pay-as-you-go plan: $20/month + on-demand ACUs
  (\~$2.25/ACU), dropping the entry price from $500.

- **2025-04 (Devin 2.0)** — cloud IDE for running multiple agents in parallel,
  Interactive Planning, and Devin Wiki/DeepWiki auto-documentation.

- **2025-07-14** — Cognition **acquires Windsurf** (IP, product, trademark,
  \~210 employees), reported at \~$250M by third-party outlets.

- **2026-04-07** — **SWE-1.6** released; free on paid plans alongside
  Anthropic/OpenAI/Google frontier models.

- **2026-04** — reports of Cognition in talks to raise at a **\~$25B**
  valuation; ARR reported doubled, Windsurf integrated.

- **2026-04-15** — Windsurf 2.0 ships (Devin in the IDE, Agent Command Center,
  SWE-1.5 at high throughput).

- **2026-05-18** — **Auto-Triage** introduced (monitor → investigate →
  correlate → open PR).

- **2026-05-21** — "Devin is Getting a Windows PC": native Windows VM
  development and testing.

- **2026-05-27** — "More Devins in More Places": **Series D > $1B at a $26B
  valuation**, led by Lux Capital, General Catalyst, and 8VC.

- **2026-05-29** — "Verifying Agentic Development at Scale": end-to-end testing
  inside Devin's VM environment.

- **2026-06-02** — **Devin Desktop** introduced: Windsurf shipped as Devin
  Desktop in an over-the-air update, combining Devin Cloud + Agent Command
  Center + a full IDE; ACP-based, manages any compatible agent.

- **2026-06-04** — **AI Productivity Guarantee** ($10M coverage) and a
  human-equivalent-hours productivity-estimation framework.

- **2026-06-08** — **FrontierCode** benchmark introduced, focused on code
  *quality* rather than mere correctness ("can models actually write good
  code?").

- **2026 (year)** — **Cognizant × Cognition** partnership to scale autonomous
  software engineering across enterprise operations.

Note: domains shifted from `cognition.ai` to `cognition.com` (the blog now
lives at `cognition.com/blog`); `devin.ai` remains the product/landing/pricing
domain and `docs.devin.ai` the documentation domain.

***

## Benchmarks & evaluation

- **SWE-bench** has historically been the headline benchmark for Devin and the
  SWE-1.x models; Cognition publishes completion-rate claims against it for
  successive model releases.

- **FrontierCode** (2026-06-08) is Cognition's own benchmark, explicitly
  measuring code *quality* (maintainability/"good code") rather than pass/fail
  correctness — a reframing of how agent output should be judged.

- Independent reviews caution that benchmark numbers diverge from real-world
  results on ambiguous or architecturally-significant tasks (see Limitations).

***

## When Cognition recommends using Devin

From Devin's docs (`essential-guidelines` / `when-to-use-devin` and the use-case
gallery), the work Cognition steers users toward:

- **Parallelizable, well-scoped tickets** — Linear/Jira tickets that can be
  expressed as a clear unit of work; "if you can do it in three hours, Devin
  can most likely do it."

- **Repetitive engineering toil** — PR review, bug fixes, test writing, and
  scheduled "chores."

- **Migrations & modernization** — framework upgrades, Java version bumps,
  COBOL modernization, large mechanical refactors across many files/repos.

- **Entire features from a clear spec** — greenfield features where the
  requirements are specified up front.

- **App testing & visual QA** — exercising a running app through the browser /
  desktop.

- **Customer-engineering support** — API integrations and prototype
  development.

Cognition's published best-practice guidance leans heavily on **good
instructions** (a "good vs bad instructions" guide, prompt templates, and
`AGENTS.md`/Knowledge onboarding), reflecting the documented sensitivity to
ambiguity noted under Limitations.

***

## Security & compliance posture

Reported / documented controls (per docs.devin.ai enterprise sections and
independent coverage):

- **SSO**: SAML, OIDC, Okta, Azure/Entra ID.

- **Access control**: custom RBAC, IdP group mapping, IP access lists, service
  users, personal access tokens, API key management.

- **Data / key management**: customer-managed keys, audit logs, a public trust
  center, and a FedRAMP-oriented security/admin guide.

- **Compliance**: SOC 2 attestation and optional data residency (per
  third-party reporting of Enterprise terms).

- **Deployment isolation**: Enterprise Cloud, Customer Dedicated Deployment,
  and dedicated SaaS with private networking (VPC).

- **Caveat**: independent reviewers still stress that Devin "does not reliably
  identify or prevent security vulnerabilities" in the code it writes, so
  human security review of generated code remains necessary.

***

## Where Shelbi differs

Factual axes of difference only. The two products target different audiences
and overlap only partly — Devin is a hosted autonomous-agent SaaS; Shelbi is
an open-source, terminal-native orchestrator for agents you already run.

- **Hosting & ownership.** Devin is a proprietary, hosted SaaS that runs work
  in Cognition's cloud VMs (with enterprise VPC options). Shelbi is
  open-source and runs entirely on the user's own machines — a hub plus any
  box reachable over SSH — with "no daemons, no servers," needing only `tmux`,
  `git`, `ssh`, and a CLI agent.

- **Who supplies the intelligence.** Devin ships its own SWE-1.6 model and an
  Adaptive router over Anthropic/OpenAI/Google models — the model is part of
  the product. Shelbi is **agent-agnostic plumbing**: it drives whatever CLI
  agents you already have (Claude Code, Codex, aider, "anything with a CLI")
  and provides no model of its own.

- **Orchestration topology.** Devin's parallelism is per-session cloud VMs and
  in-session subagents managed by Cognition. Shelbi's model is an explicit
  **orchestrator agent** dispatching to a **pool of worker agents** running in
  tmux panes spread across local and remote machines the user owns.

- **Human-in-the-loop review.** Shelbi centers a **two-pane TUI** for watching
  any worker live and reviewing/merging diffs, with per-task **git-worktree
  isolation**. Devin emphasizes autonomous PR generation reviewed in the
  existing SCM (GitHub/GitLab) rather than a dedicated review TUI.

- **State & transparency.** Shelbi keeps all task/log/worker state as plain
  **markdown/YAML files** ("grep it, version-control it, read it from your
  editor"). Devin's state lives in Cognition's hosted platform, surfaced via
  web app, REST APIs, dashboards, and consumption metrics.

- **Commercial model.** Devin is paid and metered (Free → $20 Pro → $200 Max →
  Teams → Enterprise, billed in ACUs). Shelbi is open source; the user's cost
  is their own compute plus whatever the underlying agent CLIs charge.

***

## Sources

- <https://devin.ai/> (landing page — read 2026-06-23)

- <https://devin.ai/pricing> (pricing tiers — read 2026-06-23)

- <https://docs.devin.ai/get-started/devin-intro> (docs: introduction / how it works — read 2026-06-23)

- <https://docs.devin.ai/llms.txt> (full documentation table of contents — read 2026-06-23)

- <https://cognition.com/> (company positioning / mission — read 2026-06-23)

- <https://cognition.com/blog> (release & announcement index — read 2026-06-23)

- <https://www.idlen.io/blog/devin-ai-engineer-review-limits-2026/> (independent review of limitations & success rates — read 2026-06-23)

- <https://venturebeat.com/programming-development/devin-2-0-is-here-cognition-slashes-price-of-ai-software-engineer-to-20-per-month-from-500> (Devin 2.0 + pricing change — read 2026-06-23)

- <https://techcrunch.com/2025/07/14/cognition-maker-of-the-ai-coding-agent-devin-acquires-windsurf/> (Windsurf acquisition — read 2026-06-23)

- <https://techcrunch.com/2025/04/03/devin-the-viral-coding-ai-agent-gets-a-new-pay-as-you-go-plan/> (ACU pay-as-you-go pricing — read 2026-06-23)

- <https://the-agent-report.com/2026/06/cognition-devin-desktop-agent-orchestration/> (Devin Desktop / ACP / Agent Command Center — read 2026-06-23)

- <https://siliconangle.com/2026/04/23/cognition-creator-ai-software-engineer-devin-talks-raise-hundreds-millions-25b-valuation/> (funding / valuation — read 2026-06-23)
