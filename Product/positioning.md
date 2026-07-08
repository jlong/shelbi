# Positioning

Worked out July 2026 using a homepage-first positioning method (research notes live in the 32pixels ContextStore space, `Positioning/` folder). Decisions below were made deliberately, one at a time. Treat changes to any single element as a repositioning decision, not a copy edit.

**Positioning statement:**

> For developers already running multiple coding agents in terminal tabs, Shelbi replaces the tab-juggling with a single agent you talk to: an orchestrator that writes work up as focused tasks, dispatches them to worker agents on any of your machines, and delivers the results back through workflows you define.

**Tagline (short form, README / metadata):** One agent to talk to. Workers do the rest.

<br />

## The decisions


- **Primary anchor:** competitive alternative — *the mess of terminal tabs*. The enemy is universally understood by the champion; it is literally their screen right now.

- **Differentiation wedge:** *you talk to one agent that runs the rest.* With tabs, you are the router — multiplexing attention, replying in wrong tabs, forgetting paused sessions. With Shelbi you're no longer the router. Key mechanism backing the claim: **the orchestrator doesn't do the work, it assigns it** — so you can dump work on it as it occurs to you without derailing anything.

- **Secondary anchors (hero-level):** open source · made with tmux · workers across your machines. Not in the hero: "Claude Code" by name (keeps scope open for other agent CLIs; use "coding agents" as the generic noun).

- **Register:** developer-flat. Clear beats clever. No grandeur ("mission control" was considered and rejected), no multi-order benefits ("do more," "greater productivity"), no vision copy.

- **Frame:** "Inbox Zero, for agent work" — used at solution-intro level, not the H1. Get the backlog out of your head; tell the orchestrator; it organizes it and makes sure it gets done.


<br />

## The six messaging elements

| Element                   | Shelbi                                                                                                                                                  |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Use case / context**    | Shipping code changes — large and small — with multiple coding agents working concurrently                                                              |
| **Current tool**          | Terminal tabs, one agent per tab, you as the router and scheduler                                                                                       |
| **Problem**               | Wrong-tab replies; forgotten paused agents; constant tab-cycling; the backlog living in your head                                                       |
| **Capability**            | Talk to one agent — it writes work up as focused tasks, dispatches to workers, tracks everything, and returns finished work for review                  |
| **Features**              | Orchestrator · kanban TUI · worker pool (any machine, tmux) · configurable workflow columns and review gates · specialized review agents · Zen Mode     |
| **Benefit (first-order)** | Work is out of your head and nothing gets dropped; agents do better work on focused tasks; the quality bar is built into the system, not your vigilance |

<br />

## Homepage blueprint

### Hero

> **H1:** Stop babysitting a mess of agent tabs.
>
> **H2:** You talk to one agent, the orchestrator. It writes your work up as tasks, dispatches them to workers, and brings the finished work back for your review.
>
> Badge line: `open source · made with tmux · multi-machine` — CTAs: **Install now** / **Read the docs**

(Meets the Minimum Viable Positioning bar: market element = the tab mess; product elements = orchestrator, dispatch, review.)

### Problem section — five beats

1. **Replied to the wrong tab.** You typed instructions for one agent into another one's session.
2. **The forgotten paused agent.** An agent asked you a question two hours ago. It's still waiting, in a tab you forgot.
3. **You are the scheduler.** Who's done? Who's stuck? Who needs you? You've become a human scheduler for your own tools.
4. **The backlog lives in your head.** New work occurs to you while every agent is mid-task. Interrupting one derails it, so you carry the idea instead.
5. **The review queue is you.** Every new agent makes the pile taller, and you trust it less.

### Solution intro

> **Inbox Zero, for agent work.**
>
> Dump work on the orchestrator the moment it occurs to you. It doesn't do the work itself, so there's nothing to derail. Each item becomes a focused task, and nothing gets dropped. Out of your head. Off your tabs.

### Value props — the triad

1. **Tasks keep work focused.** Every item becomes a scoped task before an agent touches it. Agents do their best work on focused chunks, and big features and quick fixes flow through the same system.
2. **Agents provide specialization.** Workers execute. Reviewers scrutinize: adversarial review, QA, security. Each does one job well, on every task.
3. **Workflows provide boundaries.** Every task moves through stages you define before it reaches you. Boundaries are what make autonomy safe. When your gates catch what you would, flip `shelbi zen on` and the orchestrator merges green work itself. You get a digest of what landed and what needs you.

