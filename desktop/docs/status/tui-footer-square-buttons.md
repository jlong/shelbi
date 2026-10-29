# tui-footer-square-buttons: the TUI footer's buttons as square buttons

## The change

The TUI sidebar footer's Settings (`⚙`) and Send Feedback (`📣`) buttons were
bare glyphs that took a half-block pill only under the pointer. They are now
always-visible square buttons, built like the back arrow at the top of the
workspace and review sidebars:

```
 ▄▄▄▄▄ ▄▄▄▄▄ ▄▄▄▄▄
   ⚙     📣    !
 ▀▀▀▀▀ ▀▀▀▀▀ ▀▀▀▀▀
 ZEN MODE ON
```

## TUI

- **One helper**: `panel::draw_square_button` draws a square (a `▄` cap, the
  glyph centered on the solid fill with width-aware padding, a `▀` cap, the
  caps carrying the fill as their foreground). `panel::render_back_button`
  and the footer's `render_footer_button` both call it;
  `panel::square_button_width` (`BACK_BTN_WIDTH`, or the glyph plus a column
  each side when wider) and `panel::square_glyph_style` (gray, or white bold
  when selected) are shared too.
- **Look**: Settings and Send Feedback on the `selection` fill with a gray
  glyph, exactly the back button. Under the pointer the fill lifts to the
  lighter `button_hover`; the glyph stays gray (white bold stays the keyboard
  focus look). The unread-errors badge is the same square in `error_button`
  (`error_button_hover` under the pointer) with a bold `!`.
- **Emoji fallback**: without `Caps::emoji` the faces are `Settings` and
  `Feedback`, each square widened to fit its word.
- **Layout**: one column of inset, a column between squares; a square that
  wouldn't fit whole is left off (a 24-column sidebar still fits all three).
  Footer height and the Zen band are unchanged: the `▄` and `▀` rows are the
  toolbar's existing half-row margins.
- **Hover name**: beside the squares when the whole name fits there; at the
  default width that room is about 12 columns (7 with the badge), so a longer
  name ("Send Feedback", "2 unread errors", Settings with versions) floats on
  the row above the squares, over whatever sits there, like the desktop's
  tooltip. Clicks and hit areas (each square's columns across its three rows)
  are unchanged.
