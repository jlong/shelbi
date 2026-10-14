# dt-theme-tokens: one set of named color tokens, dark and light

## What landed

- **`crates/shelbi-app/src/theme.rs`**: `Rgb` (with WCAG luminance, contrast,
  and lerp), `Ansi` / `TermColor`, `Swatch`, `Appearance`, and
  `Theme<C = Swatch>`, which has one field per token (36 tokens plus
  `appearance`). It also has `Theme::dark()`, `Theme::light()`,
  `Theme::for_preference(pref, system_is_dark)`, `resolve_appearance`,
  `Theme::map` (an exhaustive per-token conversion), `Theme::tokens()`
  (`(name, value)` pairs), and `Theme::project_pulse`, the palette's
  breathing pulse, moved here from the TUI so both clients share it.
  `ThemePreference` is re-exported from `shelbi-state`. No toolkit types;
  `no_ui_deps` passes.
- **Setting**: `theme: auto | dark | light` in `~/.shelbi/config.yaml`
  (`shelbi_state::UserConfig::theme`, `ThemePreference`, default `auto`,
  omitted on save when `auto`). `shelbi config lint` now accepts the key and
  rejects bad values. It's documented in
  `site/content/docs/configuration/global.mdx` (a table row plus a "Theme"
  section) and in a commented `# theme: auto` line in the `config.yaml`
  scaffold.
- **TUI**: `crates/shelbi-tui/src/theme.rs` is now the conversion layer.
  `Palette = Theme<ratatui::Color>`, chosen once at startup in
  `shell::run_with` (`install_from_config`; `auto` is dark) and read
  everywhere through `palette()`. Every surface was moved off color
  literals: about 300 sites across 17 files.
  `crates/shelbi-tui/tests/no_color_literals.rs` fails the build on a ratatui
  color literal in non-test code outside `theme.rs` and
  `shell/terminal_view.rs`.
- **Desktop**: `desktop/src/theme.rs` wraps `Theme<Hsla>` in a gpui
  `Global`, resolves `auto` from the window's appearance, and re-resolves
  live through `observe_window_appearance` (`follow_system`, held by the
  root view). `dark` / `light` pin both the theme and the window chrome
  (`set_window_appearance`). gpui-kit's light/dark mode follows the resolved
  theme. The foundation window paints from the new tokens (`canvas` replaces
  the desktop-only `main_background`).

## Decisions

- **Field names are the Figma token names** (`background`, `foreground`,
  `gray`, `white`, `muted`, `cyan`, `green`, `red`, `yellow`, `selection`,
  `search`, `divider`, `zen`). TUI constants that aliased one Figma token
  merged into it: `TEXT`/`FG_SECONDARY` became `gray`;
  `ACCENT`/`ACCENT_CYAN` became `cyan`;
  `BUSY_GREEN`/`PALETTE_GREEN`/`PROJECT_STATUS_GREEN` became `green`;
  `MUTED`/`PALETTE_MUTED` became `muted`; `SEARCH_BG`/`PALETTE_BG` became
  `search`. Each field's doc names the constants it replaced. In light, a
  name keeps its role, not its hue: `white` is the strongest text, which is
  near-black.
- **ANSI colors are tokens too.** The TUI painted about 250 cells with
  terminal palette colors (`Color::DarkGray`, `Cyan`, `Yellow`, ...) for
  status, categories, and identity. They became `ansi_*` tokens whose dark
  value is a `Swatch` carrying the ANSI slot, so the TUI keeps emitting the
  same escape codes and the dark rendering stays byte-identical. Each one's
  RGB (what the desktop paints) comes from the site's TUI mockup palette
  (`--tui-*` in `site/app/globals.css`). Light pins every token to an exact
  RGB, because a light terminal remaps ANSI unpredictably.
- **A few one-off fills became named tokens**: `on_color` (text on a
  saturated fill), `error_button`, `error_marker`, `notice`, `status_bar`,
  `code_bg`/`code_fg` (xterm 236/252 in dark), `pulse_trough`/`pulse_peak`,
  `divider_active` (ANSI cyan in dark), `workflow_badge_bg`/`_fg`, and
  `canvas` (the main-area fill: terminal default in the TUI, painted by the
  desktop).
