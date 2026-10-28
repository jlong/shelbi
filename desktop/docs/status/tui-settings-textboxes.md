# tui-settings-textboxes: one half-block textbox for every settings field

## The change

The TUI's settings fields drew as plain text, `│ ` gutters, `[text▏]`
brackets or a reversed run beside a left label column. They now share one
widget, drawn the way the sidebar's search box is: a `▄` row above, the text
on a solid fill, a `▀` row below, the label over the box (as the desktop's
Labeled Control reads) and the help or error under it.

```
Name
▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
 alpha█
▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
Workspace names are unique per project.
```

## The widget (`crates/shelbi-tui/src/textbox.rs`)

- `Textbox` (text, caret, placeholder, focused, disabled, invalid, a whole
  selected suggestion, left clipping for paths, a right trail such as `▾`
  or `read-only`) returns its three rows; the pages lay rows out as lines,
  so each page marks all three as the field's click target.
- States are fills, never borders: `input` at rest (the search box's
  fill), `input_focus` (cyan tinted) while it has the keyboard, typed in or
  not, `input_invalid` while refused, `input_disabled` (text muted) when it
  can't change. Focus wins over read-only so a read-only field the keyboard
  is on still shows it. The caret is a block in the strong text color; a
  value longer than the box scrolls to keep it in view (by cells, so wide
  characters scroll correctly); one not typed in is cut with an ellipsis,
  or keeps its end for a path.
- `area` wraps a multi-line field's rows in the same fill and caps;
  `caps` is shared with the sidebar search box.
- The four fills are new tokens in `shelbi_app::theme::Theme` (dark and
  light), with contrast tests: every text token clears 4.5:1 on each fill,
  the placeholder 3:1.

## Where it's used

- **General**: Command (the editor). Theme's label now sits over its
  segments too, so the page has one rhythm.
- **Workspace form**: Name, Machine (a picker in the same box with `▾`),
  Tags, Worktree location (read-only), Slot (optional). Labels and
  placeholders follow the desktop dialog.
- **Machine form**: Name and SSH host side by side when the form is wide
  enough (stacked otherwise, as the desktop sets them), Project directory,
  Machine tags, with the desktop's placeholders.
- **Both modals** share `shell/settings/modal_form.rs`: a field's three rows
  are its click target, a short terminal scrolls to the focused field's
  block, and the footer (a refusal, the buttons, the key hint) is pinned
  under the scrolling fields when they don't all fit.
- **Agents page**: the new agent's name, and the shared instructions editor
  (the `│ ` gutter replaced by the fill and caps).
- **Agent editor**: Name (disabled when locked), Description, Skill name,
  Description, Import skill, Custom model; the Instructions and SKILL.md
  areas; its pickers (Preferred runner, Model, Reasoning effort,
  Permissions) as label over a `▾` box.
- **Workflow editor** (Form): Name, Description, Initial status, a status's
  Workspace tags (typed in a field under its row), the transition Details
  (Target branch, Run commands under one label, Ready check, Ready timeout,
  URL), Base branch, Branch name, Merge strategy, Review mode and its
  fields, Zen settings, Required fields.
- **Builder panel**: Status, Owner, Agent, Workspace tags, From, To, and
  the same Details.
- **Workflows page**: New workflow and New status names, and a status
  rename (a field under its row).
- **Keybindings**: the search, with the desktop's placeholder.

Tables (`tui-settings-tables`) and the keybinding capture chip in the
bindings table are left as they are.

## Also fixed

- The agent and workflow editors took the first matching click target, so
  a click on an open menu could land on the page row under it; the last
  drawn now wins.
- Scrolling to the focus keeps a field's label and whole box in view
  (`settings::follow`, shared by the Form and the Builder's panel).
