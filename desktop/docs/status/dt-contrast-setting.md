# dt-contrast-setting: a Contrast slider

## The change

Settings ▸ General gets a Contrast row between Theme and Window
transparency, a slider like the translucency ones:

```
Theme
[ Auto | Dark | Light ]

Contrast
[━━━━━━━━━━○─────────]  50%
Separates text and icons from the backgrounds: darker backgrounds and
brighter text in Dark, lighter backgrounds and darker text in Light.
Terminals keep their colors.

Window transparency
[━━━━━━━━━━━━━━━━━━○─]  90%
```

- 0% to 100%, default 0% (the palette as designed, exactly).
- Applies to every open window as the slider moves (the `Theme` global is
  re-resolved and every window refreshed), and is saved when it is let go,
  to `desktop.contrast` in `~/.shelbi/config.yaml` (`desktop/README.md`).
  0 isn't written; out of range clamps to 100; a fraction (`0.5`) reads as
  50, as `window_opacity` does. It is restored at launch (`theme::init`).
- A slider moved in another window's Settings shows here too.

## How it works

`src/theme/contrast.rs`, applied when the theme resolves
(`Theme::resolve_with` → `Palette::with_contrast`), before the window
opacity, so the two compose: at 90% opacity and 60% contrast the surfaces
are the darker colors at 90% alpha. Reduce Transparency makes the window
opaque and leaves the contrast alone. Each role moves by its class, in
proportion to the setting (`docs/theme.md`, "Contrast", has the table):

| At 100% | Dark | Light |
|---|---|---|
| Surfaces (canvas, sidebar, cards, raised, textbox, keycaps, workflow badge) | 35% toward black: canvas `#1e2330` → `#14171f`, sidebar `#2c3140` → `#1d202a` | 50% toward white: sidebar `#f9f9fc` → `#fcfcfe`; white stays white |
| Text and icons | half toward white, alpha half toward opaque: `foreground` 85% → 93%, `muted` 55% → 78%, `icon` `#acb4cf` → `#d6dae7` | half toward black, alpha half toward opaque: `muted` `#444c65` 50% → `#222633` 75%, `icon` `#7b85a7` → `#3e4354` |
| Borders and dividers | half toward white, alpha doubled (`divider` 10% → 20%) | half toward black, alpha doubled |
| Accent and status hues | a fifth toward white (hue kept): accent `#62cdf6` → `#81d7f8` | a fifth toward black (hue kept): accent `#f21478` → `#c21060` |
| Translucent fills (rows, chips, buttons, selection, tints, glows, scrim), a checked box | unchanged; they composite darker over the darker surfaces | unchanged |

At 50% every move is half of that. Measured on the canvas:

| | 0 | 50 | 100 |
|---|---|---|---|
| Dark body text | 10.6 | 12.5 | 14.7 |
| Dark muted text | 5.2 | 7.5 | 10.5 |
| Dark icons | 6.4 | 8.7 | 11.6 |
| Light body text | 8.2 | 11.0 | 14.8 |
| Light muted text | 2.5 | 3.8 | 6.7 |
| Light icons | 3.1 | 4.9 | 8.4 |

The light theme's surfaces are white or nearly so, so its gain comes from
the text, icons and rules; the sidebar gets a touch lighter, closer to the
white canvas.

## Terminals

Untouched. The terminal palette is always built from the contrast-0
palette (default foreground and background, bold, dim, cursor, selection,
the ANSI sixteen), and the views hosting a live terminal (the pane, the
session view's insets) paint that background (`Theme::terminal_canvas`), so
the grid has no seam against a darker canvas. While the window is
translucent a terminal's default background is the window's glass (as with
opacity alone), which carries the adjusted canvas: no extra layer can bring
it back to the standard color without thickening the glass.

## Tests

- `theme::contrast`: every role has a class; 0 is the palette exactly; every
  pairing the WCAG checks cover holds or gains contrast at each 5% step in
  both themes (one documented dip, below); body, muted, icon and hint text
  gain at least 10% at 100 on the canvas and sidebar, and the accent and
  status colors gain on the canvas; hues keep their hue (within 4°); the
  config round-trips and 0 isn't written.
- `theme::tests`: the terminal palette is identical at every contrast (and
  with translucency); contrast composes with the opacity both ways; body text
  still holds 3:1 at the opacity floor over mid gray at 100.
- `app::settings_tests::the_contrast_slider_restyles_the_windows_as_it_moves_and_saves`:
  restored from config, keys and clicks apply and save, 0 isn't written, a
  theme switch keeps it, Reduce Transparency keeps it, another window's
  change shows on the slider.
- `shelbi-state`: `contrast_round_trips_and_reads_leniently`.

The one dip: the log's dark gray on the selected Machines row's band (1.21:1
at 0, already a listed exception) is the only text darker than its band in
Dark. The darker canvas pulls the band down while the gray brightens, so
they cross near 15% (about 1.0) before the gray clears the band; at 100 it
reads better than at 0. The test names it and checks the end-to-end gain.

## Captures

`--visual-figma` takes `SHELBI_VISUAL_CONTRAST=<percent>` (also written to
the fixture `config.yaml`, so Settings shows it), and
`SHELBI_VISUAL_ONLY=main` renders just the orchestrator, a workspace, the
board and Settings. The captures are in
`~/.shelbi/projects/shelbi/design-refs/dt-contrast-setting/`
(`<screen>-<theme>-<contrast>.png`, plus `main-window-contrast-sheet.png`:
the board at dark 0 / 50 / 100 and light 100).
