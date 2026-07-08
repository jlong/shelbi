# Vision

Where Shelbi is going and why, one level above positioning. `positioning.md` says what we tell the champion today; this says what we are building toward. Written July 2026. Future claims here trace to `Plans/`; anything that doesn't is marked as direction, not commitment.

## The world we're building for

Developers are moving from running one coding agent to running several at once, and the move is happening ahead of the tooling. The people we build for are already there: multiple agent CLIs in terminal tabs, shipping real work that way every day. What's failing them is not agent capability. Agents are already good enough to take a focused task and finish it. What's failing is the human in the middle. When you run five agents by hand, you become the router and the scheduler: you track who's done, who's stuck, who asked a question two hours ago, and you carry the backlog in your head because interrupting a mid-task agent derails it.

We think this only gets worse. Agent capability keeps rising and agent cost keeps falling, so the natural number of concurrent agents per developer keeps going up. Attention is the resource that doesn't scale with it. The developer's job is shifting toward direction and judgment: writing the plan, scoping the work, deciding what's good enough to merge. The bottleneck is everything between those moments, and that's the layer Shelbi occupies.

## What Shelbi is

Shelbi turns a fleet of coding agents into a system you can trust. Three ideas carry it.

You talk to one agent, the orchestrator. It does not do the work; it writes work up as focused tasks and assigns them to workers. Because it never has its hands in the code, you can dump work on it the moment it occurs to you without derailing anything. It organizes the backlog and makes sure things get done, so the work lives on the board instead of in your head.

Tasks are the unit of work. Every item becomes a scoped task before an agent touches it. Agents do their best work on discrete, focused chunks, and task-level resolution lets big features and one-line fixes flow through the same system.

Workflows are the quality bar. Every task moves through stages you define before it reaches you, and agents you author can govern columns: adversarial review, QA, security, whatever your project needs. The point is that the bar is built into the system rather than depending on your vigilance on a given afternoon. Boundaries are what make autonomy safe.

Underneath, Shelbi runs on things a developer already owns: tmux panes, git worktrees, SSH, markdown and YAML on disk. That is architecture, not expedience. It means there is nothing to host, nothing to trust with your code, and nothing you can't inspect with the tools you already have.

## What Shelbi will become

The trajectory in `Plans/` points in one consistent direction: more of the loop runs without you, and the parts that don't get sharper.

Workflows generalize. The five hardcoded statuses become pluggable workflows with arbitrary stages, shared status definitions, and transition hooks that run tests, lints, and builds as gates. Agents become first-class, reusable roles you write once and wire to any column in any workflow. Review itself is being rebuilt from generic workflow primitives, so a review gate is configuration, not special-cased machinery.

Review gets a real environment. Review workspaces pair a long-running dev server with the task under review, so human review means clicking through the running change, not reading a diff cold.

Trust escalates. The orchestrator gains a live event feed and auto-dispatches work so workers stay busy without you prompting each assignment. With Zen Mode on, it merges work that clears your gates and reports back on what landed and what needs you. The direction, though not a commitment, is that as your gates prove they catch what you would, you spend more of your time with Zen on. It stays an explicit choice you make per session; Shelbi will not silently escalate its own autonomy.

The pool grows. Workers already run on any machine you can SSH into, with zero install on the remote side. Worker-orchestrator communication becomes a real protocol, so workers can ask questions and the orchestrator can answer without fragile terminal plumbing.

Shelbi becomes easy to adopt and share. Homebrew and APT distribution, a public site and docs, and in-repo project config so a team's workflows, agents, and templates live in git and arrive with a clone. Tasks stay personal; process becomes shareable.

## What Shelbi will not be

The competitor research in `Research/` maps the space well: cloud VMs, desktop apps, SaaS control planes, consumption metering. Shelbi deliberately is none of it.

No cloud dependency and no SaaS control plane. Your agents run on your machines, reached over SSH trust you already have. There is no hosted state, no service that sees your code, and no company between you and your work.

No database. State is markdown and YAML in plain files. You can grep it, diff it, edit it, and put it in git. This has held through every plan to date and we treat it as load-bearing.

Terminal-native, permanently. tmux is the runtime, not a compatibility layer. Shelbi is not becoming a desktop app or an IDE.

Agent-agnostic. Shelbi drives agent CLIs; it does not ship a model or bet on a single provider.

Developer-flat, not enterprise-suite. No per-seat pricing, no SSO and RBAC dashboards, no sprawling Jira-Slack-HubSpot integration matrix. Shelbi is a tool one developer installs and trusts, and a team adopts by committing config to a repo.

And we don't compete on table stakes. Parallel agents, worktree isolation, and auto-PRs are claimed by everyone in the space. Shelbi's claim is the shape of the system around them: one orchestrator to talk to, tasks as the unit of work, workflows as the quality bar, on infrastructure you already own.
