# Shelbi

```
   ▄▀▀▀▀▀▄   ▀▀    ▀▀  ▀▀▀▀▀▀▀   ▀▀   ▀▀▀▀▀▀▀▀▀▀▄   ▀▀▀▀▀
  ▀▀        ▀▀    ▀▀  ▀▀        ▀▀        ▀▀    ▀▀   ▀▀
  ▀▀▀▀▄    ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀    ▀▀      ▀▀▀▀▀▀▀▀▄    ▀▀
▄     ▀▀  ▀▀    ▀▀  ▀▀        ▀▀        ▀▀     ▀▀  ▀▀
 ▀▀▀▀▀▀  ▀▀    ▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀
```

> Manage a team of coding agents from your terminal — an open-source orchestrator built on tmux.

Shelbi lets you run, supervise, and review AI coding agents (Claude Code,
Codex, aider, anything with a CLI) running in parallel across your laptop and
on remote machines at the same time. You talk to one **orchestrator agent**;
it delegates work to other **agents** running in **workspaces** (tmux panes) —
locally or over SSH — and reports back. You jump into any workspace's pane
when you want to watch live, and review/merge the diffs from a two-pane TUI.

An **agent** is a role (a system prompt plus skills — orchestrator, developer,
QA, security review); a **workspace** is the capacity it runs in (one tmux pane
plus one git worktree, pinned to a machine).

Part replacement-for-tmuxinator. Part terminal-native Conductor. All you need
on a workspace's machine is `tmux` and your agent CLI.

---

## Why

GUI tools like Conductor are great, but if you live in the terminal and have
spare compute (a Mac mini in the closet, a Linux box, a workstation),
you want to *spread* your agents across them — without standing up a
Kubernetes cluster.

Shelbi gives you:

- **One orchestrator, many workspaces.** Talk to a single agent in chat. It
  dispatches tasks across machines and tracks progress for you.
- **Run anywhere tmux runs.** Hub-side: Linux or macOS. Workspaces: any
  machine reachable by `ssh` with `tmux` + your agent CLI installed.
- **Worktree isolation per task.** Each task runs on its own git branch in
  its own worktree. Review, merge, archive — independently.
- **No daemons, no servers.** Just `ssh`, `tmux`, `git`, and your agent CLI.
- **Pluggable runners.** Claude Code, Codex, or any CLI you can drive
  interactively — declared per project.
- **Markdown state, not a database.** Every task, log, and workspace status is
  a markdown or YAML file. Grep it, version-control it, read it from your
  editor.

---

## Install

On Ubuntu, install Shelbi from the signed APT repository:

```bash
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://apt.shelbi.dev/shelbi-archive-keyring.gpg \
  | sudo tee /etc/apt/keyrings/shelbi-archive-keyring.gpg >/dev/null

echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/shelbi-archive-keyring.gpg] https://apt.shelbi.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/shelbi.list >/dev/null

sudo apt update
sudo apt install shelbi
```

The published key fingerprint is available at
`https://apt.shelbi.dev/shelbi-archive-keyring.fingerprint`.

For contributor/source installs, clone and run `./scripts/install.sh` — it
builds with `cargo build --release` and drops the binary at
`$HOME/bin/shelbi` (override with
`SHELBI_INSTALL_PATH=/somewhere/else`). `cargo install shelbi` will land once
the first crate is published.

```bash
git clone https://github.com/jlong/shelbi.git
cd shelbi
./scripts/install.sh
```

On macOS the script re-signs the copied binary ad-hoc — without that step
the kernel SIGKILLs the process on first exec with no useful error. Linux
needs nothing extra. Re-run the script any time you pull updates to
rebuild and reinstall in one shot.

Confirm the install:

```bash
shelbi --version
```

You'll also want `tmux` (≥ 3.2) on the hub and on any remote workspace
machines, plus whichever agent CLI you intend to run (`claude`, `codex`, …).

---

## First run

Run `shelbi` with no projects configured and it drops you into the wizard.
The wizard walks one or more projects through setup. It is idempotent —
re-running it skips anything already on disk.

The wizard asks up front where the project's config should live:

- **`global`** — everything stays under `~/.shelbi/projects/<name>.yaml`
  on your machine. Nothing lands in the repo. Right for solo work.
- **`in-repo`** — the shared half of the config (workflows, agent
  prompts, runner settings) is committed at `<repo>/.shelbi/project.yaml`
  so teammates get it on `git clone`; only per-machine bits (your
  machines and workspace pool) stay under `~/.shelbi/`. Right for teams.

Global is the default; you can migrate to in-repo later with
`shelbi project migrate-to-in-repo`. Full details in
[docs — Config modes](site/content/docs/concepts/config-modes.mdx).

