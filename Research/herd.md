# Herd

## Positioning

Herd's own hero copy from joinherd.ai reads:

> **The multi-agent coding tool.**
>
> A lightweight agent IDE for vibe coding at scale. Orchestrate dozens of AI coding agents from one desktop app, monitor every task in real time, and ship in parallel.

The footer and Open Graph metadata add a second tagline:

> **Orchestrate AI. Ship faster.**

The landing page breaks the pitch into three feature blocks:

1. "Agent IDE, lightweight and unopinionated" — "No config to tune, no setup to fight. Claude Code, Codex, and all your agents run the way their creators built them. Launch them in parallel across files, projects, or test suites. They handle the messy parts too, like merges and cleanup."
2. "Real-time monitoring" — "A unified dashboard shows every agent's progress — completions, errors, and status changes as they happen."
3. "Vibe code at scale" — "Run refactors, tests, and feature builds in parallel. No more waiting for one agent to finish before starting the next prompt."
4. "Works with your agents" — "Use Claude Code, Codex, and more to come. Preload skills like gstack to give every agent superpowers out of the box."

In plain prose: Herd is a free downloadable desktop application — Windows installer, macOS app, Linux `.deb`, and Linux `.rpm`, all 64-bit, currently at v0.2.1 — that runs multiple Claude Code, Codex, or Kimi instances on the same machine, each in its own isolated git worktree, and surfaces all of them in one window.

It is the offspring of the "vibe coding at scale" pattern popularised by Garry Tan's GStack. Per the author's own framing on Hacker News (post id 48036669, user `satosheth`):

> A few weeks ago GStack divided HN pretty cleanly. We got a lot of use out of it and quickly discovered we needed a proper multi-agent IDE when running it at scale. The existing options either didn't work well on all platforms or were too bloated with configs and setup. We basically just wanted a tool that let us run multiple agents in parallel and gave us a high-level view of where they were at, while still letting the underlying agent (Claude Code, Codex, etc) do all the heavy lifting.

The product was originally called **Ringmaster** and was renamed to **Herd** in v0.1.7 (2026-03-27); the first public release was v0.1.0 on 2026-03-19. As of the read date the project is roughly three months old.

Herd sits between two adjacent niches in the multi-agent coding tools space:

- **IDE-style multi-agent dashboards** — Conductor (by Melty Labs), Crystal, Vibe Kanban, Nimbalyst, Cursor 3, Windsurf Wave 13. These are GUI applications that orchestrate multiple agents over git worktrees.
- **Terminal-multiplexer-style agent runtimes** — Claude Squad, Herdr at herdr.dev (no relation despite the similar name), tmux-based DIY setups, Gastown, Antfarm. These run inside the terminal and treat agents as PTY sessions.

Herd's chosen pitch is "desktop IDE for parallel agents," not "terminal multiplexer" and not "task-board orchestrator." It is closer to Conductor and Vibe Kanban than to anything terminal-native, and the team's design choices — unified window, sidebar of agents, command palette, dark mode, custom title bar — are explicit homages to VS Code.

A note on naming: the user mentioned "Herdr" with an extra `r`. There are two distinct products in this space and they are easy to confuse.

- **Herd** (this document) lives at `joinherd.ai`. It is the desktop multi-agent IDE described here.
- **Herdr** lives at `herdr.dev`, has a GitHub repo at `github.com/ogulcancelik/herdr`, is written in Rust by a single developer ("Turkish developer Can"), and bills itself as "tmux for agents — an agent multiplexer that lives in your terminal." It is dual-licensed AGPL-3.0 / commercial and is a different product category from Herd.

The remainder of this document refers to **Herd** at `joinherd.ai`, the direct Shelbi competitor.

## Model

### Architecture

Single-process desktop application installed locally on the user's machine.

Distribution channels visible from the landing page: Windows installer, macOS app, Linux `.deb`, and Linux `.rpm`. All builds are 64-bit only. The current published build is **0.2.1**, dated **2026-05-02** in the changelog.

