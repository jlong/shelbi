# tab-drag-bugs: the floating ghost and the drop gap between windows

Three bugs dragging a desktop tab between windows (`tab-tearoff`).

## The problems and their causes

1. **A faint line above the floating ghost.** gpui (gpui-pre 0.3.9, macOS)
   opens a window with no titlebar as a *titled* window
   (`NSTitledWindowMask | NSFullSizeContentViewWindowMask`), and a
   `WindowKind::PopUp` as a titled `NSPanel`. A titled window draws a 1pt
   edge across its top even when its titlebar is transparent and its
   background clear. Captured with `screencapture -l` on a bare panel set up
   as gpui sets up the pop-up: the titled one shows two opaque pixel rows (at
   2x) at its top, the borderless one nothing.
2. **The ghost stopped about 80pt below the menu bar.** AppKit keeps a titled
   window's frame below the menu bar (the screen's visible frame). The ghost
   window sits `TAB_H + FLOAT_PAD` above the pointer, so its top hit the menu
   bar's bottom first. Checked the same way: a titled panel asked to put its
   top 60pt above the screen's top got the visible frame's top; a borderless
   one at the pop-up level (101) got exactly what it asked for, from
   `setFrameTopLeftPoint:`, `setFrameOrigin:` and `setFrame:display:` alike.
3. **The drop gap didn't follow the tab between windows.** Each bar kept its
   own idea of the drag, handed off edge-triggered on the move that changed
   the target, so:
   - the tab's own window never closed its slot while the tab was over
     another window's bar or torn off (two gaps at once);
   - the `+` didn't move with the tabs, so a gap at the end of another
     window's bar never showed (and the last tab slid over the `+`), though
     a drop there landed at the end;
   - a hosting window only closed its gap if the move that left it reached
     it: one that didn't, or a drag gpui ended without this window hearing
     the release, left it open.

## What landed

- **Borderless pop-up** (`platform::screen::NativeWindow::make_overlay`):
  the floating ghost's window drops every style bit but
  `NSWindowStyleMaskNonactivatingPanel`. The frame is unchanged (its content
  already filled it), so gpui's view gets no resize. Right after it opens it
  is placed at the pointer once more, in case AppKit held it while titled.
- **Unclamped position**: `tab_tearoff::float_origin(pointer)` (always the
  pointer less the anchor) and `screen::appkit_point` (screen space to
  AppKit's global space, any display, no clamp) are pure and tested.
- **The session is the gap's one source of truth**: `DragState` carries
  `over` (the bar the tab is over), `over_x` and `gap` (the gap there,
  computed by the source window against that bar's resting layout).
  `DragState::role(window)` says what each bar shows (`BarRole::Own { home,
  gap }` for the source, `Host { .. }` for the bar the tab is over, `Idle`
  for every other), and `TabReorder::follow` applies it.
  - On every move the source follows it and has every other Shelbi window
    follow it (redrawing only a bar whose state changed).
  - Every window's render follows it too, so a missed move, or a drag gpui
    ended (no active drag) without the release reaching the source, can't
    leave a gap open.
  - A drop on another bar lands with the role (and gap) the session showed
    there (`Landing`), not whatever the bar last drew.
- **Away from its bar** (`TabReorder::away`): the source's tabs close up over
  the dragged tab's slot, which isn't drawn at all (rule included); back home
  it opens at the gap again.
- **The `+` follows the tabs** (`TabFrame::tail_offset`): left by the slot
  of a tab gone from the bar, right by the gap another window's tab opens.
  Over another bar's `+` the gap is at the end (where a drop lands).
- A tab with no slide yet slides from where it rests, so a hosted gap slides
  open on entry instead of jumping.

## Decisions

- **Borderless over overriding `constrainFrameRect:toScreen:`.** The class is
  gpui's shared `GPUIPanel`; patching it would touch every gpui panel.
  Borderless removes both the edge and the constraint with one setter.
- **The ghost keeps its 1px ring.** It is part of the dialog and popover
  shadows the ghost wears (drawn tight around it, not out in space), not a
  window edge.
- **Level-triggered over edge-triggered.** Re-deriving every bar from the
  session costs a handle update per window per move (the bar lookup already
  did that), and makes "exactly one gap" hold by construction.

## Tests

- `views::tab_drag`: away closes the slot and moves the `+`; a hosted gap
  opens, closes and reopens (also at the end); a hosted drop lands in the
  gap it showed; a hosted gap slides open from rest.
- `views::tab_tearoff`: exactly one bar shows the gap A → B → A → torn off →
  B, three rounds and in one move; the front window of overlapping ones
  wins; the host's role; the ghost's origin is the pointer's offset
  anywhere (menu bar, above the primary display, other displays).
- `platform::screen`: AppKit point conversion unclamped.
- `app::window_tests`: the gap follows the tab between two real windows
  back and forth (the source's tabs close up, the other bar's tabs and `+`
  step aside), a drop past another bar's last tab lands at the end,
  overlapping windows, and a bar whose move was missed closes its gap on its
  next frame.

## Not verified

The real app was not driven with synthetic drags (that would take over the
user's pointer). The AppKit behaviour behind 1 and 2 was checked on bare
panels as described above.
