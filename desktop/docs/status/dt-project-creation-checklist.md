# dt-project-creation-checklist: the creation checklist (screen 03)

## The change

Create Project used to leave the Add Project wizard on a "Creating
project…" placeholder until the engine returned, then switch straight to
the new project. It now shows screen 03, drawn to "03 · Project · Writing
config" (2484:8673), "Creating agents" (2514:45915) and "Adding
workflows" (2514:45972): a checklist the real creation engine drives.

```
 ● ● ●
            Setting your project up for Shelbi


   (✓)  Writing Shelbi config
   ( ◜) Creating agents          ← one row runs at a time
   ( )  Adding workflows


 (no footer while it runs)
```

A failure keeps the checked rows, marks the one that failed (an x on the
Danger fill, the reason under it) and offers Try Again. Success moves on
to a "Your project is ready" screen with what the engine warned about
(the commit guard's disclosure) and Open Project; the project opens only
when the user presses it. That screen is a stand-in for
`dt-wizard-workflow-summary`, which replaces its content with the
workflow overview.

## How it is built

- **Engine (`shelbi-orchestrator::project_create`).** `ScaffoldReporter`
  gained `stage_started` / `stage_finished` / `stage_failed` with no-op
  defaults, so the CLI's reporter (and its byte-identical `shelbi init`
  output) is unchanged. `scaffold_project` runs as three
  `ScaffoldStage`s, the same operations in the same order as before:
  - **Config**: the scaffold lock, onboarding seeds, the registration
    (flat or split), the committed in-repo half, the workspace-settings
    template.
  - **Agents**: `self_heal_default_agents`.
  - **Workflows**: the Task / Subtask workflows and statuses, then the
    rest of the scaffold (Zen policy, PR template, shared preamble,
    commit guard). It is the last row, so its check means the project is
    complete.
  A stage is reported finished only after its writes succeed; an error
  reports `stage_failed` and nothing after it runs.
- **Retry.** `add_project` takes an `Attempt`. `New` keeps the old
  collision check. `Retry` accepts an existing registration only when it
  is the one this request publishes: the registration file for the
  request's mode (flat `projects/<id>.yaml` for global, split
  `<id>/local.yaml` in-repo, never both) whose hub machine works in the
  request's folder (read from the file, so a registration whose in-repo
  half was never written still answers). Any other registration under the
  id is still "a project already lives at …". Every scaffold step is
  idempotent and leaves existing files alone, so a retry fills in what is
  missing and keeps customized files.
- **Model (`shelbi_app::add_project_wizard`).** `Stage` (the rows and
  their labels), `RowState`, the progress (done stages, the one active,
  the failure, warnings), `retry()`, `creation_succeeded()` and
  `Screen::Created` with the `Created` identity (slug, name, warnings).
  Events outside a running creation are dropped. Warnings are reflowed
  from the CLI's `shelbi:`-prefixed terminal lines into a paragraph
  (`prose`).
- **Desktop (`views::add_project`).** The engine runs on the background
  executor with a reporter that sends `Report`s over an unbounded
  `async-channel`; a foreground task applies each through the view's weak
  handle, then the result. A run counter drops anything from an earlier
  run, and a closed window stops the loop (the background work finishes
  on its own; nothing restarts it, and an abandoned creation opens
  nothing). Notes go to the log (`shelbi::add_project`, debug), warnings
  to the log and the wizard.
- **Spinner (`elements::spinner`).** The Figma Spinner: a 270° ring, 4pt
  thick, its angular gradient (transparent at the tail, opaque from 70%)
  painted as 54 short arcs, a 4pt dot at the head, one turn a second.
  Under Reduce Motion it holds at its drawn angle.
- **Colors.** Pending: `background` (Figma's `#acb4cf` at 18%). Done:
  `success_fill` with the check in `icon_green`. Running: the new
  `progress` role (`progress/default`: `#62cdf6` Dark, `#f21478` Light).
  Labels: `muted`, then `text_loud` medium while running, `foreground`
  once done. Failed (not in Figma): `danger_fill` with an x in `error`.

## Decisions

- **The tail of the scaffold belongs to Adding workflows.** The Zen
  policy, PR template, shared preamble and commit guard run after the
  workflows; reordering them would change `shelbi init`'s output, so the
  last row covers them rather than a fourth, unshown stage (which would
  leave no row active while it ran).
- **Try Again starts the checklist over.** The engine re-confirms every
  stage on a retry (the ones that finished before go quickly), so rows
  uncheck and check again rather than claiming work the retry hasn't
  verified.
- **A failure stays on 03.** There is no Back from a failure: the
  registration may already exist, and a different name or mode would
  strand it. Esc / ⌘W close once nothing runs; while the engine runs,
  Esc waits.
- **The created screen waits for Open Project**, per the task (the
  Orchestrator doesn't open before the summary is acknowledged). Closing
  the wizard there leaves the project created, in the switcher.

## Checks

- `project_create`: stage order with a reporter that asserts each
  stage's files are on disk when it is checked (and the next stage's
  aren't when it starts), in both modes; a failure at each stage in both
  modes (a file where the stage needs a folder), a plain second request
  clashing with the partial project, a retry finishing it with one
  registration of the right shape and customized files kept; a retry
  never taking another folder's or another mode's registration.
- `add_project_wizard`: one active row, checks only on finish, success to
  Created and Open, failure keeping rows and retrying the same request,
  a failure before any stage, warning prose.
- Desktop: scripted engines through the real channel (every row checked,
  warnings carried, a failed stage and Try Again, closing mid-run), and
  each row state drawn from reports.
- `--visual-figma` renders `add-project-writing-config`,
  `-creating-agents`, `-adding-workflows` (plus `-failed` and `-created`,
  which have no frames). Against the frames (Dark, 1x): 1.17%, 1.21%,
  1.23%, all in the title (the Geist build Figma sets in) and the
  traffic lights, which offscreen windows don't draw; the status glyphs
  and rows land on the frames' pixels.
