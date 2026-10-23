# dt-figma-semantic-token-sync: the palette synced to `03 Desktop`

## What landed

John reorganized the Figma variables into semantic collections. The desktop
palette (`src/theme/palette.rs`) still carried older sampled values and
comments claiming Dark hover, Danger and checkbox colors were undefined. It
now reads the `03 Desktop` collection (`VariableCollectionId:2162:2`, Light
`2162:0`, Dark `2162:1`), resolved with both modes on 2026-10-22:

| Token (constant) | Was | Now |
|---|---|---|
| `text/foreground` (`FOREGROUND`), Dark | `#f2f3f7b2` (70%) | `#f2f3f7d9` (85%) |
| `text/bold`, `button/default/text` (`WHITE`), Dark | `#f8f8fa` | `#f2f3f7` |
| `text/muted` (`MUTED`), Light | `#444c65a6` (65%) | `#444c6580` (50%) |
| `text/muted` (`MUTED`), Dark | `#777a86` | `#f2f3f78c` (55%) |
| `button/default/icon` (`BUTTON_ICON`) | `#acb4cf99` / `#7b85a7e5` | `#acb4cf` / `#7b85a7`, opaque |
| `button/success/background-active`, Dark | `#5ac35161` | `#5ac35166` (40%) |
| `button/danger/background-active`, Dark | `#f2147861` | `#f2147866` (40%) |
| `text/ansi_green` / `yellow` / `blue` / `magenta` / `cyan` / `dark_gray`, Dark | `#5fb56d` `#dcb767` `#4a8fd7` `#c586c0` `#4ec9b0` `#7c7c7c` | `#5acd25` `#dbc300` `#5893d6` `#d259d7` `#00a6b2` `#666666` |
| `checkbox/default/border` (new `check_border` role) | `input_border`, 30% / 50% | `#acb4cf` at 20% / 50% |

- **Provenance.** Every `figma` constant is documented with its semantic
  name (`surface/*`, `text/*`, `icon/*`, `stroke/*`, `button/*`, `focus/*`,
  `checkbox/*`). The Rust names stay (`WHITE` is `text/bold`, `GRAY` is
  `icon/primary`); no role was renamed.
- **Frame overrides retired.** `FOREGROUND_FORM` and `MUTED_FORM` were the
  Settings frame's 85% / 55% against older shared values; the shared tokens
  now say the same, so the constants are gone and `form_text` / `form_hint`
  read `text/foreground` / `text/muted` (kept as roles). The scrim is
  `surface/overlay` and the card and textbox well `surface/depth-0`, now
  variables rather than measurements.
- **Kept on purpose.** `log_text` / `table_text` (`#7b85a7` opaque): the
  Activity and Machines frames bind a `text/gray` that isn't in
  `03 Desktop`, so the frames' value stays (`figma::measured::LOG_GRAY`),
  and so do the cards' `other/workflow_badge_*`. The sidebar, raised surface
  and Issue detail edge stay measured.
- **Buttons.** Default, Primary, Select, Success and Danger keep their own
  roles though several values coincide. `button/default/icon` is its own
  token, not `icon/primary` (90%, the `icon` role) or `icon/secondary`
  (60%); only button icons got louder.
- **Checkbox.** `controls::check` edges an unchecked box in `check_border`;
  textboxes, the slider thumb and the editors' fields keep `input_border`
  (`stroke/dark`).
- **Terminal.** The dark ANSI sixteen (`SENSIBLE`, `04 Terminal`) are
  untouched, as are user overrides and the TUI. The terminal's default
  foreground, bold and dim come from the shared text roles composited over
  the canvas (`TerminalPalette::for_palette`), so they moved with the
  tokens: dark `#d2d4d9` / `#f2f3f7` / `#92959d`, light dim `#a1a5b2`.

## Decisions

- **Alpha bytes follow Figma's float.** Figma stores opacity as an f32; a
  byte is that float times 255, rounded (70% is `b2`, 90% `e5`, 50% `80`),
  which is what every existing value already was.
  `token_alphas_are_figmas_opacities` checks every percentage-defined token
  against it.
- **No colors shifted for contrast.** New shortfalls are listed in
  `docs/theme.md`: dark `magenta on` (3.64) and `cyan on` (4.04) off the
  canvas, `check_border on` in both modes (1.37 / 1.41), and Light terminal
  dim text (2.46 on white; the terminal test holds it to a 2.4 floor instead
  of 3:1). `button_icon` left the exceptions: opaque, it now passes 3:1.

## Verified

- `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
  (744 passed) in `desktop/`. No shared crate changed.
- `--visual-figma` in both themes: buttons, checkboxes, Settings, Activity,
  the orchestrator terminal. Against fresh Figma exports of the current
  frames, Activity Dark differs on 0.36% of pixels (6% fuzz). The terminal's
  body text samples `#d2d4d9` in both the render and the Figma frame (the
  older export in `design-refs` reads `#b2b5bb`).
