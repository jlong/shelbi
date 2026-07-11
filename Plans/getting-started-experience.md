# Getting Started Experience: the 60-second wizard

## North star

`brew install jlong/shelbi/shelbi && shelbi` in any repo puts you in a working
dashboard, with the orchestrator greeting you about your own code, in under 60
seconds. One question on the happy path. Two at most.

The wizard today asks 12 questions (name, repo path, branch, GitHub URL, hub
name, hub workdir, remote machines, agent runner, workspace count, naming
style, orchestrator runner, another project). Every one of them already has a
probed or obvious default. A wizard that asks you to confirm 12 defaults is
not a wizard; it is a form. Flip the model: Shelbi detects everything, shows
its plan on one card, and asks a single question: "look right?"

## Principles

1. **Detect, don't ask.** Anything derivable from the environment (git, PATH,
   cwd, CPU count) is derived, silently. Questions are reserved for genuine
   ambiguity.
2. **Confirm, don't interrogate.** One summary card replaces the prompt
   sequence. The user reads a plan, not a questionnaire.
3. **Onboarding ends at the first task, not at the config file.** The first
   minute inside the dashboard is part of setup. The orchestrator's opening
   message is the real welcome screen.
4. **Every default is editable later, and we say so.** The palette's new
   "Edit Project Settings / Edit Workflows / Edit Zen Mode" entries (hidden
   until you type) are the escape hatch. The card footer teaches this in one
   line, so accepting defaults never feels like a trap.
5. **The full wizard survives as the escape hatch,** not the default. Press
   `c` on the card to customize; everything asked today remains askable.

## The flow

### 1. Preflight (0 questions, ~2 seconds)

On first run, after the banner, a live checklist renders as detection happens.
Each line appears with a check as it resolves, top to bottom, fast enough to
feel instant but visible enough to build trust:

```
  ✓ git repo            ~/Workspaces/shaft
  ✓ default branch      main
  ✓ remote              github.com/jlong/shaft
  ✓ agent               claude 2.1 on PATH
  ✓ tmux                3.5a
  ✓ machine             10 cores, recommending 4 workspaces
```

Detection sources, all existing code or trivial probes:
- repo, branch, remote: `GitDefaults::probe` (already exists)
- project name: `wizard_default_project_name` (already normalized)
- agent runner: probe PATH for `claude` and `codex`; version via `--version`
- tmux: version probe (we already hard-require it)
- workspace count: `workspace_count_recommendation` (already exists)

### 2. The plan card (the one question)

One boxed card that is simultaneously the config summary and the product
pitch. The user sees what they are getting before they commit to anything:

```
  ┌─ shaft ──────────────────────────────────────────────────┐
  │                                                          │
  │  repo        ~/Workspaces/shaft (main)                   │
  │  github      github.com/jlong/shaft                      │
  │  agent       claude                                      │
  │  workspaces  alpha bravo charlie delta + review (hub)    │
  │  workflows   task (branch → PR → review) · subtask       │
  │  agents      orchestrator · developer · review           │
  │              (+ qa, security, adversarial, opt-in)       │
  │                                                          │
  │  Everything above is editable later: Ctrl+Space → "Edit" │
  └──────────────────────────────────────────────────────────┘

  Enter launch    c customize    q quit
```

- **Enter** writes the project YAML, scaffolds workflows/agents/statuses
  (all existing scaffold code), and launches the dashboard. This is the
  single question of the happy path.
- **c** drops into the current full wizard with every field pre-filled from
  the detected plan, so customizing starts from the plan rather than from
  scratch.
- Remote machines are gone from first-run entirely. They move to
  `shelbi machine add` and a palette entry. Nobody needs a devbox in the
  first 60 seconds, and the remote loop is the single biggest source of
  wizard friction today.

### 3. The second question (only when the world is ambiguous)

