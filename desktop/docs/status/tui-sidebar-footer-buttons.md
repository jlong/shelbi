# tui-sidebar-footer-buttons: Settings and Send Feedback in the TUI's footer

## The change

The TUI's Settings gear sat at the end of the sidebar's title row, and the TUI
had no way to send feedback. The desktop draws both in its footer's bottom-left
corner (Figma Navigation's footer Toolbar): Settings, then the megaphone, then
the unread-errors badge while there are any. The TUI's footer now does the same.

## Shared command (`shelbi-app`)

- `CommandKind::SendFeedback` / `Effect::SendFeedback`, id
  `action:send-feedback`, palette title "Send Feedback" (the desktop's tooltip
  and menu label), decoration `📣`, always offered, not a mutation.
- Scenario `send_feedback_reaches_the_host_and_leaves_the_view`: the host opens
  Contact Us; the view, focus and overlays stay put.
- Two theme tokens: `button_hover` (the desktop's `background_hover` over the
  canvas: dark `#45474e`, light `#e6e8ee`) and `error_button_hover` (`#825454`).

## Desktop

- The megaphone (`SidebarEvent::SendFeedback`) and Help ▸ Send Feedback… run
  the registry command (`HostCall::SendFeedback` → `open_feedback`). The Help
  item is a registry slot (`Placement::Help`), so `AppCommand::SendFeedback`
  is gone. The palette shows it with the megaphone icon.
- No keyboard shortcut, as before (the desktop has none to mirror).

## TUI

- **Footer rows** (bottom up, so a squeezed sidebar keeps the band and the
  toolbar first): the zen band, a spacer (the toolbar's `▀` row), the toolbar,
  its `▄` row, the version line's own row when it doesn't fit beside the
  buttons, then the status / first-run hint. One row taller than before.
- **Toolbar**: ` ⚙ ` (Settings), ` 📣 ` (Send Feedback, double width), ` ! `
  (unread errors), one column apart from the left inset. At rest Settings and
  Feedback are gray glyphs on the terminal background; under the pointer they
  take the search box's half-block pill in `button_hover` and the glyph goes
  white. The badge is always a filled pill (`error_button`, lighter on hover)
  and is now clickable (the error log, as the desktop's).
- **Tooltip**: the hovered button's desktop tooltip ("Settings", "Send
  Feedback", "N unread errors") shows beside the buttons, in place of the
  version line while hovered.
- **Version line**: right-aligned beside the buttons when it fits whole,
  otherwise on its own row above the toolbar (never cut short).
- **Clicks**: Settings opens Settings (as `,` does); Feedback and the badge run
  their registry commands through `run_command`. Until `tui-feedback-dialog`
  adds the overlay, `Effect::SendFeedback` sets a status line pointing at the
  desktop's Contact Us or the GitHub issues page (q-001).
- **Hover state**: the shell keeps the last pointer position from every mouse
  event (an overlay's included) and derives the hover each frame; it is `None`
  under an overlay, Settings, or a review / workspace panel, and after focus is
  lost. A repaint happens only when the hovered button changes.
- **Emoji fallback**: `Caps::emoji` (`TERM=linux`/`vt*`/`dumb`, or a `C`,
  `POSIX` or non-UTF-8 locale, says no); the buttons become ` Settings ` and
  ` Feedback `.
