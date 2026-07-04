# Review Workspaces

**Status:** Draft plan (2026-07-02)
**Author:** Orchestrator, from JWL's brief

## 1. Motivation

Today shelbi's review step is diff-centric: `shelbi review <task>` checks a
branch into the machine's top-level clone and drops a bare Claude pane there so
a human (or the orchestrator) can read the change. That's fine for small edits,
but most real review of an app or a site means **running it** — booting the dev
server, hitting a URL, clicking through the change. There is no place in shelbi
today that runs a long-lived server, owns a port, or prepares a loaded
environment for a human to look at.

**Review workspaces** fill that gap. A review workspace is a normal pool
workspace — it has a machine, a runner, and a persistent worktree — but it is
*designated for human review* rather than autonomous development. When a task
enters the Review status, shelbi loads that task's branch onto a review
workspace, and a new default **Review agent** prepares the environment (install
deps, build, start/refresh the dev server). Once it's loaded and serving, it's
the human's turn to inspect the running app and accept or bounce the change.

Review workspaces are scarce by design: **1–2 per machine**, because each may
hold a running server and a port.

## 2. Terminology — resolve the "review" collision first

"Review" is already overloaded in the codebase. Before adding a fourth meaning,
name them explicitly and pick which ones this feature subsumes:

1. **`shelbi review <task>`** **/** **`review.rs`** — checks a branch into
   `machine.work_dir` (the top-level clone) and launches a prompt-only Claude
   pane (`review_tmux_addr` → window `review`). *This feature replaces it* (§11).
2. **The** **`__review`** **queue TUI** — a read-only ratatui list of tasks in the
   review column, stashed as a dashboard pane (`lib.rs` `review_cmd`). *Kept as-is*
   — it's a viewer, not a workspace.
3. **`Column::Review`** **/** **`StatusCategory::Handoff`** — the board status a task
   sits in awaiting human input. *This is the trigger* (§8), unchanged.
4. **Review workspace (new)** — a pool slot that loads and serves a task's
   branch for human review.

Proposed convention: "review **status**" (the board column), "review
**workspace**" (the slot), "the **Review agent**" (the loader). Avoid bare
"review" in new code and docs.

## 3. Current state (grounded in code)

- **`WorkspaceSpec`** (`shelbi-core/model.rs:568`) has exactly `name`, `machine`,
  `runner`. No role/kind discriminant. `validate_workspaces` (model.rs:426) only
  checks machine + runner resolve. New `WorkspaceSpec` fields ride in the
  user-local config half (`LOCAL_PROJECT_FIELDS`, model.rs:173).

- **Worktree path is hardcoded** `<work_dir>/.shelbi/wt/<name>` (`workspace.rs:53`),
  doc'd "not configurable yet." A review workspace fits this fine — it *is* a
  pool worktree.

- **Dispatch** (`start_workspace_on_task`, `workspace.rs:471`) syncs the worktree,
  deploys agent context, kills+recreates a single agent pane, waits for readiness,
  sends the prompt, and expects the agent to write a review-ready marker. Agent is
  resolved from the task's workflow **active** status (`resolve_active_agent_for_dispatch`,
  `task.rs:495`), not from the WorkspaceSpec.

- **Agents** are scaffolded from `DEFAULT_AGENTS` (`agent_workspaces.rs:98`) —
  today only `orchestrator` + `developer`, each a `default_*.md.template`. Adding
  a default agent = new template + `BundledAgent` entry; materialize/self-heal
  pick it up. **No** **`review`** **agent exists.**

- **Trigger**: the poller's `maybe_promote_to_review` (`poller.rs:445`) reads the
  review-ready marker, rebases the branch onto default, moves the task to
  `Column::Review`, emits `to_category=handoff`. Nothing auto-launches anything on
  promotion — the orchestrator is expected to react to the event stream.

- **`shelbi review`** uses `machine.work_dir` (top-level clone), and actively
  *evicts* pool worktrees off the branch (`release_branch_from_workspace_worktrees`,
  `review.rs:263`) because git forbids one branch in two worktrees. Review runs
  the bare `orchestrator.runner` with a prompt only — **no agent context, no
  skills, no** **`--append-system-prompt`.**