The site is hosted on Cloudflare Pages (the changelog HTML comment notes "extensionless URLs — Cloudflare Pages auto-strips .html"). The contact email is obfuscated via Cloudflare's email protection script. Analytics use GA4 in default-denied consent mode with an explicit cookie banner. Frontend Sentry crash reporting was added in v0.2.1.

The UI is presented as a multi-panel window. From the changelog and landing-page demo:

- **Sidebar** — lists active agents, each with an animal-themed worktree name and an AI-inferred title based on what the agent worked on (v0.2.1).
- **Main pane** — shows the selected agent's chat / output.
- **Right-hand panels** — Source Control (replaced the earlier "Diff" panel in v0.1.8), Files (sits next to Source Control in v0.2.1 — browses the worktree or the workspace repo, fuzzy-filter, drag files into chat), Run.
- **Title bar** — custom VS Code-style title bar with integrated menu, agent summary, and window controls (v0.1.5).
- **Status bar** — workspace summary and file change counts (v0.1.7).
- **Workspace TODOs panel** — surfaces TODOs from `TODOS.md` and lets the user right-click to promote them into a GitHub, GitLab, or Gitea issue (v0.2.1).

Window chrome includes VS Code-style file tabs (single-click previews, double-click pins, drag to rearrange) inside each agent (v0.2.1). The look is dark-by-default with a light mode (light mode added v0.1.10 with platform-aware theme tokens). Zen mode (Ctrl+Shift+Z) hides the title bar and sidebar for distraction-free work (v0.1.5).

The home page renders an animated brand logo composed of letters that dissolve into animal-print shapes — elephant, horse, bison, elk, rhino, camel — implemented in client-side canvas. Each worktree is given an animal name (v0.1.10), reinforcing the herd metaphor in every panel.

### Agent style

Herd does **not** introduce its own LLM agent.

It is explicitly a host for third-party agents. Quoting the landing page: "Claude Code, Codex, and all your agents run the way their creators built them."

A given Herd window spawns one OS process per agent. Each agent has:

- its own conversation thread,
- its own git worktree,
- its own Source Control tab,
- its own Run-panel dev-server config,
- and its own per-agent CLI args (settable in Settings, v0.2.1 — for example, default flags passed to `claude` or `codex` invocations).

From the changelog, the publicly supported "spawn types" include:

- **Claude Code** — since v0.1.0.
- **Codex** — since v0.1.6, with feature parity for spawn, resume, web search, and output analysis.
- **Kimi** — listed as a tile in the landing-page architecture diagram (alongside two "Claude" tiles and two "Codex" tiles in the canonical "5 around the hub" illustration).

The landing page promises "and more to come" but does not enumerate which agent CLIs are next.

### Single-agent vs multi-agent

Each spawned agent is a single-agent CLI session.

Herd's "multi-agent" framing is **multi-instance, not multi-role-within-one-task**: it does not orchestrate a planner / executor / reviewer pipeline inside one prompt. Instead it stands up N independent agents — different prompts, different worktrees, different files — and lets the user fan their attention across them.

The closest thing to internal orchestration is the v0.1.0 primitive:

> Chain agents together — when one finishes, the next starts automatically.

That is a serial-handoff trigger rather than a cooperating-agent pattern. Skill preloading via GStack (mentioned on the landing page) lets every spawned agent inherit the same set of role personas — CEO, Designer, Eng Manager, Release Manager, Doc Engineer, QA — without per-agent setup, but the personas are isolated to their own prompts; they do not message each other through Herd.

### Where it runs

Local-only on the user's workstation.

No cloud control plane, no hosted dashboard, no per-team server. The product downloads as a Windows installer, macOS app, or Linux package. Worktrees are created on the local filesystem; agents call out to their respective LLM providers using their own CLI's credentials.

WSL is explicitly supported for Windows hosts (v0.2.1: "workspaces inside WSL get the right git plumbing on Windows").

There is no mention on the public site of:

- remote attach,
- SSH targets,
- a server / daemon mode,
- shared state across machines,
- multi-user collaboration,
- or an admin / billing console.

Telemetry is opt-in via Google Analytics 4 in default-denied consent mode with a cookie consent banner. Frontend Sentry crash reporting was added in v0.2.1 so silent panics surface to the team.

