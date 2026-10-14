# Vision

Where Shelbi is going and why, one level above positioning. `positioning.md` says what we tell the champion today; this says what we are building toward. Written July 2026. Revised October 2026: Shelbi's own sessions replaced tmux, and a native macOS app joined the TUI as a second client. Future claims here trace to `Plans/`; anything that doesn't is marked as direction, not commitment.

## The world we're building for

Developers are moving from running one coding agent to running several at once, and the move is happening ahead of the tooling. The people we build for are already there: multiple agent CLIs in terminal tabs, shipping real work that way every day. What's failing them is not agent capability. Agents are already good enough to take a focused task and finish it. What's failing is the human in the middle. When you run five agents by hand, you become the router and the scheduler: you track who's done, who's stuck, who asked a question two hours ago, and you carry the backlog in your head because interrupting a mid-task agent derails it.

We think this only gets worse. Agent capability keeps rising and agent cost keeps falling, so the natural number of concurrent agents per developer keeps going up. Attention is the resource that doesn't scale with it. The developer's job is shifting toward direction and judgment: writing the plan, scoping the work, deciding what's good enough to merge. The bottleneck is everything between those moments, and that's the layer Shelbi occupies.

## What Shelbi is

Shelbi turns a fleet of coding agents into a system you can trust. Three ideas carry it.

You talk to one agent, the orchestrator. It does not do the work; it writes work up as focused tasks and assigns them to workers. Because it never has its hands in the code, you can dump work on it the moment it occurs to you without derailing anything. It organizes the backlog and makes sure things get done, so the work lives on the board instead of in your head.

Tasks are the unit of work. Every item becomes a scoped task before an agent touches it. Agents do their best work on discrete, focused chunks, and task-level resolution lets big features and one-line fixes flow through the same system.

Workflows are the quality bar. Every task moves through stages you define before it reaches you, and agents you author can govern columns: adversarial review, QA, security, whatever your project needs. The point is that the bar is built into the system rather than depending on your vigilance on a given afternoon. Boundaries are what make autonomy safe.

Underneath, Shelbi is one small backend on machines you already own: a daemon that owns each agent's terminal session, git worktrees for isolation, SSH to reach other machines, and markdown and YAML on disk for state. That is architecture, not expedience. It means there is nothing to host, nothing to trust with your code, and nothing you can't inspect with the tools you already have.

You reach that backend through a client, and there are two: the TUI and a native macOS app. They are siblings, not tiers. Both render the same models, run the same command registry, and attach to the same sessions, so you can leave the TUI running on the hub, open the app on your laptop, and watch the same agent type. One backend, two clients, and no feature that lives in only one of them.

## What has landed since July

The July trajectory pointed at more of the loop running without you. Most of that is now shipped and stays described here because it is the shape of the product, not a roadmap.

Workflows are pluggable: arbitrary statuses in five fixed categories, per-status agents, transition actions that push, open PRs and merge, and a loader that refuses ambiguous automation. Review runs in review workspaces that pair a dev server with the task under review, so human review means clicking through the running change. The orchestrator tails a live event feed and dispatches on its own; with Zen Mode on it reviews and merges work that clears your gates and reports what landed and what needs you. Workers ask the orchestrator questions over a message channel and get answers back without terminal plumbing. Workers run on any machine you can SSH into; the `shelbi` binary on the remote owns their sessions, so an agent survives a dropped connection. Homebrew and APT distribution, the public site and docs, and in-repo project config so a team's workflows, agents and templates arrive with a clone.

## What Shelbi will become

Two clients stay at parity. The desktop app ships for macOS first; a Linux build is planned for its own release (`Plans/desktop-app-gpui.md`, "Later: Linux desktop"). Parity is enforced by tests rather than by promise: every command has a scenario, each client has a smoke test, and a field a client doesn't show fails the build.

Rework resumes a conversation instead of starting cold. A task records the agent session that did its work, and a bounced task comes back to that same session with the reviewer's findings as its next message, on the same machine natively and across machines through a transcript or a hand-off summary (`Plans/resumable-task-sessions.md`; direction, not commitment).

Trust keeps escalating. As your gates prove they catch what you would, you spend more of your time with Zen on. It stays an explicit choice you make per session; Shelbi will not silently escalate its own autonomy.

The orchestrator gets sharper at the parts that still need you: better write-ups, better questions, less of your attention spent on routing and more on direction.

## What Shelbi will not be

The competitor research in `Research/` maps the space well: cloud VMs, SaaS control planes, consumption metering, apps that own your sessions on someone else's machine. Shelbi deliberately is none of it.

No cloud dependency and no SaaS control plane. Your agents run on your machines, reached over SSH trust you already have. There is no hosted state, no service that sees your code, and no company between you and your work.

No database. State is markdown and YAML in plain files. You can grep it, diff it, edit it, and put it in git. This has held through every plan to date and we treat it as load-bearing.

Not an IDE. Shelbi has a desktop client now, but the client is a window onto the same backend the TUI uses. It doesn't edit your code, hold state of its own, or move your sessions anywhere. Your editor stays your editor, and the terminal client stays a first-class way to run everything.

Agent-agnostic. Shelbi drives agent CLIs; it does not ship a model or bet on a single provider.

Developer-flat, not enterprise-suite. No per-seat pricing, no SSO and RBAC dashboards, no sprawling Jira-Slack-HubSpot integration matrix. Shelbi is a tool one developer installs and trusts, and a team adopts by committing config to a repo.

And we don't compete on table stakes. Parallel agents, worktree isolation, auto-PRs, and a desktop app are claimed by everyone in the space. Shelbi's claim is the shape of the system around them: one orchestrator to talk to, tasks as the unit of work, workflows as the quality bar, two clients over one backend, on infrastructure you already own.