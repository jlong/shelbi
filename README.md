# shelbi

> An open-source agent orchestrator for the terminal, built on tmux.

shelbi lets you run, supervise, and review AI coding agents (Claude Code,
Codex, aider, anything with a CLI) running in parallel across your laptop and
a fleet of remote machines. You talk to one **orchestrator agent**; it
delegates work to **worker agents** running in tmux panes — locally or over
SSH — and reports back. You jump into any worker's pane when you want to
watch live, and review/merge the diffs from a two-pane TUI.

Part replacement-for-tmuxinator. Part terminal-native Conductor. All you need
on a worker machine is `tmux` and your agent CLI.

---

## Why

GUI tools like Conductor are great, but they're Mac-only and single-machine.
If you live in the terminal and you have spare compute (a Mac mini in the
closet, a Linux box, a workstation), you want to *spread* your agents across
them — without standing up a Kubernetes cluster.

shelbi gives you:

- **One orchestrator, many workers.** Talk to a single agent in chat. It
  dispatches tasks across machines and tracks progress for you.
- **Run anywhere tmux runs.** Hub-side: Linux, macOS, Windows (via WSL).
  Workers: any machine reachable by `ssh` with `tmux` + your agent CLI
  installed.
- **Worktree isolation per task.** Each task runs on its own git branch in
  its own worktree. Review, merge, archive — independently.
- **No daemons, no servers.** Just `ssh`, `tmux`, `git`, and your agent CLI.
- **Pluggable agents.** Claude Code, Codex, or any CLI you can drive
  interactively — declared per project.
- **Markdown state, not a database.** Every task, log, and worker status is
  a markdown or YAML file. Grep it, version-control it, read it from your
  editor.

---

## Install

For now: clone and build from source. `cargo install shelbi` will land once
the first crate is published.

```bash
git clone https://github.com/jlong/shelbi.git
cd shelbi
./scripts/install.sh
```

The script runs `cargo build --release` and drops the binary at
`$HOME/bin/shelbi` (override with `SHELBI_INSTALL_PATH=/somewhere/else`).
On macOS it re-signs the copied binary ad-hoc — without that step the kernel
SIGKILLs the process on first exec with no useful error. Linux and Windows
need nothing extra.

Confirm the install:

```bash
shelbi --version
```

You'll also want `tmux` (≥ 3.2) on the hub and on any remote worker
machines, plus whichever agent CLI you intend to run (`claude`, `codex`, …).

---

## First run

Run `shelbi` with no projects configured and it drops you into the wizard.
Two phases: name your assistant, then walk one or more projects through
setup. Each phase is idempotent — re-running the wizard skips anything
already on disk.

```
$ shelbi
? What should we call your assistant? Orchestrator
✓ assistant: Orchestrator

? Project name: myapp
? Path to the repo: /Users/you/Workspaces/myapp
? Default branch: main
? GitHub repo URL (optional): git@github.com:you/myapp.git
? Hub machine name: hub
? Hub work directory: /Users/you/Workspaces/myapp
? Add a remote machine? Yes
  ? Remote machine name: devbox
  ? SSH host: devbox.local
  ? Work directory on remote: /home/you/work/myapp
? Add another remote machine? No
? Agent runner (used by every worker): claude
  (memory: 64 GB → recommended 4 workers per machine — configurable later)
? Worker count per machine: 4
? Worker naming style: phonetic (alpha, bravo, charlie, …)
? Orchestrator runner: claude
✓ Project myapp created (8 workers: alpha, bravo, charlie, delta, echo, foxtrot, golf, hotel).

? Set up another project? No
```

What the wizard auto-fills from your environment:

- **Project name** — current directory's basename.
- **Path to the repo** — current working directory.
- **Default branch** — `origin/HEAD` (falls back to `main`).
- **GitHub repo URL** — `git remote get-url origin`.
- **Hub work directory** — same as the repo path.
- **Worker count** — heuristic from total RAM (~16 GB per worker).
- **Worker naming style** — choose between phonetic alphabet, Greek
  letters, or Toy Story characters. Workers are laid out machine by
  machine, so the first N names land on the hub, the next N on the first
  remote, and so on.

After the wizard, shelbi launches the TUI for the project it just created.
A `.shelbi/project` marker is dropped at the repo root so subsequent
invocations from inside the repo auto-select it.

---

## Returning to shelbi

`shelbi` with no arguments dispatches based on what it finds:

- **No projects on disk** → onboarding wizard (above).
- **One project** → launches its TUI directly.
- **Two or more** → fuzzy project picker (type to filter, most-recently-
  launched at the top, `+ Add a new project` at the bottom).
- **Inside a repo with a `.shelbi/project` marker** → that project always
  wins.

Skip the picker explicitly:

```bash
shelbi -p myapp           # or SHELBI_PROJECT=myapp shelbi
```