### Supported model providers

Herd does not select or proxy models itself — each underlying agent CLI brings its own.

In practice this means:

- **Anthropic Claude family** via Claude Code (Pro, Max, or API entitlement).
- **OpenAI GPT-5 family** via Codex (ChatGPT subscription or API key).
- **Moonshot Kimi** — the third tile shown on the landing page.

The website does not advertise OpenRouter, Bedrock, Azure, Google Vertex, or local-model (Ollama / vLLM / llama.cpp) support. The product surface is whatever the underlying agent CLI exposes.

Skill preloading is described only in terms of GStack. There is no documented MCP wiring, no Code Connect equivalent, and no public skill-pack registry beyond GStack.

### Git model

Each agent runs in a dedicated git worktree of the user's repo. Worktrees get animal-themed names (v0.1.10).

Agents can commit, merge, push, and create PRs from their worktrees (v0.1.3, expanded in v0.1.7). The Source Control panel shows a tab-based diff view per agent.

A merge dialog with inline conflict-resolution view shipped in v0.1.11.

AI-generated commit messages from the agent's worktree diff shipped in v0.1.9; AI-generated merge / PR messages came in v0.1.10.

Worktree cleanup-on-kill (no orphaned worktrees) shipped in v0.1.9.

Stranded worktrees from app crashes show up as resumable suspended agents (v0.2.1) rather than being garbage-collected.

### Permissions

Agents are scoped: they can read the whole workspace but only write inside their own worktree (v0.1.3 / v0.1.7).

Safe tools — Read, Grep, Glob, WebSearch — are auto-approved so the agent does not have to ask each time (v0.1.7).

There is no public documentation of a custom allow / deny list, no per-tool granularity beyond the four safe-tool defaults, and no audit log surface.

### Release cadence (from the public changelog)

Twelve releases shipped between **2026-03-19** and **2026-05-02**, roughly weekly:

- **v0.2.1** (2026-05-02) — "Mac, Linux, and a lot more" — Mac and Linux builds; Workspace TODOs → real GitHub / GitLab / Gitea issues; Files panel next to Source Control; VS Code-style file tabs; Run-panel multi-URL chips and Vite v7 banner fix; tray badge and titlebar bell; per-agent CLI args; redesigned command-palette spawn flow; suspended-agent resume for stranded worktrees; WSL support; explicit Install / Dismiss / Restore auto-update controls; AI-inferred agent titles; closing splash; frontend Sentry.
- **v0.1.11** (2026-04-01) — "Terminal & Merge Improvements" — PowerShell support in the shell panel; merge dialog with inline conflict resolution; agent hotkey shortcuts and inline diff stats; consolidated keyboard shortcuts; inline refreshing feedback after commits.
- **v0.1.10** (2026-03-30) — "Light Mode & VS Code Polish" — light mode with theme-aware colors; VS Code-style settings modal; platform-aware shortcut labels; auto-update with crash-loop rollback protection; animal-name worktrees and AI-generated merge / PR messages; tab drag-and-drop between panels; Source Control files grouped Uncommitted / Committed.
- **v0.1.9** (2026-03-29) — "AI Commit Messages & Source Control Polish" — AI commit messages from worktree diff; unmerged changes with committed / uncommitted indicators; horizontal diff scrollbar; leading icons on buttons; worktree cleanup on agent kill.
- **v0.1.8** (2026-03-28) — "Source Control Panel & Website Demo" — new Source Control panel replacing the old Diff panel; right panels auto-collapse with no agent selected; image paste via Ctrl+V into agent conversations; self-contained website demo embed.
- **v0.1.7** (2026-03-27) — "Rebrand to Herd & Git Operations" — renamed from Ringmaster to Herd across UI, config, and docs; agents commit / merge / push / open PRs from worktrees; auto-approve safe tools (Read, Grep, Glob, WebSearch); scoped agent permissions (read-globally, write-worktree-only); Smart Run Panel for per-agent project commands; custom context menus throughout; redesigned status bar.
- **v0.1.6** (2026-03-26) — "Multi-Agent Types & Shell Terminal" — full Codex support on par with Claude Code; shell terminal tab; Shift+Enter for multi-line input; file drag-and-drop and clipboard image paste; session resume via `claude --resume`; workspace context menu (kill all, restart all).
- **v0.1.5** (2026-03-25) — "Custom Title Bar & Menu System" — VS Code-style title bar; full keyboard navigation across menus; zen mode (Ctrl+Shift+Z); responsive layout.
- **v0.1.4** (2026-03-23) — "Streamlined Agent Creation" — one-click "+ Project" and "+ Free" buttons replace the dropdown; consistent sidebar layout at any agent count.
- **v0.1.3** (2026-03-21) — "Git Operations & Agent Permissions" — agents can commit / merge / push / open PRs; auto-approve safe tools; worktree-scoped writes; right-click context menus.
- **v0.1.2** (2026-03-20) — "Interactive Multi-Turn Conversations" — back-and-forth without restarting agents; one-click agent creation (Ctrl+N); recent workspaces remembered; auto-incrementing agent names; hover-to-restart for idle agents.
- **v0.1.1** (2026-03-19) — "Resizable Panel Layout" — drag / resize / rearrange panels; layout presets from the command palette; settings panel for font size, theme, default workspace, shortcuts; status bar; zen mode; Ctrl+1/2/3 panel jumps.
- **v0.1.0** (2026-03-19) — "Initial Release" — spawn and manage multiple AI coding agents from a single window; each agent gets its own git worktree; live unified diff view across workspaces; chain agents together; session persistence; command palette (Ctrl+K); desktop notifications.

