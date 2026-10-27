# dt-wizard-workflow-summary: the workflow summary (screen 04)

## The change

Once the creation engine has created the project, the Add Project wizard
used to show a stand-in "Your project is ready" screen (the checked list
and Open Project). It now shows screen 04, "04 · Workflows · Project
ready" (2484:8897): the two workflows the project was created with, then
Okay opens the project on its Orchestrator.

```
 ● ● ●
                         You’re all set!
     Two workflows have been added to your project. Workflows
               control how work gets done with Shelbi.

  ┌──────────────────────────────────────────────────────────────┐
  │ Task [Default]                                               │
  │ Work on a feature or fix, review the result, then merge it   │
  │ into your project.                                           │
  │ ◌ Backlog → ○ To do → ◐ In progress → ◑ Review → ● Done      │
  └──────────────────────────────────────────────────────────────┘
  ┌──────────────────────────────────────────────────────────────┐
  │ Subtask                                                      │
  │ Break a task into smaller pieces. Completed work merges into │
  │ its parent task.                                             │
  │ ◌ Backlog → ○ To do → ◐ In progress → ● Done                 │
  └──────────────────────────────────────────────────────────────┘

  You can change workflows at any time in Workspace Settings.  [Okay]
```

## How it is built

- **Model (`shelbi_app::add_project_wizard`).** `created_workflows()`
  builds the cards from the definitions the scaffolder writes:
  `shelbi_core::task_workflow()` and `subtask_workflow()`, resolved
  against `default_project_statuses()` (the shipped `statuses.yaml`), so
  the names and categories are the ones on disk. The sequence drops the
  Archived status (Canceled, the way out). Task carries Default because it
  is the `default_workflow` `project_create` registers. The one-line
  summaries are the design's copy, keyed by workflow id (the shipped
  workflows' own `description:` fields are written for the YAML reader;
  changing them would be a shipped-default change needing a config-upgrade
  sniffer). Stock status names read in sentence case (`Todo` is "To do",
  `In Progress` "In progress"), as the board's headers read `TO DO`.
- **Okay is its own step.** `primary()` on 04 returns `Action::Open` and
  sets `opening`: Okay, Return and Esc do nothing until the host answers.
  `open_failed(reason)` keeps 04 with the reason and enables Okay again,
  which asks for the open again. Nothing on 04 can create the project a
  second time.
- **Desktop (`views::add_project`).** Okay re-reads the catalog off the UI
  thread (`Engine::refresh`, the app's `Catalog::reload`, so the switcher
  and first frame have the new project), then calls `open_added_project`
  at the app's top level, and closes the wizard only once that succeeded.
  While it runs Okay is drawn disabled with a 16pt spinner beside it
  (`spinner_sized`).
- **Opening (`app::open_added_project`).** It now returns a `Result`:
  - a project the catalog doesn't list (the refresh didn't see it) is an
    error, so the wizard can retry;
  - the catalog forgets any view state saved for the id (a project
    removed and added again under the same name), so it lands on the
    Orchestrator and a refresh doesn't bring the old state back;
  - a window already showing the project comes up (an earlier Okay got
    that far), else the window the wizard was opened from switches, else
    a new window opens (the first launch). So a repeated Okay never makes
    a second window or session.
  - `ShelbiWindow::project_added` drops Settings or a review that was open
    over the old project before the switch (a switch from Settings
    otherwise reopens Settings for the new project), and on a window
    already showing the project (a fresh window that read view state from
    disk) adopts the default view state: the Orchestrator.
  `open_window` / `open_new_window` return the window's handle (`None`
  when it couldn't open) for that.
- **In-repo projects in the catalog (`shelbi_state::list_projects`).** The
  switcher's list read only flat `projects/<id>.yaml` files, so a project
  registered in-repo (`projects/<id>/local.yaml` plus the committed half)
  never appeared in it, in either client. It now lists both shapes (the
  split read with `load_project_bare`, nothing migrated; the split wins
  over a stale flat file, as in `load_project`). Without it Okay could
  never open an in-repo project (the catalog wouldn't list it) and the
  switcher would lose it.

## Decisions

- **The engine's warnings stay on 04.** The revised frame has no place for
  them, but the commit guard is installed "with disclosure", and 04 is the
  last screen before the project opens, so the disclosure shows in muted
  Small type under the cards when the engine reported one.
- **The reason a failed open shows is the app's.** Opening has no engine
  error of its own; the two that can happen (the catalog doesn't list the
  project, no window could open) each have a sentence.
- **⌘W still closes 04 while it opens.** The open already asked for goes
  ahead (the user pressed Okay); the project stays created either way.
- **The step count stays off 04.** The frame has none.

## Checks

- `add_project_wizard`: 04 waits for Okay and opens once (Return, a click
  and Esc while opening do nothing), a failed open stays and retries the
  open only, the overview equals the shipped Task / Subtask workflows and
  statuses (ids, order, categories, labels), sentence-case labels.
- `projects`: `forget_view` clears and pins the default, `lists`.
- Desktop wizard: 04 draws both cards, every status, the badge, the note
  and Okay with focus, and no step count, Back or checklist; a failed
  creation never reaches 04; a failed open stays and Okay opens again with
  one creation; repeated presses while opening open once, with the
  spinner drawn.
- Desktop windows: Okay from a window left in Settings lands that window
  on the new project's Orchestrator with no new window and no stale view
  state; the first launch opens one window and a second Okay reuses it; a
  project the catalog lacks is an error that leaves the window alone.
- End to end, both modes: the real folder check, engine and catalog in a
  throwaway `SHELBI_HOME` and `git init` repo, driven folder → settings →
  checklist → 04 → Okay. The catalog lists the project when it opens, the
  project's `default_workflow` is `task`, each card's statuses (ids,
  categories, names) are the ones `load_workflow` reads back from disk,
  and the settings land in the chosen place.
- `shelbi_state`: `list_projects` lists a migrated in-repo project once,
  with the split's repo over a stale flat file.
- `--visual-figma` renders `add-project-ready` (the frame),
  `-ready-warned`, `-opening` and `-open-failed`.

## Not verified here

This was built on a Linux host: the desktop crate's 778 tests, the shared
crates' tests and clippy ran there (gpui's test platform; a type-check-only
`pkg-config` shim for the build scripts). The macOS-only pieces were not
run: the `visual-check` harness (`--visual-figma`, which needs macOS's
`VisualTestAppContext`), the pixel diff against the Figma frame (its
exports and the Figma MCP weren't reachable from this host), and a click
through the real app. Those need a Mac before merge.