```
$ shelbi
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
? Agent runner (used by every workspace): claude
  (memory: 64 GB → recommended 5 workspaces per machine — configurable later)
? Workspace count per machine: 4
? Workspace naming style: phonetic (alpha, bravo, charlie, …)
? Orchestrator runner: claude
✓ Project myapp created (8 workspaces: alpha, bravo, charlie, delta, echo, foxtrot, golf, hotel).

? Set up another project? No
```

What the wizard auto-fills from your environment:

- **Project name** — current directory's basename.
- **Path to the repo** — current working directory.
- **Default branch** — `origin/HEAD` (falls back to `main`).
- **GitHub repo URL** — `git remote get-url origin`.
- **Hub work directory** — same as the repo path.
- **Workspace count** — heuristic from total RAM, budgeting ~10 GB per
  workspace on a single-machine setup and ~12 GB when work is spread
  across multiple machines. Clamped to `[1, 16]`.
- **Workspace naming style** — choose between phonetic alphabet, Greek
  letters, or Toy Story characters. Workspaces are laid out machine by
  machine, so the first N names land on the hub, the next N on the first
  remote, and so on.

After the wizard, shelbi launches the TUI for the project it just created.
Subsequent invocations from inside the repo auto-select it: shelbi matches
the current directory against each registered project's `work_dir` in
`~/.shelbi/projects/*.yaml` — no marker file is left in your repo.

---

## Returning to shelbi

`shelbi` with no arguments dispatches based on what it finds:

- **No projects on disk** → onboarding wizard (above).
- **One project** → launches its TUI directly.
- **Two or more** → fuzzy project picker (type to filter, most-recently-
  launched at the top, `+ Add a new project` at the bottom).
- **Inside a registered project's `work_dir` (or a subdirectory)** → that
  project always wins, matched against `~/.shelbi/projects/*.yaml`.

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
watches the todo column: as soon as a workspace frees up, it picks the
highest-priority unblocked card, checks it out on a fresh branch in the
workspace's worktree, and starts the runner with the task prompt. The card
moves to **in_progress** automatically.

When a workspace finishes, its card lands in **review** and the workspace
stops. The sidebar surfaces a `Ready for Review` list with a cyan `✓`
next to each title — that's your queue. Click one (or press Enter) and
shelbi checks the branch out into the project's working directory on
the machine that ran the task, then spawns a fresh runner pane there
for you to interrogate the diff. Approve → merge into the default
branch (or push and open a PR with `--pr`). Done.

The orchestrator handles dispatch and progress tracking. You handle
direction and review. Nothing in the middle needs CLI ceremony.

---

## TUI tour

Two-pane layout. Borderless sidebar on the left — project name as a
strong header, then a 2-item nav (Chat, Tasks), then live inline lists
of declared workspaces (grouped by machine) and tasks waiting on you
(`— Ready for Review —`). Content view on the right is a real tmux
pane: the orchestrator, a workspace, or one of the built-in views.

```
 myapp                  Chat — Orchestrator
                        you: fix the login bug on safari,
 💬 Chat                  send it to delta.
 📋 Tasks
                        Orchestrator: ✓ dispatched to delta.
 — hub —                  workspace: delta
 ⏵ alpha                  branch: shelbi/fix-login-bug
 💬 bravo
 — devbox —             you: how's it going?
 · charlie
 ✓ delta                Orchestrator: editing tests now — should
 · echo                   be done in a couple minutes.
                          Ctrl+P to jump to it.
 — Ready for Review —
 ✓ fix-login     delta   > _
 ✓ csv-export    charlie
```

Workspace state badges in the sidebar:

| Badge | Meaning |
|---|---|
| `⏵` | working — agent actively running a turn |
| `💬` | awaiting input — finished a turn, sitting at the prompt |
| `⚠` | awaiting permission — showing a permission dialog |
| `✓` | review-ready — task moved to the review column |
| `·` | idle — no in-flight task assigned |

`Ctrl+P` opens a fuzzy command palette as a tmux popup — for switching
projects, jumping to a workspace, swapping the right pane to Tasks /
Machines / Review, or triggering actions. Same convention as VS Code /
JetBrains / Telescope.

`Enter` focuses the highlighted row: a nav item swaps the right pane to
that view; a workspace switches to its tmux window (or opens an SSH proxy
window for a remote workspace); a review row checks out the branch and
spawns the review pane.

The built-in views on the right pane:

- **Chat** — the orchestrator agent running in window 1. This is where you
  talk to shelbi.
- **Tasks** — a Kanban board for the task's workflow. The default
  workflow ships with Backlog / Todo / In Progress / Review / Done;
  custom workflows can rename or split those into any sequence of
  statuses. `h/l` step columns, `j/k` step rows, `Enter` / `Space`
  opens the card, `H/L` moves the selected card to the next column,
  `K/J` reorders within a column, `r` refreshes.
- **Machines** — declared machines + their workspace assignments and SSH
  health.