The cadence has slowed since 2026-04-01 — only one release in the month leading up to v0.2.1 — but each individual release ships substantial functionality.

## Key features

- **Multi-agent spawning.**
  Spawn many AI coding agents (Claude Code, Codex, Kimi) from a single window.
  Each gets a sidebar entry, an independent chat session, and its own git worktree of the active repo.
  Established in v0.1.0; the spawn flow was redesigned in v0.2.1 to pick the agent type first and the workspace second.
  Agent names auto-increment (Agent 1, Agent 2, ...) so the user does not have to name them on creation (v0.1.2); AI-inferred titles replace the placeholder names once an agent has done meaningful work (v0.2.1).

- **Skill preloading.**
  Preload skill packs — GStack is the cited example — so every spawned agent inherits the same persona prompts and tool conventions without per-agent setup.
  Quoted from the landing page: "Preload skills like gstack to give every agent superpowers out of the box."
  GStack itself is a Garry Tan-published Claude Code prompt-pack that simulates a ~15-person engineering org (CEO, Designer, Eng Manager, Release Manager, Doc Engineer, QA, etc.); Herd's contribution is making that pack available to every agent in the window without per-agent install.

- **Live unified diff dashboard.**
  A Source Control panel (introduced in v0.1.8, expanded in v0.1.9) shows each agent's worktree diff in tabs.
  Files are grouped into Uncommitted and Committed sections (v0.1.10).
  A horizontal scrollbar handles long lines (v0.1.9), and the panel surfaces live refreshing feedback after commits (v0.1.11).
  Inline diff stats appear in the sidebar (v0.1.11) so the user can scan progress across many agents at once.

- **Inline merge / conflict resolution.**
  A merge dialog with an inline conflict-resolution view (v0.1.11) handles the messy parts of pulling parallel branches back together without leaving the app.
  Agents themselves can commit, merge, push, and create PRs from their own worktrees (v0.1.3, expanded v0.1.7), so the user does not have to drop into a shell for routine git operations.

- **AI-generated commit / merge / PR messages.**
  The active agent writes its own commit messages from its worktree diff (v0.1.9) and produces merge or PR descriptions on demand (v0.1.10).
  This matters more in a parallel-agent setting than in single-agent: when three agents finish at once, the user otherwise has to write three commit messages in a row.

