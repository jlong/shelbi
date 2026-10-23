# dt-terminal-scrollbar-overlay: the terminal scrollbar as a macOS overlay

## What landed

The terminal panes' scrollbar (orchestrator, workspace, review, the Settings
▸ Terminal preview) was always visible whenever there was scrollback, and
sat inside the text padding, about 20pt in from the window edge. It is now
a macOS-style overlay scrollbar drawn against the pane area's right edge:

```
 before                                   after (shown)
┌────────────────────────────────────┐   ┌────────────────────────────────────┐
│                                    │   │                                   ▐│
│  $ cargo test                  ▐   │   │  $ cargo test                     ▐│
│  running 742 tests             ▐   │   │  running 742 tests                 │
│  ...                               │   │  ...                               │
└────────────────────────────────────┘   └────────────────────────────────────┘
   thumb at the grid's edge, always         thumb 2pt from the area's edge,
   shown with scrollback                    hidden at rest
```

- **Against the edge** (`scrollbar.rs`, `Rail`): the rail runs down the right
  edge of the pane's *padded area*: the grid's bounds grown by the padding
  its host puts around it. The host tells the pane its padding
  (`TerminalPane::set_scrollbar_padding`): the session view sets its insets
  (16/12, Review 12/12) as it renders, the Settings preview its 24pt inset
  once. The thumb is `WIDTH` 6pt wide, `INSET` 2pt from the area's right
  edge, and travels the area's full height less 2pt at each end. The text
  grid is untouched: same bounds, same rows and columns, no reflow. Panes
  without a host padding (the bench, the spike, the visual check) keep the
  old placement at the grid's edge.
- **Painted outside the element** (`element.rs`): the thumb is painted after
  the element's own content mask, so it overlays the padding (the parent's
  mask still clips it, as the preview's rounded box does). The 12pt hit
  strip along the edge is its own hitbox, inserted over the padding, so the
  pointer there reveals the scrollbar, a press grabs the thumb or jumps the
  track, the wheel scrolls, and the cursor is an arrow. Since every host's
  padding is at least the strip's 12pt, the strip never covers text: a press
  on the last text column selects text now (before, the strip covered about
  a column and a half of text).
- **Visibility** (`scrollbar.rs`, `Visibility`, a pure state machine with
  the clock passed in): shown while the terminal scrolls, for `LINGER` (1s)
  after the last scroll, while the pointer is over the edge strip, and while
  a drag holds the thumb; otherwise hidden. A change shows or hides with a
  fade on the app's ease (`motion::ease_out_expo`): in over `MENU_IN`
  (200ms), out over `DIALOG_IN` (400ms), from wherever a fade under way had
  got to (no jump when it reverses). Reduce Motion makes both instant, and
  lands a fade already under way at once.
- **What counts as a scroll** (`TerminalPane::scrollbar_frame`): each frame
  compares the history length and scroll offset with the last frame's. An
  offset change (the wheel, trackpad, Shift+PageUp/Down/Home/End, a thumb
  drag or track click, a find step, Jump to bottom, selection autoscroll) is
  a scroll, and so is history growing while scrolled back (new output while
  scrolled up). Output at the live bottom isn't. One place catches every
  path, so no scroll site had to learn about the scrollbar.
- **Timers and frames**: a fade asks for animation frames until it lands; a
  scroll's linger sets one timer for when it runs out (re-armed only when
  the deadline moves), whose wake draws the frame that starts the fade out.
  An idle pane runs nothing.
- **Leaving**: a pointer that leaves the window (often off the right edge,
  where the strip is) clears the hover (`MouseExitEvent`); letting go of a
  thumb drag recomputes it from where the pointer ended up.

### The setting: Settings ▸ Terminal ▸ Show scrollbar