`shelbi` reuses an existing tmux session named `shelbi-<project>` if one is
running, or creates a fresh one if not.

---

## The day-to-day loop

Once you're in the TUI, the loop is one continuous conversation, not a
checklist of CLI invocations.

You tell the orchestrator what you want in plain English — "fix the login
bug on Safari", "add a CSV export to the reports page", "split the auth
service into its own crate." It drops cards into the **backlog** column,
each one a markdown file with a short prompt and a proposed branch name.

You triage the backlog by moving cards to **todo**. The orchestrator
watches the todo column: as soon as a worker frees up, it picks the
highest-priority unblocked card, checks it out on a fresh branch in the
worker's worktree, and starts the runner with the task prompt. The card
moves to **in_progress** automatically.

When a worker finishes, it moves its card to **review** and stops. The
sidebar surfaces a `Ready for Review` list with a cyan `✓` next to each
title — that's your queue. Click one (or press Enter) and shelbi checks
the branch out into the machine's review work_dir and spawns a fresh
Claude pane there for you to interrogate the diff. Approve → merge into
the default branch (or push and open a PR with `--pr`). Done.

The orchestrator handles dispatch and progress tracking. You handle
direction and review. Nothing in the middle needs CLI ceremony.

---

## TUI tour

Two-pane layout. Borderless sidebar on the left — project name as a
strong header, then a 3-item nav (Chat, Tasks, Machines), then live
inline lists of declared workers (`— agents —`) and tasks waiting on
you (`— Ready for Review —`). Content view on the right is a real
tmux pane: the orchestrator, a worker, or one of the built-in views.

```
 myapp                  Chat — Orchestrator
                        you: fix the login bug on safari,
 💬 Chat                  send it to delta.
 📋 Tasks
 🖥 Machines            Orchestrator: ✓ dispatched to delta.
                          worker: delta
 — agents —               branch: shelbi/fix-login-bug
 ⏵ alpha
 💬 bravo               you: how's it going?
 · charlie
 ✓ delta                Orchestrator: editing tests now — should
 · echo                   be done in a couple minutes.
                          Ctrl+P to jump to it.
 — Ready for Review —
 ✓ fix-login     delta   > _
 ✓ csv-export    charlie

   ^P palette  Enter focus
   q  quit shelbi
```

Worker state badges in the sidebar:

| Badge | Meaning |
|---|---|
| `⏵` | working — agent actively running a turn |
| `💬` | awaiting input — finished a turn, sitting at the prompt |
| `⚠` | awaiting permission — showing a permission dialog |
| `✓` | review-ready — task moved to the review column |
| `·` | idle — no in-flight task assigned |

`Ctrl+P` opens a fuzzy command palette as a tmux popup — for switching
projects, jumping to a worker, swapping the right pane to Tasks /
Machines / Review, or triggering actions. Same convention as VS Code /
JetBrains / Telescope.

`Enter` focuses the highlighted row: a nav item swaps the right pane to
that view; a worker switches to its tmux window (or opens an SSH proxy
window for a remote worker); a review row checks out the branch and
spawns the review pane. Press `e` on any worker to open your `$EDITOR`
on its worktree.

The built-in views on the right pane:

- **Chat** — the orchestrator agent running in window 1. This is where you
  talk to shelbi.
- **Tasks** — a 5-column Kanban (Backlog / Todo / In Progress / Review /
  Done). Reorder with `j/k`, move with the column-name keybindings.
- **Machines** — declared machines + their worker assignments and SSH
  health.
- **Review** — surfaces inline as the review queue; activating a row
  triggers the review checkout.

---

## Concepts

| Concept | What it is |
|---|---|
| **Project** | A git repo + the machines it can run on + the worker pool + the agent runners available. Declared in `~/.shelbi/projects/<name>.yaml`. |
| **Machine** | A host. Either `local` (the hub) or `ssh` with a host and a working directory. |
| **Agent runner** | A pluggable CLI command shelbi knows how to launch (`claude`, `codex`, …). |
| **Worker** | A named, persistent slot pinned to a machine and a runner. Picks up the next ready task and runs it in an isolated worktree. |
| **Orchestrator** | The agent in window 1 you talk to. Creates tasks, dispatches workers via the `shelbi` CLI. |
| **Task** | A markdown file in `~/.shelbi/projects/<name>/tasks/`. Moves through Backlog → Todo → In Progress → Review → Done. |

---

## How it works

