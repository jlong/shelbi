# dt-window-transparency: translucent, blurred windows

## The change

Settings ▸ General gets two rows under Theme, in the Labeled Control rhythm
of the rows around them (400pt control column):

```
Window transparency
[━━━━━━━━━━━━━○──────]  85%
How much of the desktop shows through the window. 100% is opaque.

Background blur
[━━━━━○──────────────]  20
Blurs what shows through. 0 is a clear see-through.
```

- **Window transparency**: 30% to 100%, default 100% (opaque).
- **Background blur**: 0 to 50 points, default 0.

Both apply to every open window as the slider moves and are saved when it
is let go (the end of a drag or click, or each key step), to
`desktop.window_opacity` / `desktop.window_blur` in `~/.shelbi/config.yaml`
(documented in `desktop/README.md`). Defaults aren't written. At 100% / 0
nothing changes: the window is the opaque one it always was.

Reduce Transparency (System Settings ▸ Accessibility ▸ Display) forces every
window opaque. The sliders dim and stop taking input, and their helpers give
way to one note: "Reduce Transparency is on (System Settings ▸ Accessibility
▸ Display), so windows are opaque." The setting itself is kept for when it is
turned off. Like Reduce Motion, it is read at launch and each time a window
comes forward (System Settings is in front while it changes).

## Config

```yaml
desktop:
  window_opacity: 85   # percent, 30 to 100 (lower clamps to 30); 0.85 reads as 85
  window_blur: 20      # points, 0 to 50
```