- **Per-agent Run panel.**
  Each agent has a Run panel that detects multiple URL chips when the dev server prints more than one (v0.2.1), supports auto-open and a restart button, and is patched for Vite v7's startup banner (v0.2.1).
  Smart Run Panel was introduced in v0.1.7 for launching project commands per agent.

- **Per-agent CLI args.**
  Settings exposes per-agent default flags for the underlying CLI (v0.2.1) — for example, custom `--model` or `--allowed-tools` arguments passed to Claude or Codex on spawn.
  This lets the user run, say, one agent on Claude Opus and another on Claude Sonnet in the same window without separate shell sessions.

- **VS Code-style title bar, tabs, and shortcuts.**
  Custom title bar with integrated menu (v0.1.5).
  Full keyboard navigation across menus — arrows, Home / End, Escape, hover-switch (v0.1.5).
  Zen mode (Ctrl+Shift+Z) hides the title bar and sidebar for distraction-free work (v0.1.5).
  Platform-aware shortcut labels — Ctrl on Windows, Cmd on Mac (v0.1.10).
  Ctrl+1/2/3 to jump to any panel (v0.1.1).
  Tab drag-and-drop between panels (v0.1.10).

- **Workspace TODOs → real issues.**
  Jot a TODO into `TODOS.md`, see it in the Workspace panel, and right-click to promote it to a real GitHub, GitLab, or Gitea issue (v0.2.1).
  This is the closest analogue Herd has to a task board; it remains a single-file scratchpad, not a queue.

- **Agent chaining (serial trigger).**
  "Chain agents together — when one finishes, the next starts automatically" (v0.1.0).
  A primitive serial-handoff trigger.
  Not a dependency graph, not a queue, not a priority order — just "when A is done, fire B."

- **Session persistence and resume.**
  Sessions persist across app restart.
  Claude Code is resumed via `claude --resume` on app launch (v0.1.6).
  v0.2.1 adds AI-inferred agent titles based on what the agent actually worked on, so saved sessions are findable later in the sidebar.
  Recent workspaces are remembered automatically (v0.1.2).

- **Stranded-worktree recovery.**
  When the app crashes mid-run, the leftover worktrees show up as "suspended agents" the user can resume with one click (v0.2.1) instead of being garbage-collected.
  Worktree cleanup-on-kill (v0.1.9) handles the happy-path case so no orphans accumulate.

- **Tray badge and titlebar bell.**
  Notify-without-stealing-focus signals when an agent needs attention (v0.2.1).
  Combined with desktop notifications from v0.1.0 and a per-app close-confirmation splash (v0.2.1).

- **Scoped agent permissions.**
  Agents read globally but write only inside their worktree (v0.1.3, v0.1.7).
  Safe tools — Read, Grep, Glob, WebSearch — are auto-approved so the agent does not have to ask each time (v0.1.7).
  There is no public surface to customise the auto-approve list.

- **Image paste into chat.**
  Paste an image with Ctrl+V directly into an agent conversation (v0.1.8).
  Useful for vision-capable agent CLIs (Claude in particular).
  File drag-and-drop and clipboard image paste are supported for agent terminals as well (v0.1.6).

- **PowerShell support.**
  Shell panel supports PowerShell alongside the standard terminal on Windows (v0.1.11).

- **Auto-update with rollback.**
  Auto-update gained explicit Install / Dismiss / Restore controls and "remembers when you turn it off" (v0.2.1).
  Crash-loop rollback protection added in v0.1.10 so a bad release does not brick the app on next launch.

- **Light mode.**
  Light mode with theme-aware colors across all panels and terminals (v0.1.10) — uncommon in this category; most agent IDEs ship dark-only.

- **Command palette spawn / chain / navigate.**
  Ctrl+K opens the command palette for spawning, chaining, and navigating (v0.1.0).
  The palette is the primary keyboard entrypoint for any non-trivial operation.

## Pricing

There is **no pricing page on joinherd.ai as of 2026-06-23**.

The site's only calls to action are:

- "Download" — Windows installer, macOS app, Linux `.deb`, Linux `.rpm`, all 64-bit.
- "Learn more" — anchor to the "What It Does" section on the same landing page.