- **No dev-server / port / long-running-process infrastructure exists anywhere.**
  Every pane runs exactly one foreground process; one pane per window is baked
  into `TmuxAddr` (model.rs:687). A persistent server pane beside the agent pane
  is entirely new capability.

## 4. Design decisions (confirmed with JWL)

| # | Decision                                             | Choice                                                                                                                                                                                 |
| - | ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A | How the Review agent knows how to load/run a project | **Hybrid**: project may declare explicit setup/serve commands; the Review agent **auto-detects** (package.json→npm, Cargo.toml→cargo, …) when none are declared. Declared always wins. |
| B | Ports for concurrent review servers                  | **Base port + per-workspace offset.** Project declares a base (e.g. 3000); each review workspace gets a deterministic slot (review-1→3000, review-2→3010). Agent injects `PORT`.       |
| C | When all review workspaces are busy                  | **Queue.** The task stays in Review, marked pending-load; the orchestrator loads it onto the first review workspace that frees. Nothing preempted.                                     |
| D | Relationship to existing `shelbi review`             | **Replace it.** Review workspaces become the canonical review surface; the old per-machine review-dir flow is retired (§11).                                                           |

## 5. Schema changes

### 5.1 Mark a workspace as review-only

Add an optional role to `WorkspaceSpec` (user-local half):

```yaml
workspaces:
  - { name: alpha,    machine: hub,    runner: claude }
  - { name: review-1, machine: hub,    runner: claude, role: review }
  - { name: review-1, machine: devbox, runner: claude, role: review }
```

```rust
// shelbi-core/model.rs
# [derive(Default, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
# [serde(rename_all = "lowercase")]
pub enum WorkspaceRole { #[default] Dev, Review }

pub struct WorkspaceSpec {
    pub name: String,
    pub machine: String,
    pub runner: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub role: WorkspaceRole,
}
```

- `role` defaults to `Dev` → every existing YAML keeps working untouched.