### Feature grid (supporting facts)

Kanban TUI · workers on any machine (just tmux + an agent CLI) · made with tmux · review flow · plain-file state (markdown & YAML, no database, no cloud) · open source

### Closer

Install CTA (`curl … | sh` code block) + "Build from source" docs link.

<br />

## Handoff notes (for the homepage build)

**This blueprint supersedes the current site hero** ("Manage a team of coding agents from your terminal"). The H1, H2, and badge above are the new hero.

**Copy rules, non-negotiable:**

- Developer-flat register. Clear over clever. No grandeur (no "mission control" or similar).

- No em-dashes anywhere in published copy. Use periods, commas, or colons.

- First-order benefits only. Never "do more," "boost productivity," or other multi-order claims.

- Every claim states a mechanism. If a sentence has no feature behind it, cut it.

**Show, don't claim:**

- The single most convincing moment for the champion is watching the orchestrator turn a lazy one-line request ("the events log rotation thing from yesterday, fix it") into a crisp, scoped task. Get that beat into the hero animation.

- Put the GitHub link with stars within one click of the hero. It is the open source trust signal standing in for customer logos.

**Fact-check status (verified against source, July 2026):** orchestrator-as-interface, multi-machine SSH workers, YAML workflows with per-status agents, pluggable runners (Claude Code, Codex, aider), and the Zen primitives are all shipped. Keep two claims honest in final copy:

- "Adversarial review, QA, security" agents are built with shipped primitives (`shelbi agent new`, per-status `agent:` assignment), not shipped presets. Phrase as what you can add, not what comes in the box.

- Zen's "digest" is the orchestrator reporting back conversationally plus probe and CI output, not a formatted digest feature.

- If copy names workflow stages, use the real defaults: backlog, todo, in-progress, review, done, canceled.

**Don't lead with table stakes:** parallel agents, worktree isolation, and auto-PRs are claimed by every competitor (see `Research/` for the teardowns). Shelbi's rare claims are the orchestrator interface, kanban intake, the multi-machine pool, plain-file state, and the tmux runtime.

**Deliberately excluded from the homepage:** "Claude Code" by name in the hero (scope stays open; say "coding agents"), "Inbox Zero" as the H1 (solution-intro level only), `shelbi zen dry-run` (docs material), and the review-burden reassurance beyond beat 5 plus the triad (don't over-defend).

<br />

## Source narrative (founder's words, kept as raw material)

**Background.** My core motivation with the project was that I was getting tired of managing separate workstreams in terminal tabs each running their own agents. I sometimes found myself sending the wrong thing to an agent just because I picked the wrong Terminal tab to respond in. I also found myself forgetting about tabs where they would be paused for a long time waiting for the answer to a question. Shelbi removes the pain of manually orchestrating the agents and allows me to focus more on giving them the right direction on the plan and task level.

**How I work.** My main method is to first create a markdown plan for a feature. Then when I'm satisfied with the plan I ask the Orchestrator to break it into tasks and distribute them to the worker agents. Where Shelbi shines is in giving the agents discrete chunks of work. Task-level turns out to be an extremely helpful resolution for worker agents. By breaking large plans down into tasks, and creating one-off tasks for smaller work, Shelbi can balance large and small things well.

**On the orchestrator.** The orchestrator is key. So much easier to talk to one agent, rather than managing agents in multiple tabs. Because the orchestrator doesn't do the work, but rather assigns it to others, you can dump work on it as it occurs to you without worrying about getting it distracted. Its job is to write the work up for you in a way that other agents can execute. The agents benefit from ordered, focused work. It's like Inbox Zero. Instead of keeping all of this stuff in your head you just tell it to the orchestrator. It organizes it and makes sure it gets done. A relationship of trust develops. Eventually you turn on Zen Mode so that the orchestrator can blitz through the work.

**On quality.** Shelbi provides the primitives you need to support workflows that produce extremely high quality code: specialized agents for adversarial code review, QA, and security, each governing a column on the board so that every task benefits from the same scrutiny. Work can be delivered at a higher quality bar, not lower. The key is building that bar into the system. Workflows provide boundaries. Agents provide specialization. Tasks keep work focused.