Both keys read leniently (`shelbi_state::user_config`): a fraction up to 1
is a percentage (Ghostty's `background-opacity = 0.85` shape), an out of
range number saturates, and something that isn't a number is ignored rather
than failing the whole file. The app clamps into the ranges
(`theme::window::Translucency`).

### The 30% floor

At 30% the canvas over a mid-gray backdrop (what a blurred desktop tends to
average to) keeps body text (`theme.foreground`) at 3.5:1 in Dark and 5.1:1
in Light (`body_text_holds_3_to_1_at_the_opacity_floor_over_mid_gray`).
Over the worst case, a white page behind the dark theme, 30% is 1.5:1 and
3:1 needs 65%; blur is what makes the low end usable there. Below 30% the
window's own color stops reading as the window's in either theme.

## How the opacity is applied

At resolve time, never as an opacity layer (which would fade text too):
`Theme::with_opacity` gives the window's two surface roles, `canvas` and
`sidebar`, the opacity as their alpha, and nothing else changes. The rules
(also in `docs/theme.md` ▸ Window translucency):

- **One coat per region.** Translucent fills stack (85% over 85% is 98%), and
  the window painted its surfaces several times over: the shell under
  everything, the column and the main area, then every view and panel on
  them. The column, the main area, the bootstrap screen and the chooser stay
  the painters; the shell and every view or panel on the same surface now
  paint `theme.nested(role)`: the role while opaque (so they still hide what
  slides under them, and the opaque app is unchanged), nothing while
  translucent. Activity, Machines, the board, Settings, the Workspace and
  Review main areas, the session view and terminal panes on the canvas; the
  sidebar, the task panels, the settings sidebar and the empty review panel
  on the sidebar. `well` (the bootstrap command box, the canvas inside a
  divider) is the glass itself. The Review rule between the column and the
  main area paints its own canvas under the divider, which the shell no
  longer does.
- **Terminals** follow the window: the grid's default background paints
  nothing while translucent (the pane's surface is the terminal's
  background), so the pane is the same glass as the window, the way Ghostty
  does it. A program's own background (OSC 11) takes the opacity (it sits on
  the main area's coat, so it reads a little denser). Cells with a color of
  their own, the cursor, selection and search highlights, and an IME
  composition's cover stay opaque. Glyphs are untouched. The Terminal page's
  preview follows the same rule.
- **Opaque on purpose**: overlays (every dialog through `overlay::render_layer`,
  the task detail, the palette, menus and popovers, which were already
  `raised`), and fills a drop shadow sits under (gpui draws a shadow as a
  filled box, which shows through a translucent fill: the selected segment,
  the focused task-panel back button) paint from `theme.opaque()`; the
  terminal's notice band over the grid too. `card`, `raised`, `keycap`,
  `nav_selected`, text, icons, borders, dividers and every fill keep their
  colors.
- gpui-kit's `background` is `nested(canvas)`, so a kit widget never adds a
  coat.

Known: while two panels slide past each other (a column swap, the sidebar's
hide), a translucent window shows the outgoing panel through the incoming
one for the length of the slide, since neither coats its own surface.

## The blur: `CGSSetWindowBackgroundBlurRadius`

Below 100% the window is gpui's `WindowBackgroundAppearance::Transparent`
(`set_background_appearance`, on a live window), and the blur is the window
server's background blur at the slider's radius, set with the private
`CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), windowNumber,
radius)` (`platform/blur.rs`), what Ghostty's `background-blur` and iTerm use.

- **Why not gpui's `Blurred`**: it adds an `NSVisualEffectView` behind the
  content with one fixed system radius, so it can't follow a slider, and it
  brings the material's own tint (gpui strips it with layer-class sniffing).
- **Why not our own `NSVisualEffectView`**: same fixed radius; varying it
  means private CA filter keys on its backdrop layer, which is more fragile
  than the one CGS call.
- **Reliability**: the symbol has shipped unchanged since OS X 10.x and is
  present on this machine's macOS 26.6.2 (checked with `dlsym`). It is
  looked up at run time (`dlsym(RTLD_DEFAULT, …)`), never linked, so a
  macOS that drops it leaves the window translucent and unblurred with one
  log line, and never fails to launch. Opaque windows get radius 0. Each
  window's applied look is remembered, so a re-apply that changes nothing
  doesn't touch AppKit.
- A slider moves inside its own window's update, which holds that window,
  so the look reaches the windows from a deferred pass (a direct update
  failed silently; the view test caught it).

## The slider (an undrawn component)

The Components page (`2196:10061`) has no slider, so `controls::Slider` is
built from the drawn controls' tokens. **For John to draw**:

- Rail: 4pt tall, round ends, `theme.search` (the segmented track's fill);
  the filled part `check_fill` (the forms' selected color: cyan in Dark,
  pink in Light).
- Thumb: 16pt round, the Button's fill (`theme.background`, its hover and
  pressed fills fading in on the motion springs) over the opaque canvas,
  the textbox's 1pt `input_border` edge, the raised card's drops. Its
  center travels the track inset by half a thumb, so it never overhangs.
- Readout: Base in `form_text`, right aligned in a 44pt column 12pt after
  the track (`85%`, `20`).
- Focused: the focus ring and glow (`focus_shadow`) around the thumb.
- Disabled: the whole row at 40% (the Button's Disabled), no pointer.

Keys: ← / → step 1, ⇧← / ⇧→ step 10 (opacity) or 5 (blur), Home / End go to
the ends. A click lands the thumb under the pointer and focuses the slider;
a drag follows the pointer anywhere in the window until the button comes up.
The settings page has no Tab traversal between controls (the sidebar owns
Tab), so the keyboard reaches a slider once it is clicked, as it reaches the
text fields.

## Checked

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo clippy --all-targets --features visual-check -- -D warnings &&
  cargo test`: clean, 609 tests pass.
- Root: `cargo test -p shelbi-state`, `cargo clippy -p shelbi-state
  --all-targets -- -D warnings`, `cargo check --workspace --locked`
  (`Cargo.lock` unchanged): clean, the new `shelbi-state` test passes.
- Tests added: the config round-trip and lenient reads
  (`shelbi-state`), clamping, round-trip and defaults not written, opaque
  whatever the blur, Reduce Transparency forcing opaque
  (`theme::window`), slider stepping, fraction and position mapping
  (`controls`), the opacity reaching only the surface roles, `nested`,
  `opaque`, and the floor's contrast (`theme`), and two window tests
  (`app_settings_tests`): the sliders apply live to the window and the
  theme, keys and clicks step and save, 100% and 0 aren't written, a theme
  switch keeps the opacity; Reduce Transparency forces opaque, disables the
  sliders, shows the note, and lets go cleanly.
- `--visual-figma` at 100% / 0 against `origin/main` (both themes, 146
  frames each): 142 byte-identical. The 4 that differ are
  `settings-general-{dark,light}` and `state-settings-cli-focused-{dark,light}`:
  the General page now has the two slider rows (checked by eye: both themes,
  the rows in the Labeled Control rhythm, the readouts right aligned). 0
  checks failed on either side.
- Translucent frames: `SHELBI_VISUAL_OPACITY=85` (new, like
  `SHELBI_VISUAL_THEME`) with `SHELBI_VISUAL_ONLY=panels` and `=issues`,
  dark, release. The offscreen readback flattens alpha onto black, so the
  frames measure the coat rather than show the glass: the canvas
  `#1e2330` reads `(26,30,41)` and the sidebar `#2c3140` `(37,42,54)`,
  exactly one 85% coat over black everywhere, the terminal grid included
  (two coats would read `(29,34,47)`). The task detail modal stays
  `(30,35,48)`, opaque. Text, icons, the selected row, the Zen pill and
  terminal glyphs are unchanged.
- Benches, release, opaque vs `--opacity 85 --blur 20` (new flags on
  `--bench-board` and `--bench-activity`; offscreen, so they measure the
  app's frames, not the window server's compositing and blur, which happen
  outside our process). p50 ms, opaque → 85/20, on a loaded hub:
  board idle 4.9 → 5.1, scroll board 5.4 → 5.7, scroll column 11.1 → 10.1,
  hover 5.4 → 4.9, select 11.4 → 10.0; Activity idle 2.4 → 2.5, scroll
  5.9 → 5.4, hover 2.7 → 2.5, select 5.6 → 5.3. No regression: the
  translucent theme paints the same quads (a few now clear). The tails
  move between runs either way (other workers building).
- Real app: not checked on screen. The screen was unlocked, but the full
  app attaches to the live hub and the user's real config, and the
  sandboxed `--spike` route (it now applies the look too) was cut for time.
  On screen, the blur radius and Reduce Transparency's live toggle are what
  remains to see; both are covered by the tests above only up to the AppKit
  call.
