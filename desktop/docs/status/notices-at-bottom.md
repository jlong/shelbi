# notices-at-bottom: the notices move to the foot, and read

## What landed

The relaunch bar ("Shelbi was updated. Relaunch to keep working…") and the
remote window's action bar ("Shelbi isn't running on devbox." with Start
Shelbi) used to fill the window's top row, over the traffic lights and
above the project tabs. The main area's notices (session lost, a newer
release, the status line, the banner, the review decisions) stacked at the
top of the main area. All of them now sit at the bottom, and they read in
both themes and on a translucent window.

```
      │ Shelbi │ ChartMe │ website │ Crown & Keep │ + │   <- the tabs stay the top row
 sidebar │ main
         │ ...
         │ The session for alpha-1 was lost.             <- the main area's notices,
         │ Couldn't save the workflow…              [x]     along its foot
 ⚠ Shelbi was updated. Relaunch to keep working…   [Relaunch Shelbi]  <- the window's bars,
 ⚠ Shelbi isn't running on devbox.                 [Start Shelbi]        full width
```

- **Placement** (`app.rs`): the window column draws the tab bar and the
  shell first, then the relaunch and remote bars, full width, with normal
  side padding (no traffic-light inset). The main column draws its screen
  (`flex_1`, `min_h_0`) first, then its notices.
- **Why they were hard to read**: the bars painted a translucent tint
  (`notice`, a `*_tint`) with no surface under it. In the title row nothing
  sits under the tint, so on a translucent window (`desktop.window_opacity`
  below 100) the blurred desktop showed through behind the text, and the
  Success button's own green tint mixed with it until its label nearly
  vanished.
- **The band** (`style::notice_band`, `Theme::band`): every notice is one
  band: its tint laid on the opaque canvas, so it is opaque whatever the
  window's opacity; a divider on top; the message in `text_loud` on every
  hue (the hue lives in the band's tint and icon).
- **Its controls**: labelled buttons sit on `style::band_button`, the opaque
  canvas under the button, so they paint exactly as they do on the canvas
  elsewhere (Relaunch / Start Shelbi stay the Success style; the decision
  notice's Reopen the Default). The dismiss buttons are
  `style::band_icon_button`, the icon in `foreground` at rest, since the
  quiet icon gray fell under 3:1 on the light bands.
- **Checked**: `palette.rs`'s `pairings` adds the bands: `text_loud` on each
  band at AA, the dismiss icon and the band icons at 3:1, the update bar's
  "Upgrade with" (now `foreground`). No new contrast exceptions.
- **Tests**: `app_notice_tests.rs` pins the layout (the tabs at the top, the
  bars full width at the window's foot under the sidebar, the banner at the
  main area's foot) and the banner's dismiss.
- **Visual check**: `--visual-views` renders `notice-bars.png` (dark),
  `notice-bars-light.png` and `notice-bars-glass.png` (dark at 84% over a
  bright desktop), a stand-in window around the real bars.
