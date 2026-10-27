# dt-add-project-wizard: the Add Project wizard

## The change

Add Project in the desktop app was a one-form overlay over the project
window (name, repo path, a config radio row). It is now a wizard in a
window of its own, drawn to the Figma frames "01 · Project · Choose
folder" (2481:8574), "01 · Project · Folder selected" (2481:8810), "02 ·
Settings · In the project" (2483:8653) and "02 · Settings · Outside the
project" (2483:8897). This task owns the window and screens 01 and 02;
the creation checklist and the completion screen are separate tasks.

```
 ● ● ●                                         ● ● ●
              Add project                           Where should settings live?
  Select the folder that contains the …        Choose how Shelbi stores settings, …

 ┌──────────────────────────────────────┐      ┌──────────────────────────────────────┐
 │ My project                           │      │ ◉ In the project                     │
 │ Git repository detected. Shelbi …    │      │   Keep settings with your code …     │
 │ [~/Projects/my-project      ][Browse]│      │   my-project/.shelbi                 │
 └──────────────────────────────────────┘      └──────────────────────────────────────┘
                                               ┌──────────────────────────────────────┐
                                               │ ○ Outside the project                │
                                               │   ~/.shelbi/projects/my-project      │
                                               └──────────────────────────────────────┘
 Step 1 of 3              [Back] [Continue]    Step 2 of 3        [Back] [Create Project]
```

Every entry point opens it: the sidebar switcher's Add project…, the
palette and File ▸ Add Project… (`CommandKind::AddProject`), and the
empty-project chooser's Add Project… row all reach
`HostCall::OpenAddProject`, which now opens this window
(`views::add_project::show`). The old overlay
(`views/overlay/add_project.rs`, `ActiveOverlay::AddProject`, the window's
`detect_add_project` / `submit_add_project`) is gone. A launch with no
project at all, which used to log an error and quit, opens the wizard
instead.

## How it is built

- **`shelbi_app::add_project_wizard`** (toolkit-free): the screens, the
  values (path text, the name typed after a clash, the config mode), the
  Tab order per screen, what each key does, and the folder check's
  bookkeeping. `check_due` / `begin_check` / `finish_check` key each check
  by the text it was for, so an answer for a path the user has since
  changed is dropped, and Continue waits for an answer for the current
  text. Create Project returns one `CreateRequest` and moves to
  `Screen::Creating`, where Return, Space, Back and Esc do nothing, so it
  can't be sent twice.
- **`project_create::check_folder`** (`shelbi-orchestrator`): the folder
  check, built from the same pieces `validate_add_project` uses
  (`absolutize`, `validate_root`, `project_name_from_root`,
  `normalize_project_name`, `project_name_collides`) plus a worktree
  test (`.git` is a file) and the real storage paths
  (`<folder>/.shelbi`, `shelbi_state::projects_dir()/<id>`). It writes
  nothing. Creation still goes through `project_create::add_project`
  (which validates again), so nothing about the scaffolder forks.
- **`views::add_project`** (desktop): the window (`about::options`, 800 ×
  580, not resizable, the traffic lights over the content), the paint,
  the native folder picker, and the off-thread work through an `Engine`
  (check, create) a test swaps for a fake. Success runs
  `app::open_added_project`: the window the wizard came from switches to
  the new project (with the "Created project" flash), else a new window
  opens on it; the project window opens before the wizard closes, so a
  first launch never runs out of windows. A failed create returns to 02
  with the reason under the cards.

## Decisions

- **Its own window, not an overlay.** The frames are 800 × 580 windows
  with the traffic lights over the content, and the first launch has no
  project window to put an overlay on. One wizard at a time: asking again
  raises the open one.
- **The name is the folder's.** No name step. Only when the folder's id
  is already registered (or its name has no letters or digits) does a
  "Project name" field appear under the path, seeded with the folder's
  name; a different name is checked like a path change. The project's
  display name is the folder's own name (`My Project`), its id the slug.
- **First-screen Back.** From a project window it closes the wizard
  (that window is untouched). On the first launch it is disabled: nothing
  comes before it yet (the Orchestrator and Kanban help screens will).
  Esc and ⌘W close the wizard either way (on the first launch that quits).
- **Default card: Outside the project** (`ConfigMode::Global`), the old
  form's default. The CLI's team-signal heuristic lives in `shelbi-cli`
  and wasn't moved for this.
- **"In the project" path** reads `<folder>/.shelbi`, as the frame
  abbreviates it; "Outside the project" is the real
  `projects_dir()/<id>`, `~`-shortened.
- **Copy.** The two 02 frames differ on the second card ("on this
  machine" / "on this Mac"); the app says "on this Mac". Status lines
  for the undrawn cases (a worktree, no Git, missing, a file, a clash)
  are this task's.
- **Keys.** Tab / ⇧Tab cycle the screen's enabled controls; Return is the
  enabled primary action from anywhere; Space presses the focused button
  or card; the arrows move between the cards; Esc cancels. The wizard
  takes these through a window-scoped keystroke interceptor, ahead of the
  text field's own Tab / Return / Esc bindings. The focus ring shows once
  the keyboard moves focus (as macOS draws it), so the frames' resting
  state has none. gpui has no public accessibility API, so the labels are
  the controls' visible text.
- **Colors.** The box and the unselected card fill with `surface/depth-10`
  (the palette's `toolbar` role; Figma's #acb4cf at 10%); the selected
  card is `primary_fill` edged in `focus_ring`; the radios are drawn
  rings in `primary_label` (on) and `gray` (off). Title text is a new
  `Type::Title` (`Desktop/Title`, 24 / 32 semibold).

## Checks

- `shelbi_app::add_project_wizard` unit tests: navigation and retained
  values, picker cancel, stale checks, clashes, both modes, one create,
  failure, first-launch Back, Tab order.
- `project_create` tests (temporary `SHELBI_HOME`): `check_folder` for a
  repo with spaces in its path, a worktree, a plain folder, a file, a
  missing path, a normalized-name clash, a typed name past it; an in-repo
  create in a worktree.
- Desktop: `views::add_project::tests` (gpui windows over a fake engine)
  and `app::window_tests::add_project_opens_the_wizard_window_once`; the
  chooser test now clicks its Add row.
- `--visual-figma` renders `add-project-folder`, `-folder-selected`,
  `-settings-in-repo` and `-settings-global` (`SHELBI_VISUAL_ONLY=
  add-project`). Against the frames (Dark, 1x): 0.99%, 1.53%, 3.37%,
  3.07%, all text: the Geist build Figma sets in, and the frames'
  placeholder `acme` path.