No login flow, no sign-up, no subscription tier, no per-seat or per-task billing is advertised. There is no "Pricing" link in the header or footer; the footer lists only Privacy, Terms, Changelog, and a Cloudflare-protected contact email. There is no team plan, enterprise tier, admin console, or self-hosted server SKU.

The implicit cost model is bring-your-own-credentials: the user supplies provider credentials through the underlying agent CLI — a Claude Pro / Max / API entitlement for Claude Code, an OpenAI ChatGPT / API key for Codex, a Moonshot account for Kimi. Herd itself appears to be free to download. The cost of running it is the cost of whatever the underlying agents bill against.

Herd is **not open-source** in any obvious public form. The GitHub repository is not linked from the website, and the v0.1.7 changelog entry refers to "renaming from Ringmaster to Herd … across UI, config, and docs" — internal language consistent with a private repo. There is no source release on a CDN, no plug-in marketplace, no Code Connect or skills registry beyond the GStack reference.

Without a stated commercial model, the product is effectively in the same "free desktop tool while we figure out monetisation" category as Conductor (by Melty Labs), Crystal, and Vibe Kanban — all of which started free, and most of which moved toward a paid tier as they matured.

A reasonable inference (not stated on the site) is that pricing will eventually take one of three shapes:

- **Per-seat subscription** — typical for desktop developer tools that target professional users.
- **Team / enterprise tier** — needed to charge once Herd grows a hosted control plane, team dashboards, or shared skill packs.
- **Marketplace / skill-pack revenue share** — possible if Herd ever opens its skill-preloading surface to third-party authors beyond GStack.

None of this is announced.

## Strengths

- **Fast iteration cadence.**
  The public changelog shows twelve releases between 2026-03-19 and 2026-05-02 — roughly weekly for the first month and a half.
  Substantive features (Codex parity, source-control panel, merge UI, AI commit messages, WSL support, suspended-agent resume) all landed inside that window.
  This is a sign of a small, committed team that ships, not a side project.
  Crash-loop rollback protection (v0.1.10) and frontend Sentry (v0.2.1) suggest the team has been bitten by bad releases and built guard-rails against them, which is a sign of operational maturity for a product this young.

- **Honest "agents run the way their creators built them" stance.**
  Herd's deliberate refusal to wrap, modify, or proxy Claude Code / Codex / Kimi means upgrades to the underlying agent CLIs land for the user immediately.
  Users keep using their own provider accounts, their own settings files, and the underlying agent's native skill / MCP system.
  That avoids the "stuck on whatever version Herd bundled" failure mode common in heavier IDE wrappers.
  It also keeps the trust boundary clean — Herd does not need to see the user's API keys, because the underlying CLI already holds them.
  Per-agent CLI args (v0.2.1) means users can keep their existing flag combinations from the shell.

- **Skill preloading via GStack.**
  Choosing to ride the GStack ecosystem rather than invent yet-another-orchestration-language is pragmatic.
  GStack's role personas (CEO, Designer, Eng Manager, Release Manager, Doc Engineer, QA, etc.) are already documented and used by the broader Garry Tan / Y Combinator crowd.
  Herd inherits that ecosystem for free: every agent the user spawns picks up the same persona prompts without per-agent config, which means consistency across a parallel-agent run without writing or maintaining a custom skill pack.
  The team is plugging into a network effect rather than fighting it.

- **First-class Git plumbing.**
  Per-agent worktrees, scoped write permissions (read globally, write inside the worktree only), AI commit / merge / PR messages, an inline merge-conflict UI, and worktree cleanup on kill add up to a coherent Git story.
  The team clearly treats source control as the spine of the app rather than as an afterthought.
  The fact that stranded worktrees from app crashes are surfaced as resumable suspended agents — rather than being deleted — is a small but unusually thoughtful touch.
  The Source Control panel with grouped Uncommitted / Committed sections and live refreshing feedback is the kind of detail that separates a tool that respects git from one that tolerates it.