```
                  you (terminal)
                        │
                ┌───────▼────────┐
                │ shelbi (TUI)   │   ratatui, Ctrl+P palette
                │   hub binary   │
                └───┬──────┬─────┘
                    │      │
       owns one     │      │ talks to orchestrator
       tmux session │      │ via tmux pipe
                    │      │
            ┌───────▼──┐ ┌─▼────────────────┐
            │ tmux     │ │ orchestrator     │
            │ session  │ │ agent (e.g.      │
            │ shelbi-* │ │ claude) window 1 │
            └────┬─────┘ └────┬─────────────┘
                 │            │
                 │            │ shells out to
                 │            ▼
                 │     ┌────────────────┐
                 │     │ shelbi CLI     │
                 │     │ task add/move, │
                 │     │ diff, merge…   │
                 │     └────┬───────────┘
                 │          │
                 │   for each worker:
                 ▼          ▼
       ┌──────────────────────────────────┐
       │ ssh remote -- tmux send-keys /   │
       │ capture-pane to a remote pane    │
       │ running `claude` interactively   │
       └──────────────────────────────────┘
```

- The **orchestrator** is just another agent CLI — running in window 1 of
  shelbi's tmux session. It uses the `shelbi` CLI as its tool surface (the
  same CLI you use yourself).
- Each **worker** is `claude` (or codex, etc.) running interactively in a
  tmux pane — locally on the hub, or in a tmux session on a remote machine
  reached over SSH.
- shelbi drives workers with `tmux send-keys` + `capture-pane`,
  transparently prefixed with `ssh host --` for remotes. A poller watches
  each worker's pane title for `shelbi:<state>` markers and writes them to
  `~/.shelbi/workers/<name>/status.yaml`, which is what drives the sidebar
  badges.
- Project state lives in `~/.shelbi/projects/<name>/` — tasks, agents,
  worker settings, and a YAML config. Worker state lives in
  `~/.shelbi/workers/<name>/`. Everything is plain text.

---

## Common workflows

**Dispatch a worker explicitly without waiting for the orchestrator.**

```bash
shelbi task start fix-login --worker delta
```

**Switch projects from anywhere.**

```bash
shelbi -p sideproj    # or just `shelbi` and pick from the fuzzy list
```

**Add a remote machine to a project that already exists.** Edit
`~/.shelbi/projects/<name>.yaml` — append a machine to the `machines:`
list and a worker per slot to `workers:`. Reload the dashboard:

```bash
shelbi reload
```

**Pick up a fresh binary after `./scripts/install.sh`.** The orchestrator
and worker panes re-shell into shelbi on every call, so they pick up
the new binary automatically. The sidebar / Tasks / Review panes don't
— respawn them in place with:

```bash
shelbi reload
```

---

## Configuration

### Project

```yaml
# ~/.shelbi/projects/myapp.yaml
name: myapp
repo: /Users/you/Workspaces/myapp
default_branch: main
github_url: git@github.com:you/myapp.git
machines:
  - name: hub
    kind: local
    work_dir: /Users/you/Workspaces/myapp
  - name: devbox
    kind: ssh
    host: devbox.local
    work_dir: /home/you/work/myapp
orchestrator:
  runner: claude
agent_runners:
  claude: { command: claude, flags: [] }
  codex:  { command: codex,  flags: [] }
workers:
  - { name: alpha,    machine: hub,    runner: claude }
  - { name: bravo,    machine: hub,    runner: claude }
  - { name: charlie,  machine: devbox, runner: claude }
  - { name: delta,    machine: devbox, runner: claude }
```

### Task (auto-generated by the orchestrator)

```markdown
---
id: fix-login-bug
title: Fix login bug on Safari
column: in_progress
priority: 0
assigned_to: delta
branch: shelbi/fix-login-bug
created_at: 2026-06-18T14:22:11Z
updated_at: 2026-06-19T09:14:02Z
---

# Task

Fix the login bug on Safari — cookie domain mismatch breaks SSO redirect.

## Notes

- read `src/auth/session.ts`
- editing tests…
```

---

## Contributing

shelbi is a Cargo workspace under `crates/`:

| Crate | Purpose |
|---|---|
| `shelbi-cli` | The `shelbi` binary, subcommands, wizard. |
| `shelbi-core` | Domain model — Project, Task, Machine, Worker, Column. |
| `shelbi-state` | Filesystem layout — load/save YAML and markdown under `~/.shelbi/`. |
| `shelbi-tui` | Ratatui sidebar, Kanban, review queue. |
| `shelbi-tmux` | tmux session/window/pane helpers. |
| `shelbi-ssh` | SSH-prefixed command execution for remote workers. |
| `shelbi-agent` | Agent-runner abstraction and pane-driving logic. |
| `shelbi-orchestrator` | Orchestrator window setup, review checkout, palette views. |
| `shelbi-palette` | Nucleo-backed fuzzy matcher used by the picker and palette. |

Build, test, install:

```bash
cargo build --workspace
cargo test --workspace
./scripts/install.sh
```

Bugs and feature requests: <https://github.com/jlong/shelbi/issues>.

---

## License

MIT. See [LICENSE](LICENSE).