John's addition after dispatch. A segmented control under the Option key
row, with a helper line ("Automatic follows Show scroll bars in System
Settings ▸ Appearance."):

```
Show scrollbar
┌───────────┬────────────────┬────────┐
│ Automatic │ When scrolling │ Always │
└───────────┴────────────────┴────────┘
Automatic follows Show scroll bars in System Settings ▸ Appearance.
```

- `desktop.terminal_scrollbar: auto | scrolling | always` in `config.yaml`
  (`shelbi_state::TerminalScrollbar`), next to the other terminal settings.
  `auto` is the default and isn't written (as `option_as_meta: false`
  isn't). It reads leniently: any case, macOS's own `automatic` and
  `when-scrolling` spellings; an unknown value is ignored (reads as `auto`)
  rather than failing the whole file.
- It rides the `TerminalSettings` global, so a pick applies to every open
  terminal at once (before the write) and an open Settings view follows a
  change made elsewhere.
- `scrollbar::overlay(setting, system)`: Automatic follows the system,
  When scrolling is always the overlay, Always never is.

### Following "Show scroll bars"

`platform/scrollers.rs` reads `NSScroller.preferredScrollerStyle` (which
resolves Automatic against the pointing devices attached: overlay with a
trackpad, legacy with a mouse; off the main thread it falls back to the
`AppleShowScrollBars` default). `scrollbar::SystemScrollbars` holds it as a
global that every pane observes. It is read at launch, again whenever a
window comes to the front (beside Reduce Motion and Reduce Transparency),
and live: an `NSPreferredScrollerStyleDidChangeNotification` observer on
the main queue sends on a channel that a gpui task drains to re-read it, so
changing the setting (or plugging in a mouse under Automatic) updates open
terminals without switching apps. Tests keep the default (overlay), so they
don't depend on the machine's setting.

`Cargo.toml` gains objc2 features only (`NSNotification`; `NSScroller` and
the `NSControl`, `NSResponder`, `NSView` it needs). `Cargo.lock` doesn't
change.

## Decisions

- **Padding passed in, not padding moved into the pane.** Moving the hosts'
  padding into the element would have shifted the find bar and Jump to
  bottom (positioned from the pane's root) and changed what a click in the
  padding does. Telling the pane its host's padding keeps every other pixel
  where the Figma frames put it.
- **The find bar and Jump to bottom keep their offsets**
  (`HIT_WIDTH + 8` from the grid's edge). They cleared the old in-grid
  scrollbar; they still clear the new one by more.
- **The hit strip is live while hidden.** Pointing at the edge is how an
  overlay scrollbar is found, so the strip reveals it and a press on it
  grabs or jumps, shown or not, as in AppKit.
- **Always shows without a fade**, as legacy scrollers do; a fade there
  would only delay the thumb when history first appears.
- **Track runs the area's full height** (less the 2pt inset), not just the
  grid's, so the thumb reads as the pane's scrollbar, as an
  `NSScrollView`'s does.
- **`--visual-figma`'s "Jump to bottom" state** scrolls up and saves the
  frame at once, so the thumb is captured at the start of its fade
  (hidden). The Figma frame is about the button; not changed here.

## Verification

From `desktop/`:

- `cargo build`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo clippy --all-targets --features visual-check -- -D warnings`:
  clean.
- `cargo test` (the full suite): 743 passed, 0 failed.
- `cargo run --features visual-check -- --visual-check <dir>`: 0 checks
  failed in dark and light, including "the scrollbar shows with history"
  and "dragging the scrollbar thumb to the top scrolls to the oldest line"
  (a detached pane with no host padding: the old placement, drag intact).

The root workspace, for the `shelbi-state` change:
`cargo clippy -p shelbi-state --all-targets -- -D warnings` (clean),
`cargo check --workspace --locked` (root `Cargo.lock` unchanged),
`cargo +1.88 check -p shelbi-state --locked` (the root MSRV), and
`cargo test -p shelbi-state --lib user_config` (17 passed).

Not checked by hand in a running app: the fade's feel, and the live
follow of System Settings' "Show scroll bars" (the notification path
needs AppKit's real run loop). The QA checklist in the PR covers both.

New tests:

- `elements::terminal::scrollbar::tests` (unit, no window):
  `the_rail_runs_down_the_areas_right_edge_not_the_grids` (thumb 2pt from
  the area's edge, travel, hit strip inside the padding for the 16pt and
  12pt hosts, zero padding unchanged), `the_setting_picks_overlay_or_always`,
  `an_overlay_is_hidden_at_rest_and_shows_while_scrolling` (fade in, linger,
  restart on a new scroll, fade out, the hide timer's delay),
  `hover_and_drag_hold_an_overlay`,
  `a_fade_out_reversed_midway_fades_back_in_from_where_it_was`,
  `always_shows_at_once_and_reduce_motion_makes_fades_instant`.
- `views::session_view::tests` (the real pane and element in a gpui
  window, with a test clock skew on the pane):
  `the_scrollbar_hugs_the_areas_right_edge_over_the_padding` (the grid's
  bounds unchanged, the thumb against the window's edge at both ends, the
  strip right of the text), `an_overlay_scrollbar_shows_while_scrolling_then_hides`
  (hidden at rest, output at the bottom isn't a scroll, output while
  scrolled back is, the hide timer hides it), `the_scrollbar_fades_in`,
  `the_edge_reveals_the_scrollbar_and_its_thumb_drags` (hover in the
  padding, drag from there to the oldest line, the drag holds it, release
  and leave hides it, a track click jumps),
  `the_setting_and_the_system_decide_whether_it_rests_shown`.
- `app::settings_tests::the_terminal_page_picks_when_the_scrollbar_shows`:
  the three segments, the saved choice shown, a pick applied live and
  saved (`auto` not written), a change elsewhere followed, the preview's
  padding.
- `views::settings::tests`: the value round-trips and isn't written at its
  default.
- `shelbi-state`: `terminal_scrollbar_round_trips_and_reads_leniently`.