- **Light values** follow shelbi.dev's light mode: a near-white background
  (`#f7f7f7`), a white canvas, near-black text, and a gray ramp. The cyan and
  green keep their hue but are darkened to clear 4.5:1 (`#007a83`,
  `#367b16`). Every text token is at least 4.5:1 on `background` and
  `canvas`, and the primary text tokens are at least 4.5:1 on `selection`
  and `search` (tested).
- **The process-wide theme is a `OnceLock` plus a thread-local override**,
  not a `&Theme` threaded through every render function. The TUI picks its
  theme once, and tests can render one surface in light without racing other
  tests.
- **`Color::Reset` stays as-is** (the terminal default, not a color), and
  `shell/terminal_view.rs` is exempt from the guard because it converts a
  hosted program's own colors.
- **No config-upgrade sniffer** (per the task): a missing `theme:` is `auto`.
  The scaffold's new line is a comment, so existing configs lose nothing.

## Verification

- Dark is byte-identical: all 628 pre-existing `shelbi-tui` tests pass
  unchanged; several assert exact `Color::White` / `Color::Reset` / RGB
  cells. `theme::tests::dark_converts_to_the_tuis_long_standing_colors` pins
  every token's dark conversion to the old literal, and
  `pulse_matches_the_pre_token_formula` pins the pulse at 101 phases.
- Light: 14 `renders_readable_in_light` tests (shell sidebar, kanban
  board/popover/workflow dropdown, activity, machines, review panel,
  workspace panel, palette, add project, review confirm, review reject, zen
  intro, error log, shell chrome, markdown code spans) render under
  `with_appearance(Light)` and run `assert_light_readable`. That check fails
  on any terminal-palette color and on any glyph under 3:1 against what it
  sits on. With `SHELBI_THEME_DUMP=<dir>` they write HTML. I checked the
  sidebar, kanban, palette, and review panel dumps by eye; all read cleanly
  on white. Nothing assumed a black background: the half-block bleed rows
  carry the fill as foreground over the terminal default, and the overlay
  backdrop is the DIM modifier, which a light terminal fades toward its own
  background. No render fix was needed beyond `on_color` on the saturated
  fills.
- Desktop, by hand, before the rebase (logging each resolve, `SHELBI_HOME`
  isolated; the theme code didn't change in the rebase, only the bootstrap
  view's token names): `auto`
  started Light under a light system, went Dark when I flipped macOS dark
  mode with AppleScript, then Light again when I flipped it back. `dark` and
  `light` each resolved once and did not move when the system flipped. The
  system was left in its original (light) mode.

Commands, run after rebasing onto `jlong/desktop-app` (with
platform-bootstrap):

- `desktop/`: `cargo build` and `cargo clippy --all-targets -- -D warnings`
  are clean; `cargo test` passes 47 tests.
- Root: `cargo build --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` are clean, and
  `cargo check --workspace --locked` resolves; the root `Cargo.lock` is
  unchanged.
- `cargo test --workspace --no-fail-fast`: 3841 passed and 2 failed. The two
  failures were `shelbi --test old_session_compat` ("session socket never
  appeared" within 10s) and `shelbi --test remote_session_e2e` ("never came
  up Alive" within 10s). Both are session/relay end-to-end timing tests that
  this change doesn't touch, and both pass when rerun alone
  (`cargo test -p shelbi --test old_session_compat --test remote_session_e2e`).
  These are load flakes, so I'm leaving them to CI.
- `cargo test -p shelbi-tui`: 646 passed (628 pre-existing plus 18 new).

## Not done

- I couldn't screenshot the desktop window (no screen-capture permission for
  the terminal), so the live switch is verified from the resolve log, not an
  image.
- The TUI reads `theme:` once at startup; changing it needs a restart
  (documented).
- Dark `muted` on the desktop's `canvas` (#1c1c1c) is 3.97:1. That's the
  TUI's value, kept as-is; the contrast test checks dark against
  `background`.
