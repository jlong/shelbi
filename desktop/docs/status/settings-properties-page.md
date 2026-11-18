# settings-properties-page: Settings ▸ Properties

## What landed

A **Properties** page, first in Settings' project section, in the desktop
app and the TUI: the project YAML's own settings that no other project page
covers. The section now follows the Figma `Navigation` State=Settings
variant (file `pu3hxHgWoCDPCzJ4u6zTQK`, node 2252:27532; the project nav is
2252:27767): Properties, Orchestrator, Agents, Workspaces, Workflows.

```
│ My project                │   Properties
│ ☰ Properties           ⌃4 │   Settings for this project, stored in <file>.
│ 💬 Orchestrator        ⌃5 │   Name / Repository / Default branch
│ 🤖 Agents              ⌃6 │   Default workflow ⌄
│ 📁 Workspaces          ⌃7 │   › Advanced: the two poll intervals
│ ▥ Workflows            ⌃8 │   Issue tracker section (linear-settings-ui)
```

- **Model** (`shelbi_state::project_properties`, carried by
  `WorkflowConfig::properties` / `set_properties`): reads Name (`name:`, or
  the legacy `display_name:`), Repository (`github_url:`), Default branch,
  `workspace_poll_interval_secs` and `github_reconcile_interval_secs` out of
  the YAML, checks them, and patches only the keys that changed into the
  same plan that writes `default_workflow:` and `orchestrator.awareness:`.
  An in-repo project's user-local `local.yaml` is read
  (`WorkflowFiles::local`) but never written. Before planning a
  properties write the candidate files are parsed as `shelbi config lint`
  would (`Project::from_yaml_str` / `from_split_yaml_str`, then
  `validate_workspaces`): nothing invalid is written.
- **Form** (`shelbi_app::settings::properties::PropertiesForm`): the drafts,
  per-field errors, placeholders and help, the Default workflow picker,
  Advanced (it opens itself on an error inside it), and one save at
  a time (a commit made mid-save is handed back by `saved_result`).
- **Desktop** (`views/properties.rs`): text fields commit on Return or blur;
  the Default workflow is a menu like the workspace dialog's Machine picker.
  A save that changed the label
  refreshes the `Catalog` so every tab shows it.
- **TUI** (`shell/settings/properties.rs`): label-over-box rows; Enter types
  a field (Enter saves, Esc puts it back), ←/→ step the Default workflow.
  Leaving mid-typing goes through the leave prompt.
- **Numbers**: `SettingsPage::shortcut()` counts `SECTIONS` order (General 1
  to Workflows 8). The desktop opens pages with ⌃N; ⌘N is always the tabs'
  now. The TUI's digit keys follow the same numbers.
- **Icons**: Properties is Heroicons `list-bullet`; Agents is John's
  `robot` (`assets/icons/figma/robot.svg`, stroke `currentColor`).

## Decisions

- No Editor field (`properties-drop-editor`). The page first shipped one
  for the project's `editor:` key, but nothing in Shelbi reads that key:
  every editor launch (Edit in…, an issue body's Open in editor, Cmd+click
  on `path:line`) uses Settings ▸ General's editor. The field was removed
  from both clients; `Project::editor` and its YAML key still load, and a
  Properties save neither writes nor drops it.
- Repository edits `github_url:`; `repo:` is the checkout path and stays
  out of the page.
- No Properties page frame exists in Figma: the page uses the General
  page's controls, and Advanced uses the sidebar sections' chevron glyph.
- Integrations (in the frame) is not added; the section header stays the
  project-name label.

## Verified

- `shelbi-state`: the properties module and `WorkflowConfig` round trips
  (global keeps comments, a bad value or a file the loader refuses writes
  nothing, a project's `editor:` survives a save untouched, an in-repo save
  writes only the shared file, and a far daemon's `validate` takes the
  plan).
- `shelbi-app`: the form (load, edit, validate, save round trip keeping
  comments, queued saves, the workflow picker, a failed write, an `editor:` kept) and the nav order and
  numbers.
- Desktop: the page through the window (⌃4, the rows in order, a rename
  keeping comments, a refused interval), ⌃N / ⌘N, the ↑/↓ walk, the nav
  icons. TUI: the page through the real event path at 110 columns, the
  stops and digits.
- `--visual-figma` with `SHELBI_VISUAL_ONLY=settings`:
  `settings-properties-{dark,light}.png` and
  `state-properties-advanced-{dark,light}.png`.
