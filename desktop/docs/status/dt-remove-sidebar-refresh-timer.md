# dt-remove-sidebar-refresh-timer: no board refresh timer in the sidebar

## What it was

The desktop sidebar's footer drew the shared board freshness banner
(`SidebarModel::board_banner`, from `shelbi_state::BoardFreshness::banner`)
in the notes block above the Settings button. On a GitHub-backed board with
a warm index that banner is only the index's age: `board 12s`, `board 49s`,
`board 3m`, counting up and resetting each time the daemon republishes the
index. It was the only refresh or age indicator in the sidebar, so there was
nothing else it could have meant.

Before, with this repo's own project open (`shelbi status` read
`github: board 49s · graphql 1880 left` at the time): a small muted gray
line reading `board 49s` sat between the last workspace row and the
Settings button, in both themes, ticking up between refreshes. The
`--visual-figma` fixtures carry no banner, so the reference renders never
showed it.

## What changed

- **Only warnings show** (`views/sidebar.rs` `board_warning`). The banner is
  kept when the board needs a look: a stale index (`board 4m stale · quota
  resets 16:03`) or a cache read with no daemon (`board 4m cached · no
  daemon`). Both draw in the warning color, as before. A warm board's banner
  is dropped. The test is the same `stale` / `no daemon` text match the
  footer already used to color the line, now pinned against the real
  `BoardFreshness::banner` forms in a unit test.
- **The feed drops it too** (`refresh.rs` `sidebar_reader`). The window
  repaints only when a read's model differs from the last one
  (`apply_feed`), and the age text differed on every 5 s read while the
  index was under a minute old (then once a minute), so an
  idle window repainted (and re-derived its menus and palette entries) just
  to advance the timer. With the warm banner stripped at the reader, an
  unchanged board produces an identical model and no repaint. A stale
  warning still carries its age (minute granularity past 60 s), so it still
  changes the model while it is up; that is a real warning, not the timer.
- **Kept**: the orchestrator failure line, "Daemon unreachable · polling",
  the daemon/CLI version mismatch line, and the stale/cached board warning.
  The board itself refreshes exactly as before; only the indicator went.
  There was no dedicated ticker for the indicator: the age only advanced on
  the feed's regular reads, which still run for everything else.

## Not changed

- The shared model and its builder (`shelbi_app::view::SidebarModel`,
  `shelbi_orchestrator::sidebar_model`) are untouched, so the TUI's footer
  still shows `board 12s`. The filter is desktop-only.
- The Issues board header (`views/issues.rs`) has its own banner and is out
  of scope.
- `parity.md`'s `board_banner` row now says the desktop shows it only when
  stale or cached.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings` (with and
  without `--features visual-check`), `cargo test` in `desktop/`.
- New tests: `board_warning_drops_the_warm_age_and_keeps_the_warnings` and
  `the_footer_shows_a_stale_board_but_not_a_warm_one`.
- `--visual-figma` rendered at the base and with the change: all 94 frames
  are byte-identical and `scripts/figma-diff.sh` reports the same numbers
  for both (the fixtures carry no banner, so nothing in them moves).