The agent runner is the one choice we cannot always infer:
- exactly one of claude/codex on PATH: use it, zero questions
- both on PATH: one select, "Which agent? (claude / codex)"
- neither: not a question, a friendly stop. Install instructions for both,
  then exit 1. Do not scaffold a project that cannot run.

The orchestrator runner silently follows the agent runner (that is already
the default cursor today). Naming preset silently defaults to phonetic. Both
are implied by the card and editable later.

### 4. Landing (the wow moment)

The wizard's job ends with the user doing something, not reading a success
message. On launch:

- **The board seeds one task**: "Welcome to Shelbi", in backlog, whose body
  is a 10-line orientation (promote me to Todo and watch a workspace pick me
  up; Ctrl+P for the palette; this card is safe to delete). The empty-board
  cold start is replaced by a board that demonstrates itself.
- **The orchestrator's first message is contextual.** Its first-run prompt
  injection tells it to glance at the repo (README title, last few commit
  subjects) and open with something like: "I see you're working on shaft, a
  CLI for X. Tell me what you want done and I'll write it up as a task and
  dispatch it." The user's first impression is an agent that already read
  the room, not a blinking cursor.
- **The status line hints once**: "Ctrl+P palette · type E to edit settings"
  on first launch only (a `first_run_seen` flag in global state, same
  pattern as `zen_intro_seen`).

### 5. Edge paths (each one question or zero)

- **Not a git repo**: one confirm, "Not a git repo. Initialize one here?
  (Y/n)". Yes runs `git init -b main` and continues; no exits with a hint.
- **No tmux**: friendly stop with the brew/apt one-liner. Exit, don't limp.
- **`~/.shelbi` exists but this repo is unknown**: same preflight + card,
  framed as adding a project rather than first run. `shelbi project add`
  becomes this exact flow.
- **Re-running in a configured repo**: no wizard, straight to the dashboard
  (today's behavior, unchanged).

### 6. Non-interactive path

`shelbi init -y` accepts the full detected plan with zero prompts (errors if
the runner is ambiguous; `--runner claude|codex` resolves it). Flags mirror
the card fields for scripts and CI. This also gives the docs a one-line
"try Shelbi" snippet that cannot stall on a prompt.

## What this deletes

- The 12-prompt sequence as the default path (survives behind `c`).
- The remote-machine loop from first run (moves to `shelbi machine add`).
- The hub name / hub workdir questions entirely, even from the custom path
  (hub + repo path; still editable in YAML).
- The trailing "Set up another project?" confirm (projects are added by
  running `shelbi` in another repo, which is how people actually do it).

## Success criteria

- Happy path: exactly 1 interaction (Enter on the card). Ambiguous-runner
  path: 2. Everything else: 0 questions, it just proceeds.
- Fresh machine to dashboard with greeted orchestrator: under 60 seconds,
  dominated by `brew install`, not by Shelbi.
- The launch lands on a board with a Welcome card and a contextual greeting,
  never an empty screen.
- A screen recording of the full flow makes a compelling 30-second GIF for
  the site homepage. If the recording is boring, the flow isn't done.

## Implementation phases (task breakdown seeds)

1. **Preflight + plan card**: detection assembly, card render, Enter/c/q
   loop, runner ambiguity question, edge paths (git init, no tmux, no
   runner). Reuses `GitDefaults`, `wizard_default_project_name`,
   `workspace_count_recommendation`, and all scaffold fns. The current
   wizard becomes `customize_from(plan)`.
2. **Welcome task seed + first-run status-line hint**: scaffold writes the
   welcome task; `first_run_seen` global flag gates the hint.
3. **Contextual orchestrator greeting**: first-run prompt injection for the
   orchestrator (repo glance + opening move). Careful: inject only on the
   first launch of a project, not every reload.
4. **`shelbi init -y` + flags**: non-interactive path, docs snippet update.
5. **Site/docs refresh**: getting-started guide rewritten around the new
   flow, homepage GIF re-recorded.

Phases 1 and 4 are the core; 2 and 3 are the wow; 5 trails the release.