# tab-drag-window-move: a tab drag reorders, the bar's empty space moves the window

## The problem

Dragging a project tab (`tab-drag-reorder`) moved the whole window instead.
The tab bar sits in the window's transparent titlebar
(`TitlebarOptions { appears_transparent: true }`), and AppKit treated a
press anywhere in the titlebar strip as a window drag before gpui's own
drag could start: the window followed the pointer and no ghost showed.

## How gpui decides (gpui-pre 0.3.9, macOS)

- AppKit asks the content view's `_opaqueRectForWindowMoveWhenInTitlebar`
  which part of the titlebar strip is app content. gpui answers with an
  empty rect by default (AppKit drags the window from the whole strip) or,
  with the window option `app_owns_titlebar_drag: true`, the whole view
  (AppKit never drags it; it also stops AppKit delaying titlebar clicks
  while it waits for a possible double click). There is nothing in between.
- `WindowControlArea::Drag` (`window_control_area`) is a no-op on macOS:
  `on_hit_test_window_control` is unimplemented there (it serves Windows).
- `Window::start_window_move` is `performWindowDragWithEvent:` with the
  current event; `Window::titlebar_double_click` runs the system's
  "double-click a window's title bar" setting (zoom, fill, minimize or
  nothing).

So the main window now opens with `app_owns_titlebar_drag: true`
(`app.rs`'s `open_window`) and moves itself from the empty parts of its top
row. No gpui patch.

## What landed

- **`elements::window_drag::region(id)`**, the drag spot, modeled on
  gpui-component's `TitleBar`: a left press arms it, the first mouse move
  with the button still down (caught by a window-wide capture listener, so a
  quick flick whose first move already left the region still counts) calls
  `start_window_move`, and a double click calls `titlebar_double_click`. A
  press elsewhere disarms it (capture phase, before the pressed region
  arms). A click without movement does nothing. Regions sit beside
  interactive elements, never under them, since gpui hovers every hitbox
  under the pointer.
- **Where** (only the main window; every other window keeps AppKit's own
  titlebar drag):
  - the tab bar: the traffic lights' room before the first item
    (`tab-bar-drag-lead`, `LEAD_X` wide, replacing the bar's left padding,
    so nothing moves), and the space past the `+` (`tab-bar-drag-end`, the
    same element that is the reorder's "after the last tab" gap);
  - the workflow and agent editors' titlebar rows (which replace the tab
    bar): the traffic lights' room (`*-drag-lead`, absolute, out of the
    row's flow) and the rest of each side past its buttons
    (`*-drag-left`, `*-drag-right`);
  - the empty-state chooser (no titlebar row): a 52pt strip across its top
    (`chooser-drag`), its content padded below it.
- A tab, the hover `×`, the `+`, the right-click menu, the Toggle sidebar
  button, Back, Save and the editors' other buttons are no longer window
  drag spots at all: a press there is theirs.

## Decisions

- **The app-owned drag over a per-region opt-out.** gpui's option is the
  only supported mechanism and it is all or nothing; a per-region opaque
  rect would mean patching `gpui-pre-macos`. Marking empty spots is also the
  safer failure: a spot nobody marked just doesn't drag the window, where an
  interactive element someone forgot to opt out would lose its clicks.
- **Arm on press, move on the first drag**, as gpui-component does, rather
  than calling `start_window_move` on the press: AppKit's drag loop eats the
  mouse up, so a plain click in a region would otherwise never complete.
- **One thread-local flag** for every region: there is one pointer.
- **The test platform** doesn't implement `start_window_move` (it panics), so
  under `cfg(test)` a region records `Move` / `DoubleClick` in
  `window_drag::test_log` instead; `drag_from` and `click_at` drive the
  gestures.
- **The chooser's content moves 20pt down** (its top padding is the strip's
  52pt rather than 32pt) so no row sits under the strip.

## Tests

- `elements::window_drag::tests`: a press and move moves the window once; a
  flick out of the region still moves it; a click does nothing and a double
  click is the titlebar's; a press on a neighbor dragged into the region,
  or a buttonless move after a click, never moves it, and the neighbor still
  clicks.
- `app::tabs_tests::the_bars_empty_space_moves_the_window_and_a_tab_never_does`:
  the lead and end regions' bounds (the tabs stay put), a drag in each moves
  the window, a double click past the `+` is the titlebar's; a tab drag
  reorders without a move, the `+` never moves it, a tab still clicks.
- `app::settings_tests::the_{workflow,agent}_editors_titlebar_moves_the_window_from_its_empty_space`,
  and the chooser's strip in `app::window_tests::quitting_the_shown_project_keeps_the_window`.

## Verified in the bundled app

`scripts/bundle.sh` (ad-hoc signed), launched over the `--visual-figma`
fixtures in a sandboxed root (hub and control socket variables unset) with a
second open project, so the bar had two tabs, Shelbi and Website. The pointer
was driven with posted CGEvents, and the window frame was read from
`CGWindowListCopyWindowInfo` before and after each gesture:

- Dragging the Website tab onto Shelbi's left half: mid-drag, the ghost
  followed the pointer, the tab dimmed in place and the marker showed on
  Shelbi's left edge. On release the order was Website, Shelbi. The window
  frame stayed at (1148, 363) throughout.
- Dragging the bar's space past the `+` by (+80, +50) moved the window by
  (+76, +47). Dragging the room beside the traffic lights by (-60, -30)
  moved it (-57, -29). The window follows from the first move event, so the
  few points before it are lost, as in gpui-component's title bar.
- A double click past the `+` zoomed the window (the system setting is
  unset, so zoom) and a second one restored it. A double click on a tab
  switched to its project and did not zoom.
- A tab click switched projects. Hovering a tab showed its `×`, and clicking
  it asked "Close Shelbi?" (Esc cancelled). The `+` opened its menu, and a
  press outside closed it. Right-clicking a tab opened Close Project / Close
  All Other Projects. The sidebar header's Toggle sidebar hid the sidebar,
  and the tab bar's toggle brought it back.

The editors' and the chooser's regions are covered by the tests only.