- `validate_workspaces` extension: warn (don't hard-fail) if a machine declares
  **zero** review workspaces but the project relies on the new review flow;
  hard-error if a machine declares **> 2** review workspaces (scarcity is a design
  invariant — surface accidental over-provisioning). Tunable.

- Helpers: `Project::review_workspaces(machine)`, `Project::dev_workspaces()`,
  `WorkspaceSpec::is_review()`.

### 5.2 Per-project review/run config

New optional `review:` block on `Project` (schema is the auto-detect override):

```yaml
review:
  base_port: 3000          # per-workspace offset applied on top
  port_stride: 10          # review-1→3000, review-2→3010
  setup:                   # optional; auto-detected when omitted
    - npm install
  serve: npm run dev -- --port $PORT   # optional; auto-detected when omitted
  ready_probe:             # optional; how the agent knows the server is up
    http: http://localhost:$PORT
    timeout: 90s
```

```rust
pub struct ReviewConfig {
    #[serde(default = "default_base_port")] pub base_port: u16,   // 3000
    #[serde(default = "default_port_stride")] pub port_stride: u16, // 10
    #[serde(default)] pub setup: Vec<String>,
    #[serde(default)] pub serve: Option<String>,
    #[serde(default)] pub ready_probe: Option<ReadyProbe>,
}
```

All fields optional; an absent `review:` block means "auto-detect everything,
base port 3000." Belongs in the shared (repo) config half — it's about the
project, not the machine — except `base_port`, which may want a local override
if two machines share a port space (start shared, add a local override field
only if needed).

## 6. The Review agent

A new **default agent** named `review`, scaffolded like `developer`:

- `crates/shelbi-state/src/default_review.md.template` — its instructions.

- `pub const REVIEW_AGENT = "review"`; a `BundledAgent` entry appended to
  `DEFAULT_AGENTS` (`agent_workspaces.rs:98`) so materialize/self-heal create it.

- Reuses the workspace-settings template (auto permission mode) and gets a
  `skills/` dir like the others.

**Charter (its instructions.md):**

1. You are on a *review workspace*. The branch for task `<id>` is already checked
   out in your worktree. Your job is to make the change **runnable for a human**,
   not to modify code.
2. Determine the load steps: if the project declared `review.setup`/`review.serve`,
   use them verbatim. Otherwise auto-detect —

   - `package.json` → `npm install` (or pnpm/yarn per lockfile) then the `dev`
     script; Next.js/Vite → pass `--port $PORT`.

   - `Cargo.toml` → `cargo build`; a bin/example → `cargo run` with `PORT`/`--port`.

   - fall back to reporting "no runnable server detected — diff-only review."
3. Run setup, then start (or **refresh**, if already running for a prior task —
   §7) the dev server on your assigned `$PORT`, in a **dedicated server pane**
   (§10), backgrounded from your agent pane.
4. Wait for the ready probe (HTTP 200 on the URL, or a log-line match, or a fixed
   settle), then post a concise "ready" summary to the human: the URL
   (`http://<machine-host>:<port>`), what changed (branch + one-line task
   summary), and any setup warnings. Emit a `workspace=<name> review_ready=true url=<...>` event so the orchestrator/board can show it.
5. Then **stop and hand to the human.** Do not auto-merge, do not keep editing.
   The human inspects, then moves the task (done / back to todo). If they ask for
   a fix, you may make the tweak and refresh the server.

The Review agent is deliberately **not** a `developer`: its skills/prompt are
environment-and-serve oriented, and it must never treat "review" as "keep coding."

## 7. Load / run mechanics

- **Port assignment** is deterministic from the workspace's index among its
  machine's review workspaces: `port = base_port + index * port_stride`. Computed
  by shelbi at dispatch and injected as `PORT` (and passed into `serve` via
  `$PORT`). This means review-1 and review-2 never collide, and the URL is
  predictable.

- **Setup vs serve**: setup commands are one-shot and must exit 0 before serve.
  Serve is long-lived and owns the server pane.

- **Refresh semantics**: when a *new* task loads onto a review workspace that
  already has a server running (previous review finished/abandoned), the loader
  tears the old server down (kill server pane) and starts fresh for the new
  branch. When the *same* task gets a follow-up (human asked for a tweak), the
  agent restarts/hot-reloads in place. "Refresh if needed" = detect whether the
  running server corresponds to the current branch+commit; if not, restart.

- **Auto-detect precedence**: declared `review.serve` > framework heuristic >
  generic (`Makefile` `dev`/`serve` target, `Procfile` `web:`) > "diff-only."
  Detection is the Review agent's job (prompt + a small skill), not Rust — keeps
  it flexible and per-repo without schema churn (Decision A).

## 8. Trigger flow: Review status → load onto a review workspace

This is the orchestrator's job, as an extension of the existing auto-dispatch
contract. Today a task entering `handoff` just sits for the user. New rule:

> On `task=<id> ... to_category=handoff`: if the project has review workspaces,
> **auto-load** the task onto a free review workspace on the task's machine
> (preferring the machine the task ran on), by dispatching it there with the
> **`review`** **agent**. If none is free, leave it in Review marked *pending-load*
> and load it when a review workspace frees (§9).

Mechanically this reuses `start_workspace_on_task` with two differences:

1. `StartSpec.agent = Some("review")` (not the workflow's active agent).
2. The worktree is **already on the branch** (the dev workspace rebased it before
   handoff), so sync is a checkout, and the branch may need releasing from the
   dev worktree first — reuse `release_branch_from_workspace_worktrees` logic,
   now generalized to "move a branch from a dev worktree to a review worktree."

The Review agent's own review-ready marker is repurposed as the **"loaded and
serving"** signal — distinct from the dev "ready for review" marker. Suggest a
separate marker file (`.claude/shelbi-review-loaded`) carrying the URL, so the
board can show "▶ serving at :3000" vs "⏳ loading."

### Board / status implications

- A task in Review has a sub-state: **pending-load → loading → serving →
  (human looking)**. Model this as workspace status detail on the review
  workspace, not new board columns (keep the board's category vocabulary intact).

- The `__review` queue TUI (§2.2) gains a column: which review workspace + URL a
  task is loaded on.

## 9. Scarcity / queue handling (Decision C)

- Review workspaces per machine: 1–2. When a task hits Review and all are busy,
  it **waits in Review** (pending-load). No preemption.

- The orchestrator loads the oldest pending-load task onto a review workspace the
  moment one frees (on `workspace=<review-name> ... -> idle`, same shape as the
  normal free-workspace reaction).

- A review workspace "frees" when the human resolves its task (moves it out of
  Review — to done or back to todo). That transition tears down the server pane
  and clears the loaded marker.

- Heartbeat backstop: the periodic sweep also picks up any pending-load task if
  the free-event was missed (mirrors the existing missed-dispatch recovery).

## 10. tmux / pane model for a running server

A review workspace needs **two panes**: the Review agent pane *and* a persistent
server pane. This is new — everything today is one-pane-per-window.

Proposal:

- Keep the agent pane as the workspace's window (`workspace_tmux_addr`).

- Add a **server pane** via `split-window` in the same window (there's precedent:
  `lib.rs:368/524` split the dashboard). Name/track it by a convention
  (`<session>:<window>.server` or a stored pane id in workspace status).

- The server pane runs `cd <worktree> && PORT=<port> <serve>` under the lifecycle
  wrapper so its death emits an event (reuse `open --as-pane` shape, or a new
  `open --as-server-pane`).

- **Liveness**: shelbi already can't rely on pane titles (automatic-rename); track
  the server pane's liveness via the wrapper's `pane_alive` event and an optional
  HTTP ready-probe rather than a title marker.

- Port conflicts / leaked servers: on teardown (task leaves Review, workspace
  re-dispatched, or `shelbi workspace stop`), kill the server pane explicitly.
  Add a reaper: if a review workspace has no active review task but a live server
  pane, kill it (covers crashes).

Open sub-decision: single window with a split (simple, both visible) vs a
dedicated server *window* per review workspace (cleaner liveness, one more window).
Recommend the split for visibility during review.

## 11. Replacing `shelbi review` (Decision D)

- `shelbi review <task>` becomes a thin alias that **loads the task onto a review
  workspace** (picks a free one on the resolved machine, or reports the queue
  position) instead of checking out into `machine.work_dir`.

- Retire `resolve_review_machine`'s "check out into the top-level clone" path and
  the `release_branch_from_workspace_worktrees` eviction that it forced. The
  branch now lives in the review workspace's own worktree — no eviction of dev
  worktrees, no dirtying the top-level clone.

- The `review` window naming (`review_tmux_addr`) is replaced by the review
  workspace's own window. `shelbi review` with no free review workspace prints the
  queue position rather than failing.

- Migration: projects with no `role: review` workspaces declared get a **loud
  onboarding error** from `shelbi review` ("declare at least one `role: review`
  workspace") plus a doc pointer. (We chose full replacement over opt-in, so this
  is a hard cutover with a helpful message, not a silent fallback.)

## 12. Human-review lifecycle (end to end)

1. Dev workspace finishes → writes review-ready marker → poller rebases + moves
   task to Review.
2. Orchestrator sees `to_category=handoff` → loads the branch onto a free review
   workspace with the Review agent (or queues it).
3. Review agent installs/builds, starts the server on its port, probes ready,
   writes the loaded marker with the URL, posts a "ready at http\://…:PORT" note,
   and stops.
4. Human opens the review workspace (sidebar click → focuses the window; server
   pane visible), inspects the running app.
5. Human decides:

   - **Accept** → move task to Done (Zen/orchestrator merges per existing rules);
     server pane torn down; workspace frees; next pending-load task loads.

   - **Send back** → move task to Todo; server torn down; a dev workspace picks it
     up again.

   - **Ask for a tweak** → tell the Review agent; it edits + refreshes the server
     in place (the one case the Review agent touches code).

## 13. Backward compatibility & rollout

- `role` defaults to `Dev`; absent `review:` block ⇒ auto-detect + base 3000.
  Existing projects parse and run unchanged until they declare a review workspace.

- The Review agent self-heals in on `shelbi reload` (new `BundledAgent`).

- Split-config: `role` and `base_port`-local-override ride the user-local half;
  `review.setup/serve/ready_probe` ride the shared (repo) half.

- The event-line grammar gains `review_ready`/`review_loaded` verbs — extend the
  back-compat parser (the events line shape is already versioned).

## 14. Phased implementation

1. **Schema + validation** — `WorkspaceRole`, `WorkspaceSpec.role`, `ReviewConfig`,
   `Project::review_workspaces`, validation (≤2/machine, warnings). No behavior yet.
2. **Review agent** — `default_review.md.template`, `REVIEW_AGENT`, `DEFAULT_AGENTS`
   entry, skill for load/run auto-detection. Materialize/self-heal + tests.
3. **Dispatch-with-review-agent + port injection** — teach `start_workspace_on_task`
   (or a `load_review_workspace` sibling) to dispatch a review workspace with
   `agent=review` and `PORT` env; branch-release from the dev worktree.
4. **Server pane** — split-window server pane + lifecycle wrapper + liveness/reaper.
5. **Orchestrator trigger + queue** — the `to_category=handoff` auto-load rule,
   pending-load state, free-on-resolve reload, heartbeat backstop. Update the
   orchestrator instructions (`default_orchestrator.md.template`) and CLAUDE.md.
6. **Replace** **`shelbi review`** — alias to the load path; retire the top-level-clone
   checkout + eviction; onboarding error when no review workspace declared.
7. **TUI** — `__review` queue view shows workspace + URL; sidebar shows review
   workspaces distinctly (grouped, with serving state + port).
8. **Docs** — concepts page for review workspaces; update the review + workspace
   docs; `shelbi review` reference.

Phases 1–2 are safe and independent (land first). 3–4 are the core new capability.
5–6 flip the trigger + retire the old flow (needs 1–4 in place). 7–8 polish.

## 15. Open questions / risks

- **Server pane liveness** without stable pane titles — needs the wrapper-event +
  HTTP-probe approach; confirm it's robust across restarts.

- **Multi-port apps** (frontend + API) — base+offset gives one port; apps needing
  N ports need either a small port *block* per review workspace (`base + index*stride .. +stride`) or explicit `review.ports:`. Recommend reserving a stride-sized
  block per workspace so a project can use `$PORT`, `$PORT+1`, … within its slot.

- **Remote review workspaces** — the URL the human opens must be reachable
  (`machine.host:port`); may need an SSH tunnel or Tailscale-style access. The
  ready summary should print the exact reachable URL for the machine kind.

- **Leaked servers / port exhaustion** — the reaper (§10) is load-bearing; a
  crashed review pane that leaves a server bound will block re-dispatch until
  reaped. Test the kill paths hard.

- **Security** — a review workspace runs a project's `dev` command and binds a
  port; on a shared machine that port is reachable by other local users. Note the
  exposure; consider binding to localhost by default and documenting.

- **Interaction with Zen auto-merge** — should Review-status tasks still be
  eligible for Zen auto-merge, or does "loaded for human review" imply a human
  gate? Likely: review workspaces are the *human* path, so tasks routed to a
  review workspace should **not** auto-merge under Zen — the whole point is human
  eyes. Make this explicit in the orchestrator's Zen rules.

## 16. Sidebar UX (refinement — 2026-07-02, JWL)

Supersedes the sidebar/board notes in §8 where they conflict. Grounded in the
current sidebar (`crates/shelbi-tui/src/sidebar.rs`): today a single `Row::Review`
(sidebar.rs:177) renders review-status tasks as a one-line `✓ <title> … <workspace>`,
and workspace rows (`Row::Workspace`, sidebar.rs:141) carry a decoration glyph that
can read as a completion check.

### Two review sections, keyed to "loaded on a review worktree"

Split review state into **two sidebar sections**:

- **— Ready for Review —**: ONLY tasks **loaded into a review worktree** — the
  Review agent has the branch checked out and is loading/serving it, so a human
  can actually look now. Decoration: **✓** (cyan check), as the current `Row::Review`.

- **— Queued for Review — (new)**: tasks in Review status **waiting for a free
  review workspace** (the pending-load queue, §9). Decoration: **·** (dim middot),
  signalling "not yet loaded."

A task moves Queued → Ready when the orchestrator loads it onto a freed review
workspace. This makes the scarcity model (§9) visible: with 1–2 review workspaces
per machine, everything else stacks in "Queued for Review."

### Two-line entries in both review sections

Each entry in BOTH lists renders on **two lines**:

1. **Line 1** — the task title.
2. **Line 2** — the branch name (`shelbi/<id>` or the task’s `branch:`), dim.

This replaces the current single-line right-aligned `title … <workspace>` layout
for review rows. The review workspace/URL can move to a right-aligned badge on
line 1 or onto line 2; branch name is the priority here. `Row::Review` becomes a
two-line item, so the list’s selection index and row-height math must account for
multi-line rows (today every row is height 1).

### Machine workspaces: NO completion check — close the session immediately

Today a dev workspace lingers after its task goes review-ready and can surface a
check glyph. **Change:** a dev (machine) workspace **closes its tmux session
immediately on completion**. As soon as it writes the review-ready marker and the
poller promotes the task, the workspace tears its pane/session down and returns to
plain **idle** — no check, no lingering "done" glyph. Completion is represented
entirely in the review sections, never on the workspace row.

Implications:

- `Row::Workspace` decoration loses its review-ready/complete check state; it shows
  only active (agent name) or `idle`.

- The handoff path gains an explicit "close session on completion" step for dev
  workspaces (kill the workspace tmux session/pane right after the marker is
  consumed) instead of leaving the finished agent pane alive.

- Frees the dev slot immediately for the next task (tighter turnaround) and keeps
  the "Workspaces" group showing only live work.

- **Ordering is load-bearing:** close the dev session only AFTER the branch is
  safely handed off — sequence: marker → poller rebases + promotes to Review →
  close dev session → load onto a review workspace (or queue). Never close before
  the branch is rebased/promoted, or work could be stranded.

### Glyph vocabulary

| Section           | Glyph               | Meaning                                          |
| ----------------- | ------------------- | ------------------------------------------------ |
| Workspaces (dev)  | agent name / `idle` | live work or free — **no check**                 |
| Ready for Review  | **✓**               | loaded on a review worktree; human can look      |
| Queued for Review | **·**               | in Review status, waiting for a review workspace |

### Mockup

Current (for contrast) — one flat `Row::Review` line, workspaces can show a check:

```
 — Workspaces —
 ▾ hub
   · alpha              idle
   ▸ bravo         Developer
   ✓ charlie      (complete)     ← check lingers on the dev workspace
 — Ready for Review —
 ✓ Cache warm-up on cold start            charlie
```

New sidebar:

```
 shelbi
 💬 Chat
 📋 Tasks
 ⚡ Activity

 — Workspaces —
 ▾ hub
   · alpha              idle
   ▸ bravo         Developer
   · charlie            idle     ← finished → session closed, back to idle
   ▸ review-1        Review
 ▾ devbox
   ▸ delta         Developer
   · echo               idle
   ▸ review-1        Review

 — Ready for Review —
 ✓ Palette fuzzy-match fix
   shelbi/palette-fuzzy-match-fix
 ✓ Dark-mode toggle polish
   shelbi/dark-mode-toggle-polish

 — Queued for Review —
 · Rework onboarding copy
   shelbi/rework-onboarding-copy
 · Retry webhook dead-letters
   shelbi/retry-webhook-dead-letters
 · Migrate CI to arm64
   shelbi/migrate-ci-to-arm64
```

Reading it:

- **Workspaces** — dev slots show `Developer`/`idle` only; review slots show
  `Review` while loading/serving, `idle` when free. **No check anywhere** — a
  finished dev workspace has already closed its session and reads `idle`.

- **Ready for Review** (`✓`) — the two tasks actually loaded on a review worktree.
  Line 1: title + right-aligned `<machine>:<port>` (the URL the human opens). Line
  2: branch, dim. Exactly as many entries as there are occupied review workspaces
  (here 2 review workspaces → at most 2 ready items).

- **Queued for Review** (`·`) — tasks promoted to Review status but not yet loaded
  (all review workspaces busy). Line 1: title. Line 2: branch, dim. No location
  yet — they have no port/URL until one loads. First in this list is next to load
  when a review workspace frees.

Selecting a Ready item focuses that review workspace’s window (server pane
visible); selecting a Queued item is inert (or could show queue position). The
`·` glyph is shared with the idle-workspace marker but never ambiguous in context
(different sections, and queued rows are two-line with a branch beneath).
