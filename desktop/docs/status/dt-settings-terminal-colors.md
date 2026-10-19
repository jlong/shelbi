# dt-settings-terminal-colors: ANSI colors and a live preview on Settings ▸ Terminal

## The change

The Terminal page (`dt-settings-sections`) had the font, size and Option key
controls from John's Terminal frame (`2252:28494`). This adds the rest of it:

- **ANSI colors** (`2263:4808`): the title with a Small **Reset** button,
  the helper, then a row per color (Black to White) with its Regular and
  Bold field. Each field is the Textbox with the color's 16pt swatch before
  its hex. Typing a valid hex (`#E04F52`, `e04f52`) updates the swatch, the
  preview and every open terminal at once, and saves. On Enter or focus out
  the field is rewritten the table's way (`#E04F52`). Text that isn't six
  hex digits gives the field an error edge, shows a hint under the table
  ("A color is six hex digits, like #E04F52."), and isn't saved or applied.
  Reset puts every field back to the theme's colors, removes the config
  block, and is drawn disabled (40%, the Button's Disabled) when there is
  nothing to undo.
- **Preview** (`2264:7445`): the label over a 1pt `divider_strong` border,
  6pt corners, 24pt in. Inside is a read-only terminal pane drawn by the
  real terminal element (`TerminalPane::canned`) from a canned ANSI stream
  of the frame's Claude Code session (`views/terminal_preview.rs`): the
  mascot and identity, the request, the reply, Read / Update (with its
  red/green diff) / Bash (with the test output), the summary, "Worked for
  38s", and the prompt block (dividers, `❯` with the cursor, the mode line,
  the hint) at the foot of the grid. It uses the terminal settings global
  like every pane, so font, size and colors follow at once. There is no PTY
  behind it: keys and pastes go nowhere, and each new size is answered by a
  reset plus the stream laid out for that size.

## Config

`desktop.terminal_colors` in `~/.shelbi/config.yaml` (`shelbi_state::
TerminalColors`), documented in `desktop/README.md`:

```yaml
desktop:
  terminal_colors:
    red:
      regular: "#E04F52"   # ANSI 1
      bold: "#E69589"      # ANSI 9
```

- Only colors that differ from the theme's default for the current
  appearance are written. Typing the default back removes the key, and
  Reset removes the block.
- Bold is the bright eight (ANSI 8 to 15), which the frame labels Bold. The
  terminal still doesn't do bold-is-bright.
- One set of overrides applies in both appearances, as one palette does in
  other terminals.
- The app ignores a value that isn't `#rrggbb`. The field shows the default,
  and the next save from Settings drops it.
- Live apply: the overrides sit on `TerminalSettings` (the global the font
  and size use) as `ansi: [Option<Rgb>; 16]`. `TerminalElement` lays them
  over the theme's `TerminalPalette` each frame (`with_ansi`), and the row
  cache already re-lays rows when the palette changes. So agent panes,
  workspace terminals and the preview all pick up a change without a
  restart.

## Default palette (John, q-001)

The task said to keep the app's current palette unless the frame's values
were the documented defaults. They weren't: dark matched the frame only on
red, green and cyan. The frame binds its table to Figma variables
`ansi/regular/*` and `ansi/bold/*`, and its button layer is named "Reset to
Sensible", so I asked. John: switch the dark defaults to the frame's values
(the Sensible palette), Reset restores them, and light keeps its own set.
They are `theme::SENSIBLE`, also listed in `docs/theme.md`.

Dark, before and after:

| Color | Before regular | Before bold | After regular | After bold |
|---|---|---|---|---|
| Black | `#1d1f21` | `#7c7c7c` | `#000000` | `#666666` |
| Red | `#e04f52` | `#d54e53` | `#e04f52` | `#e69589` |
| Green | `#5acd25` | `#b9ca4a` | `#5acd25` | `#9bdaa6` |
| Yellow | `#dcb767` | `#e7c547` | `#dbc300` | `#e6d77b` |
| Blue | `#4a8fd7` | `#7aa6da` | `#5893d6` | `#5abdff` |
| Magenta | `#c586c0` | `#c397d8` | `#d259d7` | `#dc8de6` |
| Cyan | `#00a6b2` | `#70c0b1` | `#00a6b2` | `#82e6e5` |
| White | `#c5c8c6` | `#eaeaea` | `#bababa` | `#ffffff` |