- **Linux + WSL support out of the gate.**
  Most macOS-first agent IDEs (Conductor, Crystal) lag on Linux.
  Herd ships Linux `.deb` and `.rpm` packages plus an explicit WSL plumbing fix in v0.2.1.
  That makes it a viable choice on engineering-team workstations where Linux is the default and on Windows machines where the toolchain lives in WSL.
  PowerShell support in the shell panel (v0.1.11) reinforces the same "Windows is a first-class target" stance.

## Limitations / gaps

- **Single-machine, single-user.**
  Herd has no notion of a multi-machine pool, no remote attach, no SSH targeting, no concept of "the team's agents."
  It is a desktop app for one developer's laptop.
  Compared to systems that route work across multiple machines, that is a hard ceiling on how much "ship in parallel" actually scales — the number of agents you can run is the number your laptop can host, not the number you have tasks for.
  There is no documented plan for a server / daemon mode, a shared dashboard, or a team-level pool.

- **No task board, no priority queue, no scheduler.**
  The product surface is a sidebar of agents and a command palette to spawn more.
  There is no backlog → todo → in_progress → review → done columned workflow, no auto-assignment of new work to free agents, no priority ordering, and no built-in concept of "ready" vs "blocked" tasks.
  The closest hint is workspace TODOs promotable to GitHub / GitLab / Gitea issues (v0.2.1), but those leave the system entirely — the moment a TODO becomes an issue, Herd no longer tracks it.
  Coordination across many parallel agents is left to the human's working memory and the unified diff view, which becomes hard to hold past ~5 simultaneous agents.

- **Multi-agent = multi-instance, not multi-role.**
  Agents do not collaborate inside a single task — they fan out across separate tasks.
  There is no planner / executor / verifier loop, no adversarial-verify pattern, no judge panel, no synthesis stage.
  That keeps the model simple, but it means non-trivial work that benefits from internal review still requires the user to set up the verification flow themselves outside of Herd — typically by spawning a second agent and pointing it at the first agent's diff manually.

- **Limited and undocumented model surface.**
  The advertised provider list is Claude Code, Codex, and Kimi.
  There is no mention of OpenRouter, Bedrock, Vertex, Azure, local Ollama / vLLM, OpenCode, Droid, Cursor CLI, Aider, or other terminal agents — the same list Herdr (the unrelated terminal multiplexer) explicitly enumerates.
  New CLI agents have to be added by the Herd team, not by an end-user shim.
  There is no public plug-in / extension surface, no Code Connect, no skill-pack registry beyond GStack.

- **No open-source repository, no documentation site, no public API.**
  The site lists no `/docs`, no API reference, no plug-in surface, no Code Connect equivalent, and no GitHub link.
  The only public artefact is the changelog.
  Power users cannot read the source, file PRs, write their own integrations, or script Herd from another tool.
  By contrast Vibe Kanban, Crystal, Claude Squad, and Herdr (the multiplexer) all have public source.
  The release cadence has also slowed — only one release in April (v0.1.11), and v0.2.1 in early May is the most recent visible release — which raises a small flag about whether weekly-shipping is sustainable.

## Where Shelbi differs

These are factual differences between Herd and Shelbi as architectures, not arguments for one over the other.

- **Worker pool topology vs single-machine IDE.**
  Shelbi's workers are long-lived slots declared in project YAML, optionally spread across machines, each with a persistent worktree and a `prefers_machine` routing hint.
  The pool is fixed and known up-front; when a worker frees up, the orchestrator dispatches the next ready task to that specific worker.
  Herd is a single desktop app on a single machine; the "fleet" lives in one window, and the number of agents is bounded by what one laptop can host.
  There is no remote attach, no cross-machine routing, and no notion of "this task prefers the Mac with the local model loaded."

- **Kanban as the work-intake surface vs spawn-from-palette.**
  Shelbi has a five-column board (backlog → todo → in_progress → review → done) that the user triages and the orchestrator dispatches from.
  Tasks have priority (`shelbi task prio --top|--up|--down|--bottom|--set N`), explicit assignment (`shelbi task assign`), and a backlog-versus-ready distinction that lets the human decide what is ground-truth ready before any work starts.
  Herd's primary intake is "press Ctrl+K, pick an agent type, pick a workspace, type a prompt."
  Backlog grooming, ordering, and ready-vs-blocked are absent in Herd; the user holds those in their head.
  Workspace TODOs in Herd (v0.2.1) are the closest analogue, but they are a single-file scratchpad with a one-way promotion path to external issue trackers, not a managed queue.