- **Review** — a ratatui list of every task currently in the review
  column; activating a row triggers the same checkout flow as clicking
  the inline `Ready for Review` entries in the sidebar.

---

## Concepts

| Concept | What it is |
|---|---|
| **Project** | A git repo + the machines it can run on + the workspace pool + the agent runners available. Declared in `~/.shelbi/projects/<name>.yaml`. |
| **Machine** | A host. Either `local` (the hub) or `ssh` with a host and a working directory. |
| **Agent runner** | A pluggable CLI command shelbi knows how to launch (`claude`, `codex`, …). |
| **Workspace** | A named, persistent slot pinned to a machine — one tmux pane + one git worktree. Picks up the next ready task and runs it in isolation. |
| **Agent** | A role: a system prompt + skill set (orchestrator, developer, QA, security review). Loaded into a workspace per task. Lives in `agents/<name>/`. |
| **Orchestrator** | The agent in window 1 you talk to. Creates tasks, dispatches them to workspaces via the `shelbi` CLI. |
| **Workflow** | A YAML schema that declares the statuses a task moves through and what happens on each transition. The default workflow is the canonical Backlog → Todo → In Progress → Review → Done; projects can drop additional workflow YAMLs alongside it. |
| **Task** | A markdown file in `~/.shelbi/projects/<name>/tasks/`. Moves through its workflow's statuses (Backlog → Todo → In Progress → Review → Done in the default workflow). |

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
                 │   for each workspace:
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
- Each **workspace** runs `claude` (or codex, etc.) interactively in a
  tmux pane — locally on the hub, or in a tmux session on a remote machine
  reached over SSH. Which **agent** (role + prompt) runs there is chosen
  per task.
- shelbi drives workspaces with `tmux send-keys` + `capture-pane`,
  transparently prefixed with `ssh host --` for remotes. A poller watches
  each workspace's pane title for `shelbi:<state>` markers and writes them to
  `~/.shelbi/workspaces/<name>/status.yaml`, which is what drives the sidebar
  badges.
- Project state lives in `~/.shelbi/projects/<name>/` — tasks, agents,
  workspace settings, and a YAML config. Workspace state lives in
  `~/.shelbi/workspaces/<name>/`. Everything is plain text.

---

## Common workflows

**Dispatch to a workspace explicitly without waiting for the orchestrator.**

```bash
shelbi task start fix-login --worker delta
```

**Switch projects from anywhere.**

```bash
shelbi -p sideproj    # or just `shelbi` and pick from the fuzzy list
```

**Add a remote machine to a project that already exists.** Edit
`~/.shelbi/projects/<name>.yaml` — append a machine to the `machines:`
list and a workspace per slot to `workspaces:`. Reload the dashboard:

```bash
shelbi reload
```

**Pick up a fresh binary after `./scripts/install.sh`.** The orchestrator
and workspace panes re-shell into shelbi on every call, so they pick up
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
workspaces:
  - { name: alpha,    machine: hub,    runner: claude }
  - { name: bravo,    machine: hub,    runner: claude }
  - { name: charlie,  machine: devbox, runner: claude }
  - { name: delta,    machine: devbox, runner: claude }
```

To boot the orchestrator with Codex, set `orchestrator.runner: codex`
and keep `agent_runners.codex` declared. Shelbi launches Codex as the
configured command plus flags, then adds an initial startup prompt that
embeds the rendered orchestrator instructions. Claude-specific permission
and `--append-system-prompt` flags are only added for Claude.

### Task (auto-generated by the orchestrator)

```markdown
---
id: fix-login-bug
title: Fix login bug on Safari
column: in_progress
priority: 0
assigned_to: delta
branch: shelbi/fix-login-bug
depends_on: []
created_at: 2026-06-18T14:22:11Z
updated_at: 2026-06-19T09:14:02Z
---

# Task

Fix the login bug on Safari — cookie domain mismatch breaks SSO redirect.

## Notes

- read `src/auth/session.ts`
- editing tests…
```

`depends_on` is a list of other task IDs. A task is **blocked** while any of
them are not in `done`. The orchestrator skips blocked todo items when
auto-dispatching; the Kanban shows them with a 🔒 badge.

---

## Contributing

shelbi is a Cargo workspace under `crates/`:

| Crate | Purpose |
|---|---|
| `shelbi-cli` | The `shelbi` binary, subcommands, wizard. |
| `shelbi-core` | Domain model — Project, Task, Machine, Workspace, Agent, Workflow. |
| `shelbi-state` | Filesystem layout — load/save YAML and markdown under `~/.shelbi/`. |
| `shelbi-tui` | Ratatui sidebar, Kanban, review queue. |
| `shelbi-tmux` | tmux session/window/pane helpers. |
| `shelbi-ssh` | SSH-prefixed command execution for remote workspaces. |
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