"Before" was Tomorrow Night with the Figma roles in slots 1 to 6 and 8
(`theme.red/green/cyan`, `theme.ansi_yellow/blue/magenta/dark_gray`). Light
is unchanged: the GitHub-light set with those roles in the same slots. The
palette roles themselves (`yellow`, `blue`, `magenta`, `dark_gray`, which
Activity's tags use) keep their values. Only the terminal's ANSI slots moved
to the frame's variables. The terminal contrast test still passes on the new
dark set (every hue at least 3:1 on the canvas, black and bright black
excepted as before).

## Other decisions

- **Block elements** (`elements/terminal/boxdraw.rs`). The preview showed
  Claude Code's mascot (`▐▛███▜▌`) striped: Source Code Pro has no block
  elements, and the fallback font's glyphs leave seams. Every agent pane
  running Claude Code showed the same. The pane now fills U+2580 to U+259F
  itself (halves, eighths, `▔ ▕`, the quadrants) as quads snapped to device
  pixels, like the box lines, so neighbors share an edge. The shades
  (`░ ▒ ▓`) stay the font's. `mascot.png` in the images directory shows the
  result.
- **Error state.** The Components page's Textbox has only Focus and
  Placeholder variants, no error state. `controls::invalid_textbox` uses
  the `error` role for the edge (and its glow, at 30%, while focused), plus
  the hint line. John may want to draw one.
- **Disabled button.** There was no disabled Button in code.
  `style::disabled_button` is the Button with Disabled on (its fill and
  label at `button/disabled/opacity`, 40%, no hover, no pointer), as the
  frame draws Reset.
- **Reset is live when a field holds bad text** even if nothing is saved, so
  it can also put a half-typed field back.
- **The preview's surface** is the terminal palette's background (the
  canvas), not the frame's `color/background` black. The frame draws the
  preview in its own `color/*` set (black, `#c6c6c6` text, `#7a7a7a` muted,
  `#414141` rules), which no terminal in the app uses. The preview is meant
  to show what terminals look like, so it draws with what they draw with.
  This is most of the diff below. If the app's terminals should move to that
  black surface too, that's a separate change to `TerminalPalette`.
- **The stream's colors.** Lines the frame binds to `ansi/*` use the same
  SGR color (the mascot bright red, "Claude Code" and the input `❯` bold
  bright white, arguments cyan or yellow, the diff red and green, `ok`
  green, the result bold bright green, the mode line bright magenta). Tool
  output and footers are dim (SGR 2), the frame's `color/muted`. Lines the
  frame paints literal white (the request, the tool names) are the default
  foreground, bold for tool names, so they stay readable in light, where
  ANSI white is a gray. The rules are dim bright black. The frame's 16pt
  gaps are a blank row; its 4pt and 8pt gaps have no row.
- **Light** draws the dark frame's layout in light tokens (there is no light
  frame). The table shows the light defaults.

## figma-diff

`scripts/figma-diff.sh` at the default FUZZ 10, full 1440x1024 frame, dark,
against `settings-terminal-dark.png`:

| screen | main (`dt-settings-sections`) | after |
|---|---:|---:|
| settings-terminal-dark | 49.13% | **43.00%** |

The heat map is almost all one region, the preview's surface (674x965 at
739,32): the frame's black against the canvas, decided above. Split by
region (ImageMagick `compare -metric AE -fuzz 10%`, the same split as
`dt-settings-sections`):

| region | differing pixels |
|---|---:|
| controls column (400x700 at 312,32) | 1.10% |
| ANSI colors block (400x376 at 312,192) | 0.68% |
| preview (664x960 at 744,32) | 6.07% |
| sidebar (280x1024 at 0,0) | 0.29% |

Beyond the surface, the rest is known:

- The frame draws the font size as 13pt. The harness fixture keeps the 14pt
  default, so the preview's text runs a little wider and taller, and the
  size field reads `14pt`.
- The project section of the sidebar is missing (left out in
  `dt-settings-sections`).
- The Option segmented control is wider than the frame's 385pt (the Geist
  1.2 vs 1.7.2 advances `dt-figma-qa-activity` measured).
- The swatch sits 1pt right of the frame's (it follows the Textbox's 8pt
  inset inside its 1pt edge; the text lands on the frame's 33pt).

The other screens in the full run: settings-general 1.08% and
settings-keybindings 5.32% (both unchanged). The terminal screens
(orchestrator 2.40% / 2.60%, workspace 0.80% / 0.96%, review 0.94% / 1.08%)
render with the new dark palette and pass their checks.

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-settings-terminal-colors/`
(the dark and light renders, figma-diff's side, heat and overlay images, and
the mascot close-up).

## Tests

- `shelbi-state` `user_config`: `terminal_colors_write_only_what_is_set`
  (get/set by slot, YAML shape, empty block omitted).
- `elements/terminal/colors.rs`: hex parsing and validation (six digits,
  with or without `#`, case-free; short forms and junk rejected), the
  config round trip with a bad value ignored, and `with_ansi` replacing only
  its slots.
- `views/settings.rs`: the values round trip carries the colors, writing
  only what changed; defaults clear a stale block.
- `views/terminal_preview.rs`: the stream fed into a real `TerminalView`
  (the banner, the transcript, the prompt block at the foot with the cursor
  in it, a short grid that scrolls instead), and each line's ANSI color and
  attributes.
- `elements/terminal/boxdraw.rs`: block elements fill their parts on device
  pixels, quadrants tile without overlapping, and neighbors share an edge.
- `app_settings_tests.rs` (the headless window):
  `the_terminal_page_edits_colors_live_saves_them_and_resets` (a typed color
  reaches `TerminalSettings`, the preview pane and the saved config; bad
  text is marked and not saved; the default isn't written; Reset clears
  everything and disables), `the_terminal_page_shows_saved_colors`, and
  `the_preview_draws_the_frame_s_session_with_no_session_behind_it` (the
  pane shows the stream for its laid-out size, and keys change nothing).
- `theme/mod.rs` and `layout.rs` assertions moved to the Sensible values.

A test's settings view saves to an in-memory config (`saved`), never to
`~/.shelbi`.

## Verified

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo clippy --all-targets --features visual-check -- -D warnings &&
  cargo test`: all pass (575 tests, rebased on #1815).
- `--visual-check` and `--visual-views`: 0 checks failed in both themes.
  `--visual-figma` (full run, both themes): 0 checks failed. figma-diff is
  above.
- Root, since `shelbi-state` changed: `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo check --workspace --locked`
  passes, `Cargo.lock` unchanged. `cargo test -p shelbi-state`: 905 pass.
  Inside a worker pane, `event_log::append_workspace_pane_event_writes_lifecycle_line`
  fails because the pane's `SHELBI_HUB_SOCK` routes the append to the live
  hub. With `env -u SHELBI_HUB_SOCK` it passes, and it is unrelated to this
  change.
