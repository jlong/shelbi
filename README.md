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

> **Status:** very early — design phase. The plan is in place and v1
> implementation is in progress.

---

## Why

GUI tools like Conductor are great, but they're Mac-only and single-machine.
If you live in the terminal and you have spare compute (a Mac mini in the
closet, a Linux box, a workstation), you want to *spread* your agents across
them — without standing up a Kubernetes cluster.

shelbi gives you:

- **One orchestrator, many workers.** Talk to a single agent in chat. It
  dispatches tasks across machines and tracks progress for you.
- **Run anywhere tmux runs.** Hub-side: Linux, macOS, Windows. Workers: any
  machine reachable by `ssh` with `tmux` + your agent CLI installed.
- **Worktree isolation per task.** Each task runs on its own git branch in
  its own worktree. Review, merge, archive — independently.
- **No daemons, no servers.** Just `ssh`, `tmux`, `git`, and your agent CLI.
- **Pluggable agents.** Claude Code, Codex, aider, or any CLI you can drive
  interactively — declared per project.
- **Markdown state, not a database.** Every agent's task, log, and status is
  a markdown file with YAML frontmatter. Grep it, version-control it, read
  it from your editor.

---

## How it works

```
                  you (terminal)
                        │
                ┌───────▼────────┐
                │ shelbi (TUI)   │   ratatui, Ctrl+Space palette
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
                 │     │ spawn, status, │
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
  shelbi's tmux session. It uses the `shelbi` CLI as its tool surface (same
  CLI you use yourself).
- Each **worker** is `claude` (or codex, etc.) running interactively in a
  tmux pane — locally on the hub, or in a tmux session on a remote machine
  reached over SSH.
- shelbi drives workers with `tmux send-keys` + `capture-pane`, transparently
  prefixed with `ssh host --` for remotes.
- Worker state lives in `~/.shelbi/projects/<name>/agents/<id>.md` (frontmatter
  + log).

---

## TUI

Two-pane layout. Borderless sidebar on the left — project name as a
strong header, a tight 3-item nav (Chat, Tasks, Machines), then live
inline lists of declared workers (`— agents —`) and tasks waiting on
you (`— Ready for Review —`). Content view on the right is a real
tmux pane (orchestrator, worker, or one of the built-in views).
`Ctrl+P` opens a fuzzy command palette for switching views, selecting
workers, and triggering actions — same convention as VS Code /
JetBrains / Telescope.

Review is intentionally *not* in the nav: it surfaces inline below as
a live list of tasks that need your attention, not a destination to
jump to.

```
 myapp                  Chat — orchestrator
                        you: fix the login bug on safari,
 💬 Chat                  send it to delta.
 📋 Tasks
 🖥 Machines            shelbi: ✓ dispatched to devbox.
                          worker: delta
 — agents —               branch: shelbi/fix-login-bug
 ⏵ alpha
 💬 bravo               you: how's it going?
 · charlie
 ✓ delta                shelbi: editing tests now — should
                          be done in a couple minutes.
 — Ready for Review —     Ctrl+P to jump to it.
 ✓ fix-login     delta
 ✓ csv-export    charlie  > _

   ^P palette  Enter focus
   q  quit shelbi
```

Worker state badges: `⏵` working · `💬` awaiting input · `⚠`
awaiting permission · `✓` review-ready · `·` idle. Review rows reuse
`✓` (cyan) to mean *ready for you*.

Press `e` on any agent or diff hunk to open your `$EDITOR` on the worktree
(or specific file) — locally or transparently over SSH for remote workers.

---

## Concepts

| Concept | What it is |
|---|---|
| **Project** | A git repo + the machines it can run on + the agent runners available. Declared in `~/.shelbi/projects/<name>.yaml`. |
| **Session** (workspace) | One or more projects loaded together, tmuxinator-style. `~/.shelbi/sessions/<name>.yaml`. |
| **Machine** | A host. Either `local` (the hub) or `ssh` with a host and a working directory. |
| **Agent runner** | A pluggable CLI command shelbi knows how to launch (claude, codex, aider, …). |
| **Worker** | One running task = one git worktree + one branch + one tmux pane + one agent CLI. |
| **Orchestrator** | The agent in window 1 you talk to. Drives workers via the `shelbi` CLI. |

---

## Quickstart (target — not yet implemented)

```bash
# install
cargo install shelbi    # eventually

# initialize
shelbi init             # scaffold ~/.shelbi/, walk through first project

# launch a workspace
shelbi daily            # opens TUI in a tmux session named shelbi-daily

# … or use the CLI directly
shelbi spawn fix-login --on m2 --runner claude "fix the login bug on safari"
shelbi status
shelbi diff fix-login
shelbi merge fix-login
```

Inside the TUI: type into Chat, or press `Ctrl+P` to fuzzy-find anything.

---

## Platform support

shelbi compiles and runs on **Linux, macOS, and Windows**. The hub-side binary
is fully portable.

`tmux` and the agent CLI are required on whichever side runs workers:

- Native on Linux/macOS.
- On Windows: WSL is the path of least resistance for local workers; for
  remote-only setups (workers all over SSH to Linux/macOS hosts), no WSL
  needed.

---

## Configuration

### Session (workspace)

```yaml
# ~/.shelbi/sessions/daily.yaml
name: daily
projects:
  - myapp:    { machines: [hub, m2] }
  - sideproj: { machines: [hub] }
startup:
  - open: dashboard
  - tail: myapp/orchestrator
```

### Project

```yaml
# ~/.shelbi/projects/myapp.yaml
name: myapp
repo: git@github.com:me/myapp.git
default_branch: main
machines:
  - name: hub
    kind: local
    work_dir: ~/Workspaces/myapp
  - name: m2
    kind: ssh
    host: m2.local
    work_dir: ~/work/myapp
orchestrator:
  runner: claude
agent_runners:
  claude: { command: "claude", flags: [] }
  codex:  { command: "codex", flags: [] }
```

### Agent state (auto-generated)

```markdown
---
id: fix-login-bug
project: myapp
machine: m2
runner: claude
branch: shelbi/fix-login-bug
worktree: ~/work/myapp/.shelbi/wt/fix-login-bug
status: running
created: 2026-06-18T14:22:11Z
tmux: { session: shelbi-daily, window: w-fix-login-bug }
---

# Task

Fix the login bug on Safari — cookie domain mismatch breaks SSO redirect.

## Progress

- read `src/auth/session.ts`
- editing tests…
```

---

## Roadmap

| Phase | Status | Goal |
|---|---|---|
| 0 | 🛠 in progress | Cargo workspace + crate scaffolding |
| 1 |   | Single local worker — `shelbi spawn` opens a tmux window, runs an agent, captures output |
| 2 |   | Full CLI: spawn / send / status / tail / diff / merge / list / archive |
| 3 |   | Orchestrator bridge — window 1 + generated system prompt + CLI tool surface |
| 4 |   | TUI dashboard — two-pane layout + Ctrl+Space palette |
| 5 |   | SSH/remote workers — `Local \| Remote` host abstraction |
| 6 |   | Session config — workspace YAML, multi-project |
| 7 |   | Review/merge polish — inline diff, `--pr` flow, archive flow |
| 8 |   | Hardening — reconnect on SSH drop, `shelbi init` scaffolder |

---

## License

MIT. See [LICENSE](LICENSE).