- **Orchestrator agent vs human-as-driver.**
  In Shelbi, an LLM orchestrator (Claude) tails the event log (`shelbi events tail --follow`) and auto-dispatches ready tasks to free workers per declared reaction rules.
  The orchestrator is a programmable agent in the loop with its own CLAUDE.md instructions about how to react to events like "task moved to todo," "worker became free," or "pane died."
  In Herd, the human is the orchestrator: launches agents, chains them, watches the dashboard.
  Herd's only piece of automation along this axis is the v0.1.0 chain primitive ("when one finishes, the next starts automatically"), which is a serial trigger, not a scheduler.

- **Review-marker handoff vs same-window inspection.**
  Shelbi workers write a `shelbi-review-ready` file marker to signal "done for human review," and the hub poller moves the card to the `review` column where the user can inspect the branch via `shelbi review <id>` in a dedicated pane.
  The handoff is explicit, file-based, and survives the worker's machine going to sleep.
  Herd surfaces the live diff and chat in the same window the agent runs in.
  "Done" is implicit when the agent's output ends, and there is no separate review staging area — when the user closes the window or quits the app, the in-flight state goes with it (modulo session persistence and stranded-worktree resume).

- **CLI-driven vs GUI-driven.**
  Shelbi exposes its full surface as a CLI (`shelbi task add | list | move | start | assign | prio`, `shelbi worker list | stop`, `shelbi events tail`, `shelbi review <id>`) and is designed to be driven by another agent.
  Any other tool — an LLM, a cron job, a webhook, a shell script — can add tasks, move them, and assign workers.
  Herd exposes its surface only through the desktop GUI; there is no documented CLI, no socket API, no remote-trigger entrypoint, and therefore no way for another agent or scheduler to dispatch work to Herd from outside the window.

## Sources

- https://joinherd.ai/ — Herd landing page; hero copy, taglines, feature blocks, supported agents (Claude Code, Codex, Kimi), download tiles, version 0.2.1 (read 2026-06-23)
- https://joinherd.ai/changelog — Herd changelog page (renders client-side from `assets/changelog.json`) (read 2026-06-23)
- https://joinherd.ai/assets/changelog.json — raw changelog JSON with twelve releases from v0.1.0 (2026-03-19) through v0.2.1 (2026-05-02); source of every dated feature reference in this document (read 2026-06-23)
- https://news.ycombinator.com/item?id=48036669 — "Herd – a lightweight multi-agent IDE, built with GStack" by user `satosheth`, posted ~47 days before read; confirms GStack origin story and Ringmaster → Herd rename (read 2026-06-23)
- https://www.augmentcode.com/learn/garry-tan-gstack-claude-code — Augment Code's writeup of Garry Tan's GStack; cited for the "Conductor + multi-worktree" parallel-agent pattern that Herd directly competes with (does not name Herd) (read 2026-06-23)
- https://nimbalyst.com/blog/best-multi-agent-coding-tools-2026/ — Nimbalyst's 2026 roundup of multi-agent coding tools (Cursor 3, Windsurf Wave 13, Claude Code, Codex app, Conductor, Vibe Kanban, Nimbalyst, Claude Squad, Cline, Gastown, Antfarm); does not yet list Herd, used for the competitive context (read 2026-06-23)
- https://herdr.dev/ — Herdr (terminal multiplexer; a distinct product despite the similar name), referenced for disambiguation (read 2026-06-23)
- https://github.com/garrytan/gstack — GStack source repository; referenced for the GStack skill ecosystem Herd preloads (read 2026-06-23)
- https://docs.gstack.io/ — GStack documentation; cited for the seven-role persona model GStack establishes (read 2026-06-23)
- https://github.com/ogulcancelik/herdr — Herdr (multiplexer) source; cited for disambiguation (read 2026-06-23)
