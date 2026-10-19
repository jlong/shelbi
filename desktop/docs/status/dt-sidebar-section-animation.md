# dt-sidebar-section-animation: sidebar sections fold open and closed

## What it was

A click on a sidebar section header (Ready for review, Queued for review,
Workspaces) or a machine group header toggled it, and the next frame's
`sidebar::layout` dropped or added its rows. Everything below jumped in one
frame, and the header's chevron swapped between its two orientations.

## What changed

- **A reusable fold** (`theme/fold.rs`). `Folds<K>` keeps only the motion:
  the owner keeps what is collapsed and calls `toggle(key, collapsed, now,
  reduce_motion)` when the user flips a body, then asks `openness(key,
  collapsed, now)` (0 closed to 1 open) each frame. A fold runs for `FOLD`
  (220ms) on Taskpage's `ease_out_expo`. A flip mid-fold starts from the
  openness it has reached, so nothing jumps, and runs for the share of
  `FOLD` it still has to cover (at least 40%). `Folds::body(key, openness,
  content)` draws a body: when open, the content as it is. Mid-fold, the
  content stays at its natural height inside a clip set to that share of
  the height, and fades with it. A canvas probe measures the content each
  frame it paints, so a fold starts from the height the body last had, and
  it follows rows that come and go mid-fold. `fold::chevron(openness)`
  turns a down-pointing chevron by up to a quarter turn. The Settings
  sidebar (`views/settings_nav.rs`, landed in #1813, sections not yet
  collapsible) can take the same pieces: a `Folds<settings_nav::Section>`
  in its owner, `body` around each section's pages, `chevron` on each
  header.
- **The sidebar draws what is still folding** (`views/sidebar.rs`). Each
  `Entry` now knows the section body it sits in (`section`) and, for an
  indented workspace, its machine group (`machine`). `sidebar::drawn` is
  `layout` plus the rows of any section or machine group that is still
  folding closed. For machine groups it lays out a copy of the model with
  those groups open, then restores each group row's `collapsed` and takes
  the chords from the real layout. `render` groups consecutive rows into
  their section body (and a machine body inside Workspaces) and wraps each
  in `Folds::body`. The headers' chevrons rotate by the same openness.
- **The keys follow what is collapsed, not what is drawn.** The window still
  walks `layout` (`sidebar_entries`), so Up and Down skip a section's rows
  from the frame it starts closing, and pick up an opening section's rows at
  once. In `render`, a row folding away gets no selectable ordinal, so it is
  never drawn as selected, and it has no click handler (an ordinal-less row
  would otherwise activate row 0). The ⌘1 to ⌘9
  numbering is `layout`'s throughout.
- **The window** (`app.rs`) owns `folds: SidebarFolds`. `toggle_section` and
  `toggle_machine` start a fold on the injected clock, instant under Reduce
  Motion (`self.reduce_motion`, the same setting `sync_reduce_motion` hands
  gpui). The render settles finished folds and asks for another frame while
  any still runs. `adopt_view_state` (launch, a project switch) and the
  switch reset clear the folds, so restored collapsed sections draw as they
  were left, with no animation. `toggle_machine` no longer writes
  `state.json` from a test window (`offline()`); before, a click in the
  test harness would have written it.
- **Untouched:** the sidebar container's show and hide
  (`dt-sidebar-toggle-animation`'s), the switcher, the nav rows, the footer.
  `shelbi-app` is not changed, so the TUI is unaffected.

## Behaviors checked

- The selection chip: a selected row below a folding section keeps its own
  inline chip and rides up with it (no slide is started). A selected row
  inside a closing section loses the selection at the click, as it did
  before (the selection is `layout`'s, which no longer has the row).
- Scroll: gpui clamps the rows' scroll offset each frame, and since the
  content shrinks gradually, a fold from the bottom of a scrolled sidebar
  eases the offset toward the top. It never goes out of range and never
  jumps.
- Live updates: a review arriving in or leaving a folding body is drawn
  inside it (the probe re-measures), and the fold carries on and settles.
  If the owner's state changes without a toggle (a refresh flipping a
  machine group), the fold for that body stops counting.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy
  --all-targets --features visual-check -- -D warnings`, `cargo test` (all
  in `desktop/`): clean, 549 tests passing after the rebase onto `79e0bed4`
  (#1813).
- New tests:
  - `theme::fold::tests`: closes on the curve and settles, opens from
    closed, a mid-fold flip turns around with no jump and a shorter run,
    Reduce Motion lands at once (mid-fold too), `clear` and an owner change
    end a fold, the chevron angle.
  - `views::sidebar::tests`: `rows_know_the_body_they_fold_with`,
    `a_closing_body_stays_drawn_until_its_fold_ends` (rows kept, headers
    say collapsed, chords from the collapsed layout, only closing rows
    flagged, settled equals `layout`).
  - `app::fold_tests` (the headless window with an injected clock): the
    body shrinks part-way at 50ms and the next section glides up by exactly
    the body's height; it opens from closed; a click mid-fold turns it around
    with no jump and is remembered open; Reduce Motion folds in the same
    frame; a restore mid-fold lands with no fold; Up and Down skip a closing
    section's rows, a click on a closing row does nothing, and ⌘1 and ⌘2
    keep their rows; the selection rides with its row; a row arriving or
    leaving mid-fold; a machine group folds too; the scroll stays in range
    through a fold from the bottom.
  - `app_sidebar_tests` runs its collapse tests under Reduce Motion (instant
    folds), as their assertions are about the collapsed state, not the
    motion.
- The real app: not checked. The screen was locked for the whole task
  (`CGSSessionScreenIsLocked=Yes`), so the motion was checked only through
  the tests and the offscreen frames.
- Offscreen frames (`--visual-figma`, new
  `state-sidebar-fold-{closing,opening}-{0,40,90,220,settled}`, both
  themes), checked by eye. Closing Queued for review: full height at 0ms;
  at 40ms the body is about a quarter of its height and faded, the chevron
  most of the way to pointing right, and Workspaces has moved most of the
  way up; at 90ms it is nearly closed; at 220ms it is the settled frame.
  Opening from collapsed: at 40ms two of the three rows show through the
  growing clip, so the probe had measured the body while it was clipped to
  zero.
- Regression: `--visual-figma` from `origin/main` (as of `21f2cc54` plus
  #1812) against this branch before the rebase: 76 of the 98 frames they
  share are byte-identical, including every sidebar, orchestrator,
  workspace, review, palette and settings frame. The 22 that differ are
  Issues board frames. Their sidebar region (x 0 to 280) is identical in
  every one; the board area differs because that base already had #1812's
  Issues toolbar and search, which this branch did not have yet.
