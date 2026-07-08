# Positioning


**Positioning statement:**

> For developers already running multiple coding agents in terminal tabs, Shelbi replaces the tab-juggling with a single agent you talk to — an orchestrator that writes work up as focused tasks, dispatches them to worker agents on any of your machines, and delivers the results back through workflows you define.

**Tagline (short form, README / metadata):** One agent to talk to. Workers do the rest.

<br />

## The decisions


- **Primary anchor:** competitive alternative — *the mess of terminal tabs*. The enemy is universally understood by the champion; it is literally their screen right now.

- **Differentiation wedge:** *you talk to one agent that runs the rest.* With tabs, you are the router — multiplexing attention, replying in wrong tabs, forgetting paused sessions. With Shelbi you're no longer the router. Key mechanism backing the claim: **the orchestrator doesn't do the work, it assigns it** — so you can dump work on it as it occurs to you without derailing anything.

- **Secondary anchors (hero-level):** open source · terminal/tmux-native · workers across your machines. Not in the hero: "Claude Code" by name (keeps scope open for other agent CLIs; use "coding agents" as the generic noun).

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
> **H2:** With Shelbi you talk to one agent — an orchestrator that breaks work into tasks, dispatches them to worker agents on any of your machines, and brings finished work back for your review.
>
> Badge line: `open source · tmux-native` — CTAs: **Install now** / **Read the docs**

(Meets the Minimum Viable Positioning bar: market element = the tab mess; product elements = orchestrator, dispatch, review.)

### Problem section — four beats

1. **Replied to the wrong tab.** You typed instructions meant for one agent into another agent's session.
2. **The forgotten paused agent.** An agent asked you a question two hours ago. It's still sitting there, blocked, in a tab you forgot existed.
3. **You are the scheduler.** Cycling through tabs asking "who's done? who's stuck? who needs me?" — you've become a human scheduler for your own tools.
4. **The backlog lives in your head.** New work occurs to you while every agent is mid-task. Interrupting one derails it, so you sit on the idea — and carry it.

### Solution intro

> **Inbox Zero, for agent work.**
>
> Dump work on the orchestrator the moment it occurs to you — it writes each item up as a focused task, dispatches it to worker agents, and makes sure it comes back done. Out of your head. Off your tabs. Through your review.

(Interrupt-safety is the load-bearing claim: because the orchestrator doesn't do the work itself, telling it something new never derails work in flight.)

### Value props — the triad

1. **Tasks keep work focused.** The orchestrator writes every item up as a scoped, self-contained task before any agent touches it. Agents do their best work on ordered, focused chunks — and big features and one-off fixes flow through the same system.
2. **Agents provide specialization.** Workers execute. Specialized agents review — adversarial code review, QA, security — each doing one job well, so every task gets the same expert-shaped scrutiny.
3. **Workflows provide boundaries.** Every task moves through stages you define — review gates, QA, security — before it reaches you. And boundaries are what make autonomy safe: when your gates are catching what you would, flip `shelbi zen on` and let the orchestrator triage, dispatch, and merge green work itself. You get a digest of what landed and what needs you.

### Feature grid (supporting facts)

Kanban TUI · workers on any machine · tmux-native · review flow · events log · open source

### Closer

Install CTA (`curl … | sh` code block) + "Build from source" docs link.

<br />

## Source narrative (founder's words, kept as raw material)

**Background.** My core motivation with the project was that I was getting tired of managing separate workstreams in terminal tabs each running their own agents. I sometimes found myself sending the wrong thing to an agent just because I picked the wrong Terminal tab to respond in. I also found myself forgetting about tabs where they would be paused for a long time waiting for the answer to a question. Shelbi removes the pain of manually orchestrating the agents and allows me to focus more on giving them the right direction on the plan and task level.

**How I work.** My main method is to first create a markdown plan for a feature. Then when I'm satisfied with the plan I ask the Orchestrator to break it into tasks and distribute them to the worker agents. Where Shelbi shines is in giving the agents discrete chunks of work. Task-level turns out to be an extremely helpful resolution for worker agents. By breaking large plans down into tasks, and creating one-off tasks for smaller work, Shelbi can balance large and small things well.

**On the orchestrator.** The orchestrator is key. So much easier to talk to one agent, rather than managing agents in multiple tabs. Because the orchestrator doesn't do the work, but rather assigns it to others, you can dump work on it as it occurs to you without worrying about getting it distracted. Its job is to write the work up for you in a way that other agents can execute. The agents benefit from ordered, focused work. It's like Inbox Zero. Instead of keeping all of this stuff in your head you just tell it to the orchestrator. It organizes it and makes sure it gets done. A relationship of trust develops. Eventually you turn on Zen Mode so that the orchestrator can blitz through the work.

**On quality.** Shelbi provides the primitives you need to support workflows that produce extremely high quality code — specialized agents for adversarial code review, QA, and security, each governing a column on the board so that every task benefits from the same scrutiny. Work can be delivered at a higher quality bar, not lower. The key is building that bar into the system. Workflows provide boundaries. Agents provide specialization. Tasks keep work focused.
